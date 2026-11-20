//! The calls made to the cluster. Every one blocks. Errors name the resource and carry the
//! API's message, never a request or response body.

use std::time::{Duration, Instant};

use kube::api::{
    Api, ApiResource, DeleteParams, DynamicObject, GroupVersionKind, Patch, PatchParams,
};
use kube::discovery::{self, Scope};
use serde_json::{Value as Json, json};

use crate::connect::Connection;
use crate::manifest::{InstallIdentity, MANAGER, ResourceId, identify, labels};

/// Why a call to the cluster failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClusterError {
    NotFound,
    Unauthorized(String),
    /// Fields of `resource` are owned by other managers: `(field, manager)`.
    Conflict {
        resource: String,
        details: Vec<(String, String)>,
    },
    /// The cluster has no such kind (given as `<apiVersion> <kind>`).
    UnknownKind(String),
    Other(String),
}

impl ClusterError {
    /// One or more lines for the user.
    pub fn message(&self, resource: &str) -> String {
        match self {
            Self::NotFound => format!("{resource}: not found"),
            Self::Unauthorized(message) | Self::Other(message) => format!("{resource}: {message}"),
            Self::Conflict { resource, details } => details
                .iter()
                .map(|(field, manager)| {
                    format!(
                        "{resource}: field {field} is owned by {manager}; use \
                         --force-conflicts to take it over"
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Self::UnknownKind(kind) => format!("the cluster has no kind {kind}"),
        }
    }
}

fn convert(error: kube::Error, resource: &str) -> ClusterError {
    match error {
        kube::Error::Api(status) => match status.code {
            404 => ClusterError::NotFound,
            401 => ClusterError::Unauthorized(
                "unauthorized: the cluster rejected the credentials".to_owned(),
            ),
            403 => ClusterError::Unauthorized(format!("forbidden: {}", status.message)),
            409 => {
                let details: Vec<(String, String)> = status
                    .details
                    .iter()
                    .flat_map(|details| details.causes.iter())
                    .filter(|cause| cause.reason == "FieldManagerConflict")
                    .map(|cause| {
                        // `conflict with "<manager>" using <apiVersion>`
                        let manager = cause
                            .message
                            .split('"')
                            .nth(1)
                            .unwrap_or("another manager")
                            .to_owned();
                        (cause.field.clone(), manager)
                    })
                    .collect();
                if details.is_empty() {
                    ClusterError::Other(status.message.clone())
                } else {
                    ClusterError::Conflict {
                        resource: resource.to_owned(),
                        details,
                    }
                }
            }
            _ => ClusterError::Other(status.message.clone()),
        },
        other => ClusterError::Other(format!("could not reach the cluster: {other}")),
    }
}

fn group_version(api_version: &str) -> (&str, &str) {
    api_version.split_once('/').unwrap_or(("", api_version))
}

impl Connection {
    /// Runs one request, giving it up when the cluster does not answer in time: a connection
    /// that goes silent must not hold the command for ever.
    fn call<T>(
        &self,
        request: impl Future<Output = Result<T, kube::Error>>,
    ) -> Result<Result<T, kube::Error>, ClusterError> {
        self.runtime
            .block_on(async { tokio::time::timeout(self.request_wait, request).await })
            .map_err(|_| {
                ClusterError::Other(format!(
                    "no answer from the cluster within {}s",
                    self.request_wait.as_secs_f32().ceil()
                ))
            })
    }

    /// What the cluster says about a kind: how to address it, and whether it is namespaced.
    /// A kind the cluster does not know is asked for again for a while before giving up.
    fn kind(&self, api_version: &str, kind: &str) -> Result<(ApiResource, bool), ClusterError> {
        let key = (api_version.to_owned(), kind.to_owned());
        if let Some(known) = self.kinds.borrow().get(&key) {
            return Ok(known.clone());
        }
        let (group, version) = group_version(api_version);
        let gvk = GroupVersionKind::gvk(group, version, kind);
        let started = Instant::now();
        loop {
            match self.call(discovery::pinned_kind(&self.client, &gvk))? {
                Ok((resource, capabilities)) => {
                    let known = (resource, capabilities.scope == Scope::Namespaced);
                    self.kinds.borrow_mut().insert(key, known.clone());
                    return Ok(known);
                }
                // An unknown group is a 404; an unknown kind in a known group is a discovery
                // error. Either may be a definition that has not settled yet. Anything else
                // the cluster says is a failure, never "no such kind".
                Err(kube::Error::Api(status)) if status.code != 404 => {
                    return Err(convert(kube::Error::Api(status), kind));
                }
                Err(kube::Error::Api(_) | kube::Error::Discovery(_)) => {
                    if started.elapsed() >= self.discovery_wait {
                        return Err(ClusterError::UnknownKind(format!("{api_version} {kind}")));
                    }
                    std::thread::sleep(Duration::from_millis(200).min(self.discovery_wait));
                }
                Err(other) => return Err(convert(other, kind)),
            }
        }
    }

    /// Whether a kind is namespaced.
    pub fn scope(&self, api_version: &str, kind: &str) -> Result<bool, ClusterError> {
        self.kind(api_version, kind)
            .map(|(_, namespaced)| namespaced)
    }

    fn api(&self, id: &ResourceId) -> Result<Api<DynamicObject>, ClusterError> {
        let (resource, namespaced) = self.kind(&id.api_version, &id.kind)?;
        Ok(match (&id.namespace, namespaced) {
            (Some(namespace), true) => {
                Api::namespaced_with(self.client.clone(), namespace, &resource)
            }
            _ => Api::all_with(self.client.clone(), &resource),
        })
    }

    /// The objects `id` names the kind and namespace of (its name is ignored) whose labels match `selector`.
    pub(crate) fn list_of(
        &self,
        id: &ResourceId,
        selector: &str,
    ) -> Result<Vec<Json>, ClusterError> {
        let api = self.api(id)?;
        let params = kube::api::ListParams::default().labels(selector);
        let list = self
            .call(api.list(&params))?
            .map_err(|e| convert(e, &id.kind))?;
        Ok(list
            .items
            .into_iter()
            .map(|item| serde_json::to_value(item).unwrap_or(Json::Null))
            .collect())
    }

    /// The object as the cluster has it.
    pub fn get(&self, id: &ResourceId) -> Result<Json, ClusterError> {
        let api = self.api(id)?;
        let object = self
            .call(api.get(&id.name))?
            .map_err(|e| convert(e, &id.display()))?;
        serde_json::to_value(object).map_err(|e| ClusterError::Other(e.to_string()))
    }

    /// Server-side applies a complete manifest. `force` takes over fields other managers own.
    pub fn apply(&self, manifest: &Json, force: bool) -> Result<(), ClusterError> {
        let id = identify(manifest);
        let api = self.api(&id)?;
        let mut parameters = PatchParams::apply(MANAGER);
        if force {
            parameters = parameters.force();
        }
        self.call(api.patch(&id.name, &parameters, &Patch::Apply(manifest)))?
            .map(drop)
            .map_err(|e| convert(e, &id.display()))
    }

    /// Deletes the object; `NotFound` if it is not there.
    pub fn delete(&self, id: &ResourceId) -> Result<(), ClusterError> {
        let api = self.api(id)?;
        self.call(api.delete(&id.name, &DeleteParams::default()))?
            .map(drop)
            .map_err(|e| convert(e, &id.display()))
    }

    pub fn namespace_exists(&self, name: &str) -> Result<bool, ClusterError> {
        match self.get(&namespace_id(name)) {
            Ok(_) => Ok(true),
            Err(ClusterError::NotFound) => Ok(false),
            Err(other) => Err(other),
        }
    }

    /// Creates a namespace, labelled as this install's.
    pub fn create_namespace(
        &self,
        name: &str,
        identity: &InstallIdentity<'_>,
    ) -> Result<(), ClusterError> {
        self.apply(
            &json!({
                "apiVersion": "v1",
                "kind": "Namespace",
                "metadata": {"name": name, "labels": labels(identity)},
            }),
            false,
        )
    }
}

pub(crate) fn namespace_id(name: &str) -> ResourceId {
    ResourceId {
        api_version: "v1".to_owned(),
        kind: "Namespace".to_owned(),
        namespace: None,
        name: name.to_owned(),
    }
}
