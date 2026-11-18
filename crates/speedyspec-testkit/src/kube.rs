//! An in-memory Kubernetes API server, for tests that must never reach a real cluster.
//!
//! It answers discovery, GET, DELETE and server-side apply for a handful of built-in kinds and
//! any custom kind a test defines. It is deliberately small: objects are stored whole, deletion
//! is immediate, and nothing reconciles. Tests play the controllers themselves with
//! [`FakeCluster::set_status`].

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value as Json, json};

use crate::server::{Received, Reply, Server};

const TOKEN: &str = "fake-cluster-token";

/// `(group/version or "v1", kind, namespace, name)`
type ObjectKey = (String, String, Option<String>, String);

#[derive(Clone)]
struct Kind {
    group: String,
    version: String,
    kind: String,
    plural: String,
    namespaced: bool,
}

impl Kind {
    fn api_version(&self) -> String {
        if self.group.is_empty() {
            self.version.clone()
        } else {
            format!("{}/{}", self.group, self.version)
        }
    }
}

#[derive(Default)]
struct State {
    kinds: Vec<Kind>,
    objects: BTreeMap<ObjectKey, Json>,
    /// `(method, kind, status, message)`: the next matching request fails this way.
    failures: Vec<(String, String, u16, String)>,
    /// `(object, manager, field)`: fields another manager owns.
    owned: Vec<(ObjectKey, String, String)>,
    requests: Vec<Received>,
    versions: u64,
}

/// The fake cluster; it stops when dropped.
pub struct FakeCluster {
    server: Server,
    state: Arc<Mutex<State>>,
}

impl FakeCluster {
    /// Starts a cluster holding the namespace `default` and nothing else.
    pub fn start() -> FakeCluster {
        let mut state = State::default();
        for (group, version, kind, plural, namespaced) in [
            ("", "v1", "Namespace", "namespaces", false),
            ("", "v1", "ConfigMap", "configmaps", true),
            ("", "v1", "Secret", "secrets", true),
            ("", "v1", "Service", "services", true),
            ("", "v1", "Pod", "pods", true),
            (
                "",
                "v1",
                "PersistentVolumeClaim",
                "persistentvolumeclaims",
                true,
            ),
            ("apps", "v1", "Deployment", "deployments", true),
            ("apps", "v1", "StatefulSet", "statefulsets", true),
            ("apps", "v1", "DaemonSet", "daemonsets", true),
            ("batch", "v1", "Job", "jobs", true),
        ] {
            state.kinds.push(Kind {
                group: group.to_owned(),
                version: version.to_owned(),
                kind: kind.to_owned(),
                plural: plural.to_owned(),
                namespaced,
            });
        }
        state.objects.insert(
            (
                "v1".to_owned(),
                "Namespace".to_owned(),
                None,
                "default".to_owned(),
            ),
            json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "default"}}),
        );
        let state = Arc::new(Mutex::new(state));
        let shared = Arc::clone(&state);
        let server = Server::handling(move |request| {
            let mut state = shared.lock().expect("fake cluster state");
            state.requests.push(request.clone());
            respond(&mut state, request)
        });
        FakeCluster { server, state }
    }

    /// `http://127.0.0.1:<port>`
    pub fn url(&self) -> String {
        self.server.url()
    }

    /// The bearer token every request must carry.
    pub fn token(&self) -> &'static str {
        TOKEN
    }

    /// The stored object, if it exists.
    pub fn object(
        &self,
        api_version: &str,
        kind: &str,
        namespace: Option<&str>,
        name: &str,
    ) -> Option<Json> {
        self.lock()
            .objects
            .get(&key(api_version, kind, namespace, name))
            .cloned()
    }

    /// Stores `object` as it is, as another actor in the cluster would (no labels added, no
    /// manager recorded).
    pub fn put(&self, object: Json) {
        let api_version = object["apiVersion"]
            .as_str()
            .expect("apiVersion")
            .to_owned();
        let kind = object["kind"].as_str().expect("kind").to_owned();
        let namespace = object["metadata"]["namespace"].as_str().map(str::to_owned);
        let name = object["metadata"]["name"]
            .as_str()
            .expect("name")
            .to_owned();
        self.lock()
            .objects
            .insert((api_version, kind, namespace, name), object);
    }

    /// Sets the object's `status`, as its controller would.
    pub fn set_status(
        &self,
        api_version: &str,
        kind: &str,
        namespace: Option<&str>,
        name: &str,
        status: Json,
    ) {
        let mut state = self.lock();
        let object = state
            .objects
            .get_mut(&key(api_version, kind, namespace, name))
            .expect("set_status on an object that exists");
        object["status"] = status;
    }

    /// Every stored object of `kind`, in key order.
    pub fn objects_of(&self, kind: &str) -> Vec<Json> {
        self.lock()
            .objects
            .iter()
            .filter(|(key, _)| key.1 == kind)
            .map(|(_, object)| object.clone())
            .collect()
    }

    /// Makes the cluster know a custom kind, as applying its definition eventually does.
    pub fn define(&self, group: &str, version: &str, kind: &str, plural: &str, namespaced: bool) {
        self.lock().kinds.push(Kind {
            group: group.to_owned(),
            version: version.to_owned(),
            kind: kind.to_owned(),
            plural: plural.to_owned(),
            namespaced,
        });
    }

    /// Makes the next `method` request for an object of `kind` fail with this status and message.
    pub fn fail_next(&self, method: &str, kind: &str, status: u16, message: &str) {
        self.lock().failures.push((
            method.to_owned(),
            kind.to_owned(),
            status,
            message.to_owned(),
        ));
    }

    /// Records that `manager` owns `field` of the object: an apply by anyone else is a conflict
    /// unless it is forced.
    pub fn own_field(
        &self,
        api_version: &str,
        kind: &str,
        namespace: Option<&str>,
        name: &str,
        manager: &str,
        field: &str,
    ) {
        self.lock().owned.push((
            key(api_version, kind, namespace, name),
            manager.to_owned(),
            field.to_owned(),
        ));
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<Received> {
        self.lock().requests.clone()
    }

    /// The requests that change something (`PATCH`, `DELETE`), as `"METHOD path"`.
    pub fn writes(&self) -> Vec<String> {
        self.requests()
            .iter()
            .filter(|request| request.method == "PATCH" || request.method == "DELETE")
            .map(|request| format!("{} {}", request.method, request.path))
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("fake cluster state")
    }
}

fn key(api_version: &str, kind: &str, namespace: Option<&str>, name: &str) -> ObjectKey {
    (
        api_version.to_owned(),
        kind.to_owned(),
        namespace.map(str::to_owned),
        name.to_owned(),
    )
}

fn status(code: u16, reason: &str, message: &str) -> Reply {
    status_with(code, reason, message, json!({}))
}

fn status_with(code: u16, reason: &str, message: &str, details: Json) -> Reply {
    let body = json!({
        "kind": "Status",
        "apiVersion": "v1",
        "metadata": {},
        "status": "Failure",
        "message": message,
        "reason": reason,
        "details": details,
        "code": code,
    });
    Reply::json(code, body.to_string())
}

fn respond(state: &mut State, request: &Received) -> Reply {
    let authorized = request
        .headers
        .get("authorization")
        .is_some_and(|value| value == &format!("Bearer {TOKEN}"));
    if !authorized {
        return status(401, "Unauthorized", "Unauthorized");
    }
    let segments: Vec<&str> = request.path.trim_matches('/').split('/').collect();
    match segments.as_slice() {
        ["version"] => Reply::json(
            200,
            json!({"major": "1", "minor": "34", "gitVersion": "v1.34.1", "gitCommit": "fake",
                   "gitTreeState": "clean", "buildDate": "2026-01-01T00:00:00Z",
                   "goVersion": "go1.25", "compiler": "gc", "platform": "linux/amd64"})
            .to_string(),
        ),
        ["api"] => Reply::json(
            200,
            json!({"kind": "APIVersions", "versions": ["v1"],
                   "serverAddressByClientCIDRs": []})
            .to_string(),
        ),
        ["apis"] => {
            let mut groups: Vec<(String, String)> = Vec::new();
            for kind in state.kinds.iter().filter(|kind| !kind.group.is_empty()) {
                let entry = (kind.group.clone(), kind.version.clone());
                if !groups.contains(&entry) {
                    groups.push(entry);
                }
            }
            let groups: Vec<Json> = groups
                .iter()
                .map(|(group, version)| {
                    let group_version = format!("{group}/{version}");
                    json!({"name": group,
                           "versions": [{"groupVersion": group_version, "version": version}],
                           "preferredVersion": {"groupVersion": group_version, "version": version}})
                })
                .collect();
            Reply::json(
                200,
                json!({"kind": "APIGroupList", "apiVersion": "v1", "groups": groups}).to_string(),
            )
        }
        ["api", "v1"] => resource_list(state, "", "v1"),
        ["apis", group, version] => resource_list(state, group, version),
        ["api", "v1", rest @ ..] => object_request(state, request, "", "v1", rest),
        ["apis", group, version, rest @ ..] => object_request(state, request, group, version, rest),
        _ => status(
            404,
            "NotFound",
            "the server could not find the requested resource",
        ),
    }
}

fn resource_list(state: &State, group: &str, version: &str) -> Reply {
    let resources: Vec<Json> = state
        .kinds
        .iter()
        .filter(|kind| kind.group == group && kind.version == version)
        .map(|kind| {
            json!({"name": kind.plural, "singularName": kind.kind.to_lowercase(),
                   "namespaced": kind.namespaced, "kind": kind.kind,
                   "verbs": ["create", "delete", "get", "list", "patch", "update", "watch"]})
        })
        .collect();
    if resources.is_empty() {
        return status(
            404,
            "NotFound",
            "the server could not find the requested resource",
        );
    }
    let group_version = if group.is_empty() {
        version.to_owned()
    } else {
        format!("{group}/{version}")
    };
    Reply::json(
        200,
        json!({"kind": "APIResourceList", "apiVersion": "v1", "groupVersion": group_version,
               "resources": resources})
        .to_string(),
    )
}

/// `rest` is `<plural>/<name>` or `namespaces/<ns>/<plural>/<name>`.
fn object_request(
    state: &mut State,
    request: &Received,
    group: &str,
    version: &str,
    rest: &[&str],
) -> Reply {
    let (namespace, plural, name) = match rest {
        ["namespaces", namespace, plural, name] => (Some(*namespace), *plural, *name),
        [plural, name] => (None, *plural, *name),
        _ => {
            return status(
                404,
                "NotFound",
                "the server could not find the requested resource",
            );
        }
    };
    let Some(kind) = state
        .kinds
        .iter()
        .find(|kind| kind.group == group && kind.version == version && kind.plural == plural)
        .cloned()
    else {
        return status(
            404,
            "NotFound",
            "the server could not find the requested resource",
        );
    };
    if kind.namespaced != namespace.is_some() {
        return status(
            404,
            "NotFound",
            "the server could not find the requested resource",
        );
    }
    if let Some(at) = state
        .failures
        .iter()
        .position(|(method, failing, _, _)| *method == request.method && *failing == kind.kind)
    {
        let (_, _, code, message) = state.failures.remove(at);
        return status(code, "Invalid", &message);
    }
    let object_key = key(&kind.api_version(), &kind.kind, namespace, name);
    let missing = || {
        status(
            404,
            "NotFound",
            &format!("{} \"{name}\" not found", kind.plural),
        )
    };
    match request.method.as_str() {
        "GET" => match state.objects.get(&object_key) {
            Some(object) => Reply::json(200, object.to_string()),
            None => missing(),
        },
        "DELETE" => {
            if state.objects.remove(&object_key).is_none() {
                return missing();
            }
            if kind.kind == "Namespace" {
                state
                    .objects
                    .retain(|key, _| key.2.as_deref() != Some(name));
            }
            Reply::json(
                200,
                json!({"kind": "Status", "apiVersion": "v1", "metadata": {}, "status": "Success",
                       "code": 200})
                .to_string(),
            )
        }
        "PATCH" => apply(state, request, &kind, object_key, namespace, name),
        _ => status(405, "MethodNotAllowed", "method not allowed"),
    }
}

fn apply(
    state: &mut State,
    request: &Received,
    kind: &Kind,
    object_key: ObjectKey,
    namespace: Option<&str>,
    name: &str,
) -> Reply {
    let content_type = request.headers.get("content-type").map(String::as_str);
    if content_type != Some("application/apply-patch+yaml") {
        return status(
            415,
            "UnsupportedMediaType",
            "only server-side apply is supported",
        );
    }
    let Some(manager) = request.parameter("fieldManager").filter(|m| !m.is_empty()) else {
        return status(
            400,
            "BadRequest",
            "fieldManager is required for apply patch",
        );
    };
    let Ok(mut object) = serde_json::from_slice::<Json>(&request.body) else {
        return status(400, "BadRequest", "the body is not valid JSON");
    };
    if let Some(namespace) = namespace {
        let exists = state.objects.contains_key(&(
            "v1".to_owned(),
            "Namespace".to_owned(),
            None,
            namespace.to_owned(),
        ));
        if !exists {
            return status(
                404,
                "NotFound",
                &format!("namespaces \"{namespace}\" not found"),
            );
        }
    }
    let forced = request.parameter("force") == Some("true");
    let conflicts: Vec<(String, String)> = state
        .owned
        .iter()
        .filter(|(owned, other, _)| *owned == object_key && other != manager)
        .map(|(_, other, field)| (other.clone(), field.clone()))
        .collect();
    if !conflicts.is_empty() {
        if !forced {
            let causes: Vec<Json> = conflicts
                .iter()
                .map(|(other, field)| {
                    json!({"reason": "FieldManagerConflict",
                           "message": format!("conflict with \"{other}\" using {}", kind.api_version()),
                           "field": field})
                })
                .collect();
            return status_with(
                409,
                "Conflict",
                &format!("Apply failed with {} conflict", conflicts.len()),
                json!({"causes": causes}),
            );
        }
        state.owned.retain(|(owned, _, _)| *owned != object_key);
    }
    state.versions += 1;
    let previous = state.objects.get(&object_key);
    let generation = match previous {
        Some(previous) if previous["spec"] == object["spec"] => {
            previous["metadata"]["generation"].as_u64().unwrap_or(1)
        }
        Some(previous) => previous["metadata"]["generation"].as_u64().unwrap_or(1) + 1,
        None => 1,
    };
    if let Some(previous) = previous
        && !previous["status"].is_null()
    {
        object["status"] = previous["status"].clone();
    }
    object["metadata"]["name"] = json!(name);
    if let Some(namespace) = namespace {
        object["metadata"]["namespace"] = json!(namespace);
    }
    object["metadata"]["generation"] = json!(generation);
    object["metadata"]["resourceVersion"] = json!(state.versions.to_string());
    object["metadata"]["uid"] = json!(format!("uid-{}-{name}", kind.plural));
    let code = if previous.is_some() { 200 } else { 201 };
    state.objects.insert(object_key, object.clone());
    Reply::json(code, object.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    /// A bare HTTP/1.1 exchange, so the fake is tested without any client library.
    fn send(
        cluster: &FakeCluster,
        method: &str,
        target: &str,
        token: Option<&str>,
        body: &str,
    ) -> (u16, Json) {
        let address = cluster.url().trim_start_matches("http://").to_owned();
        let mut stream = TcpStream::connect(&address).unwrap();
        let authorization =
            token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
        let request = format!(
            "{method} {target} HTTP/1.1\r\nHost: {address}\r\n{authorization}\
             Content-Type: application/apply-patch+yaml\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        let code = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (code, serde_json::from_str(body).unwrap())
    }

    const CONFIG_MAP: &str = "/api/v1/namespaces/default/configmaps/settings";

    #[test]
    fn applies_reads_and_deletes_an_object() {
        let cluster = FakeCluster::start();
        let token = Some(cluster.token());
        let body = r#"{"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"settings"},"data":{"a":"1"}}"#;
        let target = format!("{CONFIG_MAP}?fieldManager=test");

        assert_eq!(send(&cluster, "PATCH", &target, token, body).0, 201);
        assert_eq!(send(&cluster, "PATCH", &target, token, body).0, 200);
        let (code, stored) = send(&cluster, "GET", CONFIG_MAP, token, "");
        assert_eq!(code, 200);
        assert_eq!(stored["data"]["a"], "1");
        assert_eq!(stored["metadata"]["namespace"], "default");
        assert!(
            cluster
                .object("v1", "ConfigMap", Some("default"), "settings")
                .is_some()
        );

        assert_eq!(send(&cluster, "DELETE", CONFIG_MAP, token, "").0, 200);
        assert_eq!(send(&cluster, "GET", CONFIG_MAP, token, "").0, 404);
        assert_eq!(cluster.writes().len(), 3);
    }

    #[test]
    fn refuses_requests_without_the_token() {
        let cluster = FakeCluster::start();
        assert_eq!(send(&cluster, "GET", "/version", None, "").0, 401);
        assert_eq!(send(&cluster, "GET", "/version", Some("wrong"), "").0, 401);
        assert_eq!(
            send(&cluster, "GET", "/version", Some(cluster.token()), "").0,
            200
        );
    }

    #[test]
    fn apply_needs_a_field_manager_and_an_existing_namespace() {
        let cluster = FakeCluster::start();
        let token = Some(cluster.token());
        let body = r#"{"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"settings"}}"#;
        assert_eq!(send(&cluster, "PATCH", CONFIG_MAP, token, body).0, 400);
        let elsewhere = "/api/v1/namespaces/nope/configmaps/settings?fieldManager=test";
        assert_eq!(send(&cluster, "PATCH", elsewhere, token, body).0, 404);
    }

    #[test]
    fn another_managers_field_is_a_conflict_until_forced() {
        let cluster = FakeCluster::start();
        let token = Some(cluster.token());
        let body = r#"{"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"settings"}}"#;
        let target = format!("{CONFIG_MAP}?fieldManager=test");
        send(&cluster, "PATCH", &target, token, body);
        cluster.own_field(
            "v1",
            "ConfigMap",
            Some("default"),
            "settings",
            "kubectl-edit",
            ".data.a",
        );

        let (code, conflict) = send(&cluster, "PATCH", &target, token, body);
        assert_eq!(code, 409);
        assert_eq!(conflict["details"]["causes"][0]["field"], ".data.a");
        assert!(
            conflict["details"]["causes"][0]["message"]
                .as_str()
                .unwrap()
                .contains("kubectl-edit")
        );

        let forced = format!("{target}&force=true");
        assert_eq!(send(&cluster, "PATCH", &forced, token, body).0, 200);
        assert_eq!(
            send(&cluster, "PATCH", &target, token, body).0,
            200,
            "ownership was taken"
        );
    }

    #[test]
    fn generation_moves_only_when_the_spec_changes() {
        let cluster = FakeCluster::start();
        let token = Some(cluster.token());
        let target = "/apis/apps/v1/namespaces/default/deployments/web?fieldManager=test";
        let one = r#"{"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"web"},"spec":{"replicas":1}}"#;
        let two = r#"{"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"web"},"spec":{"replicas":2}}"#;
        assert_eq!(
            send(&cluster, "PATCH", target, token, one).1["metadata"]["generation"],
            1
        );
        assert_eq!(
            send(&cluster, "PATCH", target, token, one).1["metadata"]["generation"],
            1
        );
        assert_eq!(
            send(&cluster, "PATCH", target, token, two).1["metadata"]["generation"],
            2
        );
    }

    #[test]
    fn deleting_a_namespace_empties_it_and_discovery_lists_custom_kinds() {
        let cluster = FakeCluster::start();
        let token = Some(cluster.token());
        let namespace = r#"{"apiVersion":"v1","kind":"Namespace","metadata":{"name":"shop"}}"#;
        send(
            &cluster,
            "PATCH",
            "/api/v1/namespaces/shop?fieldManager=test",
            token,
            namespace,
        );
        let body = r#"{"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"settings"}}"#;
        send(
            &cluster,
            "PATCH",
            "/api/v1/namespaces/shop/configmaps/settings?fieldManager=test",
            token,
            body,
        );
        send(&cluster, "DELETE", "/api/v1/namespaces/shop", token, "");
        assert!(
            cluster
                .object("v1", "ConfigMap", Some("shop"), "settings")
                .is_none()
        );

        assert_eq!(
            send(&cluster, "GET", "/apis/example.test/v1", token, "").0,
            404
        );
        cluster.define("example.test", "v1", "Widget", "widgets", true);
        let (code, list) = send(&cluster, "GET", "/apis/example.test/v1", token, "");
        assert_eq!(code, 200);
        assert_eq!(list["resources"][0]["kind"], "Widget");
    }
}
