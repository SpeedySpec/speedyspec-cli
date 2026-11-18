//! Shared by the tests that drive the driver against the fake cluster.
#![allow(dead_code)]

use std::sync::Once;
use std::time::Duration;

use serde_json::{Value as Json, json};
use speedyspec_driver::{KubernetesResources, PluginRef, Progress};
use speedyspec_driver_kubernetes::{
    ConnectFlags, Connection, InstallIdentity, InstallOptions, Record, connect,
};
use speedyspec_testkit::FakeCluster;

static NO_KUBECONFIG: Once = Once::new();

/// A connection to the fake cluster. The user's own kubeconfig is put out of reach first.
pub fn connection(cluster: &FakeCluster) -> Connection {
    NO_KUBECONFIG.call_once(|| {
        // SAFETY: runs once, before any test in this binary has built a client.
        unsafe { std::env::set_var("KUBECONFIG", "/nonexistent/speedyspec-test-kubeconfig") };
    });
    let url = cluster.url();
    let flags = ConnectFlags {
        server: Some(&url),
        token_file: None,
        ca_cert: None,
        insecure_skip_tls_verify: false,
        context: None,
    };
    let mut connection = connect(&flags, Some(cluster.token()), &mut std::io::empty())
        .unwrap_or_else(|_| panic!("could not connect to the fake cluster"));
    // Unknown kinds are retried for a while in case their definition is still settling; tests
    // do not wait for that.
    connection.set_discovery_wait(Duration::from_millis(600));
    connection
}

#[derive(Default)]
pub struct Steps(pub Vec<String>);

impl Progress for Steps {
    fn step(&mut self, message: &str) {
        self.0.push(message.to_owned());
    }
}

pub const SHOP: InstallIdentity<'static> = InstallIdentity {
    app: "shop",
    name: "shop",
    namespace: "shop",
};

pub fn plugins() -> Vec<PluginRef> {
    vec![PluginRef {
        name: "acme/shop".to_owned(),
        version: "1.2.0".to_owned(),
    }]
}

pub fn options<'a>(identity: InstallIdentity<'a>, plugins: &'a [PluginRef]) -> InstallOptions<'a> {
    InstallOptions {
        identity,
        parts: &[],
        force_conflicts: false,
        plugins,
        cli_version: "0.1.0-test",
    }
}

pub fn answer(resources: Vec<Json>) -> KubernetesResources {
    KubernetesResources {
        resources,
        wait_for: None,
    }
}

pub fn settings() -> Json {
    json!({"apiVersion": "v1", "kind": "ConfigMap", "metadata": {"name": "settings"},
           "data": {"title": "Hi"}})
}

pub fn web() -> Json {
    json!({"apiVersion": "apps/v1", "kind": "Deployment", "metadata": {"name": "web"},
           "spec": {"replicas": 1, "selector": {"matchLabels": {"app": "web"}},
                    "template": {"metadata": {"labels": {"app": "web"}},
                                 "spec": {"containers": [{"name": "web", "image": "registry.k8s.io/pause:3.10"}]}}}})
}

pub fn secret() -> Json {
    json!({"apiVersion": "v1", "kind": "Secret", "metadata": {"name": "credentials"},
           "stringData": {"password": "s3cr3t-value"}})
}

/// The install record as stored in the fake cluster.
pub fn record(cluster: &FakeCluster, namespace: &str, name: &str) -> Record {
    let secret = cluster
        .object(
            "v1",
            "Secret",
            Some(namespace),
            &format!("speedyspec-install-{name}"),
        )
        .expect("the install record exists");
    Record::from_secret(&secret).expect("the install record reads")
}

pub fn recorded_names(record: &Record) -> Vec<String> {
    record
        .resources
        .iter()
        .map(|recorded| {
            let mark = if recorded.retained { "(retained)" } else { "" };
            format!("{}/{}{mark}", recorded.id.kind, recorded.id.name)
        })
        .collect()
}
