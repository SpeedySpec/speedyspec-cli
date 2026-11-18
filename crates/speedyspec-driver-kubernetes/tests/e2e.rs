//! End to end against a real, throwaway cluster: the k3s that `scripts/k3s.sh up` starts.
//!
//! Every test is ignored by default and takes its kubeconfig from `SPEEDYSPEC_E2E_KUBECONFIG`
//! only. It fails, rather than fall back to the user's own cluster, when that is unset.

mod support;

use std::time::Duration;

use kube::api::{Api, DynamicObject, Patch, PatchParams};
use kube::discovery::ApiResource;
use serde_json::json;
use speedyspec_driver_kubernetes::{
    ConnectFlags, Connection, InstallIdentity, ResourceId, Status, apply_install, connect,
    find_install, remove_install, wait_and_finish,
};
use support::{Steps, answer, options, plugins, settings, web};

const TIMEOUT: Duration = Duration::from_secs(180);
const INTERVAL: Duration = Duration::from_secs(1);

/// The kubeconfig of the throwaway cluster. Never the user's.
fn kubeconfig() -> String {
    std::env::var("SPEEDYSPEC_E2E_KUBECONFIG").expect(
        "set SPEEDYSPEC_E2E_KUBECONFIG to the kubeconfig `scripts/k3s.sh up` wrote; this test \
         never uses your own cluster",
    )
}

fn connection() -> Connection {
    // SAFETY: the tests in this file run one at a time (`--test-threads=1`) and all set the
    // same value.
    unsafe { std::env::set_var("KUBECONFIG", kubeconfig()) };
    let flags = ConnectFlags {
        server: None,
        token_file: None,
        ca_cert: None,
        insecure_skip_tls_verify: false,
        context: None,
    };
    connect(&flags, None, &mut std::io::empty()).unwrap_or_else(|_| panic!("could not connect"))
}

fn service() -> serde_json::Value {
    json!({"apiVersion": "v1", "kind": "Service", "metadata": {"name": "web"},
           "spec": {"selector": {"app": "web"}, "ports": [{"port": 80}]}})
}

/// A kind of its own, and one of them: the cluster serves the kind only after the definition
/// has been applied.
fn widget_definition() -> serde_json::Value {
    json!({"apiVersion": "apiextensions.k8s.io/v1", "kind": "CustomResourceDefinition",
           "metadata": {"name": "widgets.e2e.speedyspec.test"},
           "spec": {"group": "e2e.speedyspec.test", "scope": "Namespaced",
                    "names": {"kind": "Widget", "plural": "widgets", "singular": "widget"},
                    "versions": [{"name": "v1", "served": true, "storage": true,
                                  "schema": {"openAPIV3Schema": {"type": "object",
                                             "x-kubernetes-preserve-unknown-fields": true}}}]}})
}

fn widget() -> serde_json::Value {
    json!({"apiVersion": "e2e.speedyspec.test/v1", "kind": "Widget", "metadata": {"name": "w"},
           "spec": {"size": 1}})
}

fn namespace(name: &str) -> ResourceId {
    ResourceId {
        api_version: "v1".to_owned(),
        kind: "Namespace".to_owned(),
        namespace: None,
        name: name.to_owned(),
    }
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up"]
fn installs_reinstalls_and_uninstalls_on_a_real_cluster() {
    let connection = connection();
    let identity = InstallIdentity {
        app: "shop",
        name: "e2e",
        namespace: "speedyspec-e2e",
    };
    let plugins = plugins();

    let applied = apply_install(
        &connection,
        &answer(vec![
            settings(),
            web(),
            service(),
            widget_definition(),
            widget(),
        ]),
        &options(identity, &plugins),
        &mut Steps::default(),
    )
    .unwrap();
    wait_and_finish(
        &connection,
        &applied,
        None,
        &identity,
        TIMEOUT,
        INTERVAL,
        &mut Steps::default(),
    )
    .unwrap();
    let removal = find_install(&connection, "e2e", "speedyspec-e2e").unwrap();
    assert_eq!(removal.record.status, Status::Installed);
    assert_eq!(removal.record.created_namespaces, vec!["speedyspec-e2e"]);

    // Again, without the Service: it is left running and stays in the record.
    let applied = apply_install(
        &connection,
        &answer(vec![settings(), web(), widget_definition(), widget()]),
        &options(identity, &plugins),
        &mut Steps::default(),
    )
    .unwrap();
    let retained: Vec<String> = applied.retained.iter().map(ResourceId::display).collect();
    assert_eq!(retained, vec!["speedyspec-e2e/Service/web"]);
    wait_and_finish(
        &connection,
        &applied,
        None,
        &identity,
        TIMEOUT,
        INTERVAL,
        &mut Steps::default(),
    )
    .unwrap();

    let removal = find_install(&connection, "e2e", "speedyspec-e2e").unwrap();
    let removed = remove_install(
        &connection,
        &removal,
        TIMEOUT,
        INTERVAL,
        &mut Steps::default(),
    )
    .unwrap();
    assert_eq!(removed.deleted.len(), 5, "{removed:?}");
    assert_eq!(removed.namespaces, vec!["speedyspec-e2e"]);
    assert!(
        connection.get(&namespace("speedyspec-e2e")).is_err(),
        "the namespace is gone"
    );
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up"]
fn conflict_with_another_manager_on_a_real_cluster() {
    let connection = connection();
    let identity = InstallIdentity {
        app: "shop",
        name: "conflict",
        namespace: "speedyspec-e2e-conflict",
    };
    let plugins = plugins();
    apply_install(
        &connection,
        &answer(vec![web()]),
        &options(identity, &plugins),
        &mut Steps::default(),
    )
    .unwrap();

    // Another manager takes the replica count, as `kubectl scale` would.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let client = kube::Client::try_default().await.unwrap();
        let resource = ApiResource::from_gvk(&kube::api::GroupVersionKind::gvk("apps", "v1", "Deployment"));
        let api: Api<DynamicObject> = Api::namespaced_with(client, "speedyspec-e2e-conflict", &resource);
        let patch = json!({"apiVersion": "apps/v1", "kind": "Deployment", "metadata": {"name": "web"},
                           "spec": {"replicas": 2}});
        api.patch("web", &PatchParams::apply("someone-else").force(), &Patch::Apply(&patch))
            .await
            .unwrap();
    });

    let error = apply_install(
        &connection,
        &answer(vec![web()]),
        &options(identity, &plugins),
        &mut Steps::default(),
    )
    .unwrap_err();
    assert!(
        error.contains(".spec.replicas") && error.contains("someone-else"),
        "{error}"
    );

    let mut forced = options(identity, &plugins);
    forced.force_conflicts = true;
    apply_install(
        &connection,
        &answer(vec![web()]),
        &forced,
        &mut Steps::default(),
    )
    .unwrap();

    let removal = find_install(&connection, "conflict", "speedyspec-e2e-conflict").unwrap();
    remove_install(
        &connection,
        &removal,
        TIMEOUT,
        INTERVAL,
        &mut Steps::default(),
    )
    .unwrap();
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up"]
fn a_namespace_the_cluster_already_has_is_not_taken_over() {
    let connection = connection();
    let identity = InstallIdentity {
        app: "shop",
        name: "takeover",
        namespace: "default",
    };
    let plugins = plugins();
    let existing =
        json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "default"}});
    let error = apply_install(
        &connection,
        &answer(vec![existing, settings()]),
        &options(identity, &plugins),
        &mut Steps::default(),
    )
    .unwrap_err();
    assert!(
        error.contains("Namespace/default already exists"),
        "{error}"
    );
    let namespace = connection.get(&namespace("default")).unwrap();
    assert!(
        namespace["metadata"]["labels"]["speedyspec.com/install"].is_null(),
        "the namespace was not labelled"
    );
    assert!(find_install(&connection, "takeover", "default").is_err());
}
