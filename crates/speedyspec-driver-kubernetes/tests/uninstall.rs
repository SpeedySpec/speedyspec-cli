//! Removing an install, against the fake cluster.

mod support;

use std::time::Duration;

use serde_json::json;
use speedyspec_driver_kubernetes::{
    InstallIdentity, ResourceId, apply_install, find_install, remove_install, summarise,
};
use speedyspec_testkit::FakeCluster;
use support::{SHOP, Steps, answer, connection, options, plugins, secret, settings, web};

const TIMEOUT: Duration = Duration::from_secs(5);
const QUICK: Duration = Duration::from_millis(20);

fn install(
    cluster: &FakeCluster,
    identity: InstallIdentity<'_>,
    resources: Vec<serde_json::Value>,
) {
    let connection = connection(cluster);
    let plugins = plugins();
    apply_install(
        &connection,
        &answer(resources),
        &options(identity, &plugins),
        &mut Steps::default(),
    )
    .unwrap();
}

fn remove(
    cluster: &FakeCluster,
    name: &str,
) -> Result<speedyspec_driver_kubernetes::Removed, String> {
    let connection = connection(cluster);
    let removal = find_install(&connection, name, "shop")?;
    remove_install(&connection, &removal, TIMEOUT, QUICK, &mut Steps::default())
}

fn displays(ids: &[ResourceId]) -> Vec<String> {
    ids.iter().map(ResourceId::display).collect()
}

fn pvc() -> serde_json::Value {
    json!({"apiVersion": "v1", "kind": "PersistentVolumeClaim", "metadata": {"name": "data"},
           "spec": {"accessModes": ["ReadWriteOnce"], "resources": {"requests": {"storage": "1Gi"}}}})
}

#[test]
fn removes_everything_in_reverse_order() {
    let cluster = FakeCluster::start();
    install(&cluster, SHOP, vec![settings(), web(), secret()]);
    let removed = remove(&cluster, "shop").unwrap();
    assert_eq!(
        displays(&removed.deleted),
        vec![
            "shop/Secret/credentials",
            "shop/Deployment/web",
            "shop/ConfigMap/settings"
        ]
    );
    let deletes: Vec<String> = cluster
        .writes()
        .into_iter()
        .filter(|w| w.starts_with("DELETE"))
        .collect();
    let position = |suffix: &str| {
        deletes
            .iter()
            .position(|d| d.ends_with(suffix))
            .expect(suffix)
    };
    assert!(
        position("/secrets/credentials") < position("/deployments/web"),
        "{deletes:?}"
    );
    assert!(
        position("/deployments/web") < position("/configmaps/settings"),
        "{deletes:?}"
    );
    assert!(
        position("/configmaps/settings") < position("/secrets/speedyspec-install-shop"),
        "the record goes after the resources: {deletes:?}"
    );
    for (version, kind, name) in [
        ("v1", "ConfigMap", "settings"),
        ("apps/v1", "Deployment", "web"),
    ] {
        assert!(
            cluster.object(version, kind, Some("shop"), name).is_none(),
            "{kind}"
        );
    }
    assert!(
        cluster
            .object("v1", "Secret", Some("shop"), "speedyspec-install-shop")
            .is_none()
    );
}

#[test]
fn removes_retained_resources_too() {
    let cluster = FakeCluster::start();
    install(&cluster, SHOP, vec![settings(), web()]);
    install(&cluster, SHOP, vec![web()]);
    let removed = remove(&cluster, "shop").unwrap();
    assert!(displays(&removed.deleted).contains(&"shop/ConfigMap/settings".to_owned()));
    assert!(
        cluster
            .object("v1", "ConfigMap", Some("shop"), "settings")
            .is_none()
    );
}

#[test]
fn already_gone_is_not_an_error() {
    let cluster = FakeCluster::start();
    install(&cluster, SHOP, vec![settings(), web()]);
    let connection = connection(&cluster);
    // Someone deleted it behind the install's back.
    connection
        .delete(&ResourceId {
            api_version: "v1".to_owned(),
            kind: "ConfigMap".to_owned(),
            namespace: Some("shop".to_owned()),
            name: "settings".to_owned(),
        })
        .unwrap();
    let removed = remove(&cluster, "shop").unwrap();
    assert_eq!(
        displays(&removed.already_gone),
        vec!["shop/ConfigMap/settings"]
    );
    assert_eq!(displays(&removed.deleted), vec!["shop/Deployment/web"]);
}

#[test]
fn a_relabelled_resource_is_skipped() {
    let cluster = FakeCluster::start();
    install(&cluster, SHOP, vec![settings(), web()]);
    // Something else replaced it: same name, no longer this install's.
    cluster.put(json!({"apiVersion": "v1", "kind": "ConfigMap",
                       "metadata": {"name": "settings", "namespace": "shop",
                                    "labels": {"speedyspec.com/install": "another"}},
                       "data": {"theirs": "yes"}}));
    let removed = remove(&cluster, "shop").unwrap();
    assert_eq!(removed.skipped.len(), 1);
    assert_eq!(removed.skipped[0].0.display(), "shop/ConfigMap/settings");
    assert!(
        removed.skipped[0]
            .1
            .contains("no longer part of this install"),
        "{}",
        removed.skipped[0].1
    );
    let kept = cluster
        .object("v1", "ConfigMap", Some("shop"), "settings")
        .expect("left alone");
    assert_eq!(kept["data"]["theirs"], "yes");
    // The install created the namespace, but it cannot go while it holds what was skipped.
    assert!(removed.namespaces.is_empty(), "{:?}", removed.namespaces);
    assert_eq!(removed.kept_namespaces.len(), 1);
    assert_eq!(removed.kept_namespaces[0].0, "shop");
    assert!(
        removed.kept_namespaces[0]
            .1
            .contains("shop/ConfigMap/settings")
    );
    assert!(cluster.object("v1", "Namespace", None, "shop").is_some());
    assert!(
        cluster
            .object("apps/v1", "Deployment", Some("shop"), "web")
            .is_none(),
        "the rest went"
    );
}

#[test]
fn deletes_only_namespaces_it_created() {
    let cluster = FakeCluster::start();
    cluster.put(json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "shop"}}));
    let mut elsewhere = settings();
    elsewhere["metadata"]["namespace"] = json!("data");
    install(&cluster, SHOP, vec![elsewhere, web()]);
    let removed = remove(&cluster, "shop").unwrap();
    assert_eq!(removed.namespaces, vec!["data"]);
    assert!(cluster.object("v1", "Namespace", None, "data").is_none());
    assert!(
        cluster.object("v1", "Namespace", None, "shop").is_some(),
        "it was there before"
    );
}

#[test]
fn the_records_own_namespace_goes_last() {
    let cluster = FakeCluster::start();
    install(&cluster, SHOP, vec![settings()]);
    let removed = remove(&cluster, "shop").unwrap();
    assert_eq!(removed.namespaces, vec!["shop"]);
    let deletes: Vec<String> = cluster
        .writes()
        .into_iter()
        .filter(|w| w.starts_with("DELETE"))
        .collect();
    let record = deletes
        .iter()
        .position(|d| d.ends_with("/secrets/speedyspec-install-shop"))
        .unwrap();
    let namespace = deletes
        .iter()
        .position(|d| d.ends_with("/api/v1/namespaces/shop"))
        .unwrap();
    assert!(record < namespace, "{deletes:?}");
    assert!(cluster.object("v1", "Namespace", None, "shop").is_none());
}

#[test]
fn a_created_namespace_someone_took_over_is_kept() {
    let cluster = FakeCluster::start();
    let mut elsewhere = settings();
    elsewhere["metadata"]["namespace"] = json!("data");
    install(&cluster, SHOP, vec![elsewhere]);
    // The namespace is no longer labelled as this install's.
    cluster.put(json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "data"}}));
    let removed = remove(&cluster, "shop").unwrap();
    assert!(!removed.namespaces.contains(&"data".to_owned()));
    assert!(cluster.object("v1", "Namespace", None, "data").is_some());
}

#[test]
fn missing_record_is_a_clear_error() {
    let cluster = FakeCluster::start();
    cluster.put(json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "shop"}}));
    let connection = connection(&cluster);
    let error = find_install(&connection, "nope", "shop")
        .err()
        .expect("there is no such install");
    assert_eq!(error, "no install named nope in namespace shop");
}

#[test]
fn summary_says_what_goes_and_warns_about_persistent_volume_claims() {
    let cluster = FakeCluster::start();
    install(&cluster, SHOP, vec![settings(), web(), pvc()]);
    let connection = connection(&cluster);
    let removal = find_install(&connection, "shop", "shop").unwrap();
    let summary = summarise(&removal).join("\n");
    assert!(summary.contains("shop"), "{summary}");
    assert!(
        summary.contains("1 ConfigMap") && summary.contains("1 Deployment"),
        "{summary}"
    );
    assert!(summary.contains("namespace shop"), "{summary}");
    assert!(
        summary.contains("this deletes 1 PersistentVolumeClaim(s) and the data in them"),
        "{summary}"
    );
}

#[test]
fn a_failed_delete_keeps_the_record() {
    let cluster = FakeCluster::start();
    install(&cluster, SHOP, vec![settings(), web()]);
    cluster.fail_next("DELETE", "Deployment", 500, "the server is on fire");
    let error = remove(&cluster, "shop").unwrap_err();
    assert!(
        error.contains("shop/Deployment/web") && error.contains("on fire"),
        "{error}"
    );
    assert!(
        cluster
            .object("v1", "Secret", Some("shop"), "speedyspec-install-shop")
            .is_some(),
        "the record stays so the command can be run again"
    );
    remove(&cluster, "shop").unwrap();
    assert!(
        cluster
            .object("apps/v1", "Deployment", Some("shop"), "web")
            .is_none()
    );
}

#[test]
fn uninstall_after_a_failed_install_cleans_up() {
    let cluster = FakeCluster::start();
    let connection = connection(&cluster);
    let plugins = plugins();
    cluster.fail_next_named("PATCH", "Secret", "credentials", 422, "invalid");
    apply_install(
        &connection,
        &answer(vec![settings(), web(), secret()]),
        &options(SHOP, &plugins),
        &mut Steps::default(),
    )
    .unwrap_err();
    let removed = remove(&cluster, "shop").unwrap();
    assert_eq!(
        displays(&removed.already_gone),
        vec!["shop/Secret/credentials"]
    );
    assert_eq!(
        displays(&removed.deleted),
        vec!["shop/Deployment/web", "shop/ConfigMap/settings"]
    );
    assert!(cluster.object("v1", "Namespace", None, "shop").is_none());
}
