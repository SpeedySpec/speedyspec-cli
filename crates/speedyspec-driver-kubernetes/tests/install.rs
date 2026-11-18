//! Applying an install, against the fake cluster.

mod support;

use std::time::Duration;

use serde_json::json;
use speedyspec_driver_kubernetes::{InstallIdentity, Status, apply_install};
use speedyspec_testkit::FakeCluster;
use support::{
    SHOP, Steps, answer, connection, options, plugins, record, recorded_names, secret, settings,
    web,
};

const LABELS: [(&str, &str); 3] = [
    ("app.kubernetes.io/managed-by", "speedyspec"),
    ("speedyspec.com/app", "shop"),
    ("speedyspec.com/install", "shop"),
];

#[test]
fn applies_resources_in_order_with_labels_and_field_manager() {
    let cluster = FakeCluster::start();
    let connection = connection(&cluster);
    let plugins = plugins();
    let applied = apply_install(
        &connection,
        &answer(vec![settings(), web()]),
        &options(SHOP, &plugins),
        &mut Steps::default(),
    )
    .unwrap();

    let writes = cluster.writes();
    let config_map = writes
        .iter()
        .position(|w| w.ends_with("/configmaps/settings"))
        .expect("the ConfigMap was applied");
    let deployment = writes
        .iter()
        .position(|w| w.ends_with("/deployments/web"))
        .expect("the Deployment was applied");
    assert!(config_map < deployment, "{writes:?}");
    for request in cluster.requests().iter().filter(|r| r.method == "PATCH") {
        assert_eq!(
            request.parameter("fieldManager"),
            Some("speedyspec"),
            "{}",
            request.path
        );
    }
    for (kind, version, name) in [
        ("ConfigMap", "v1", "settings"),
        ("Deployment", "apps/v1", "web"),
    ] {
        let stored = cluster
            .object(version, kind, Some("shop"), name)
            .expect("stored in shop");
        for (label, value) in LABELS {
            assert_eq!(stored["metadata"]["labels"][label], value, "{kind} {label}");
        }
    }
    let displays: Vec<String> = applied.resources.iter().map(|id| id.display()).collect();
    assert_eq!(
        displays,
        vec!["shop/ConfigMap/settings", "shop/Deployment/web"]
    );
    assert!(applied.retained.is_empty());
}

#[test]
fn creates_missing_namespaces_and_records_them() {
    let cluster = FakeCluster::start();
    let connection = connection(&cluster);
    let plugins = plugins();
    let mut elsewhere = settings();
    elsewhere["metadata"]["namespace"] = json!("data");
    apply_install(
        &connection,
        &answer(vec![elsewhere, web()]),
        &options(SHOP, &plugins),
        &mut Steps::default(),
    )
    .unwrap();
    for namespace in ["shop", "data"] {
        let created = cluster
            .object("v1", "Namespace", None, namespace)
            .expect("created");
        assert_eq!(
            created["metadata"]["labels"]["speedyspec.com/install"],
            "shop"
        );
    }
    assert!(
        cluster
            .object("v1", "ConfigMap", Some("data"), "settings")
            .is_some()
    );
    assert_eq!(
        record(&cluster, "shop", "shop").created_namespaces,
        vec!["shop", "data"]
    );
}

#[test]
fn existing_namespace_is_not_recorded_as_created() {
    let cluster = FakeCluster::start();
    cluster.put(json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "shop"}}));
    let connection = connection(&cluster);
    let plugins = plugins();
    apply_install(
        &connection,
        &answer(vec![settings()]),
        &options(SHOP, &plugins),
        &mut Steps::default(),
    )
    .unwrap();
    assert!(
        record(&cluster, "shop", "shop")
            .created_namespaces
            .is_empty()
    );
    let untouched = cluster.object("v1", "Namespace", None, "shop").unwrap();
    assert!(
        untouched["metadata"]["labels"].is_null(),
        "an existing namespace is not relabelled"
    );
}

#[test]
fn record_is_written_before_the_first_apply() {
    let cluster = FakeCluster::start();
    let connection = connection(&cluster);
    let plugins = plugins();
    apply_install(
        &connection,
        &answer(vec![settings(), web()]),
        &options(SHOP, &plugins),
        &mut Steps::default(),
    )
    .unwrap();
    let writes = cluster.writes();
    let record_at = writes
        .iter()
        .position(|w| w.ends_with("/secrets/speedyspec-install-shop"))
        .expect("the record was written");
    let first_resource = writes
        .iter()
        .position(|w| w.ends_with("/configmaps/settings"))
        .unwrap();
    assert!(record_at < first_resource, "{writes:?}");
}

#[test]
fn record_lists_everything_about_to_be_applied() {
    let cluster = FakeCluster::start();
    let connection = connection(&cluster);
    let plugins = plugins();
    let parts = ["web".to_owned()];
    let mut options = options(SHOP, &plugins);
    options.parts = &parts;
    apply_install(
        &connection,
        &answer(vec![settings(), web()]),
        &options,
        &mut Steps::default(),
    )
    .unwrap();
    let record = record(&cluster, "shop", "shop");
    assert_eq!(
        recorded_names(&record),
        vec!["ConfigMap/settings", "Deployment/web"]
    );
    assert_eq!(
        record.status,
        Status::Installing,
        "marking it installed is the wait's job"
    );
    assert_eq!(record.app, "shop");
    assert_eq!(record.parts, vec!["web"]);
    assert_eq!(record.plugins, plugins);
    assert_eq!(record.cli_version, "0.1.0-test");
}

#[test]
fn failure_marks_the_record_failed_and_keeps_what_was_applied() {
    let cluster = FakeCluster::start();
    let connection = connection(&cluster);
    let plugins = plugins();
    cluster.fail_next("PATCH", "Deployment", 422, "spec.replicas: Invalid value");
    let error = apply_install(
        &connection,
        &answer(vec![settings(), web(), secret()]),
        &options(SHOP, &plugins),
        &mut Steps::default(),
    )
    .unwrap_err();
    assert!(error.contains("shop/Deployment/web"), "{error}");
    assert!(error.contains("spec.replicas: Invalid value"), "{error}");

    let record = record(&cluster, "shop", "shop");
    assert_eq!(record.status, Status::Failed);
    assert!(
        record
            .message
            .as_deref()
            .unwrap()
            .contains("shop/Deployment/web")
    );
    assert_eq!(
        recorded_names(&record),
        vec!["ConfigMap/settings", "Deployment/web", "Secret/credentials"]
    );
    assert!(
        cluster
            .object("v1", "ConfigMap", Some("shop"), "settings")
            .is_some(),
        "kept"
    );
    assert!(
        cluster
            .object("v1", "Secret", Some("shop"), "credentials")
            .is_none(),
        "not reached"
    );
}

#[test]
fn error_messages_never_contain_resource_bodies() {
    let cluster = FakeCluster::start();
    let connection = connection(&cluster);
    let plugins = plugins();
    cluster.fail_next_named(
        "PATCH",
        "Secret",
        "credentials",
        422,
        "the Secret is invalid",
    );
    let mut steps = Steps::default();
    let error = apply_install(
        &connection,
        &answer(vec![secret()]),
        &options(SHOP, &plugins),
        &mut steps,
    )
    .unwrap_err();
    assert!(error.contains("shop/Secret/credentials"), "{error}");
    assert!(!error.contains("s3cr3t-value"), "{error}");
    assert!(!steps.0.join("\n").contains("s3cr3t-value"));
    assert!(
        !record(&cluster, "shop", "shop")
            .message
            .unwrap()
            .contains("s3cr3t-value")
    );
}

#[test]
fn conflict_names_field_and_manager() {
    let cluster = FakeCluster::start();
    let connection = connection(&cluster);
    let plugins = plugins();
    apply_install(
        &connection,
        &answer(vec![web()]),
        &options(SHOP, &plugins),
        &mut Steps::default(),
    )
    .unwrap();
    cluster.own_field(
        "apps/v1",
        "Deployment",
        Some("shop"),
        "web",
        "kubectl-edit",
        ".spec.replicas",
    );

    let error = apply_install(
        &connection,
        &answer(vec![web()]),
        &options(SHOP, &plugins),
        &mut Steps::default(),
    )
    .unwrap_err();
    assert_eq!(
        error,
        "shop/Deployment/web: field .spec.replicas is owned by kubectl-edit; use --force-conflicts \
         to take it over"
    );
    assert_eq!(record(&cluster, "shop", "shop").status, Status::Failed);
    let forced = cluster
        .requests()
        .iter()
        .any(|r| r.parameter("force") == Some("true") && r.path.contains("deployments"));
    assert!(!forced, "nothing is forced without the flag");
}

#[test]
fn force_conflicts_takes_ownership() {
    let cluster = FakeCluster::start();
    let connection = connection(&cluster);
    let plugins = plugins();
    apply_install(
        &connection,
        &answer(vec![web()]),
        &options(SHOP, &plugins),
        &mut Steps::default(),
    )
    .unwrap();
    cluster.own_field(
        "apps/v1",
        "Deployment",
        Some("shop"),
        "web",
        "kubectl-edit",
        ".spec.replicas",
    );
    let mut forced = options(SHOP, &plugins);
    forced.force_conflicts = true;
    apply_install(
        &connection,
        &answer(vec![web()]),
        &forced,
        &mut Steps::default(),
    )
    .unwrap();
    let last = cluster
        .requests()
        .into_iter()
        .rev()
        .find(|r| r.method == "PATCH" && r.path.ends_with("/deployments/web"))
        .unwrap();
    assert_eq!(last.parameter("force"), Some("true"));
}

#[test]
fn reinstall_with_fewer_resources_retains_the_dropped_ones() {
    let cluster = FakeCluster::start();
    let connection = connection(&cluster);
    let plugins = plugins();
    apply_install(
        &connection,
        &answer(vec![settings(), web()]),
        &options(SHOP, &plugins),
        &mut Steps::default(),
    )
    .unwrap();
    let applied = apply_install(
        &connection,
        &answer(vec![web()]),
        &options(SHOP, &plugins),
        &mut Steps::default(),
    )
    .unwrap();
    let retained: Vec<String> = applied.retained.iter().map(|id| id.display()).collect();
    assert_eq!(retained, vec!["shop/ConfigMap/settings"]);
    assert!(
        cluster
            .object("v1", "ConfigMap", Some("shop"), "settings")
            .is_some(),
        "left running"
    );
    assert_eq!(
        recorded_names(&record(&cluster, "shop", "shop")),
        vec!["Deployment/web", "ConfigMap/settings(retained)"]
    );
}

#[test]
fn reinstall_under_another_app_is_refused() {
    let cluster = FakeCluster::start();
    let connection = connection(&cluster);
    let plugins = plugins();
    apply_install(
        &connection,
        &answer(vec![settings()]),
        &options(SHOP, &plugins),
        &mut Steps::default(),
    )
    .unwrap();
    let writes_before = cluster.writes().len();
    let blog = InstallIdentity {
        app: "blog",
        name: "shop",
        namespace: "shop",
    };
    let error = apply_install(
        &connection,
        &answer(vec![web()]),
        &options(blog, &plugins),
        &mut Steps::default(),
    )
    .unwrap_err();
    assert!(
        error.contains("shop") && error.contains("blog") && error.contains("--name"),
        "{error}"
    );
    assert_eq!(cluster.writes().len(), writes_before, "nothing was changed");
}

#[test]
fn empty_answer_is_refused_before_touching_the_cluster() {
    let cluster = FakeCluster::start();
    let connection = connection(&cluster);
    let plugins = plugins();
    let error = apply_install(
        &connection,
        &answer(vec![]),
        &options(SHOP, &plugins),
        &mut Steps::default(),
    )
    .unwrap_err();
    assert_eq!(error, "no plugin supplied anything to install");
    assert!(cluster.requests().is_empty());
}

#[test]
fn invalid_resource_stops_before_anything_is_applied() {
    let cluster = FakeCluster::start();
    let connection = connection(&cluster);
    let plugins = plugins();
    let bad = json!({"apiVersion": "v1", "kind": "Namespace",
                     "metadata": {"name": "extra", "namespace": "shop"}});
    let error = apply_install(
        &connection,
        &answer(vec![settings(), bad]),
        &options(SHOP, &plugins),
        &mut Steps::default(),
    )
    .unwrap_err();
    assert!(error.contains("Namespace is cluster-wide"), "{error}");
    assert!(cluster.writes().is_empty(), "{:?}", cluster.writes());
}

#[test]
fn custom_resource_after_its_definition() {
    let cluster = FakeCluster::start();
    let mut connection = connection(&cluster);
    connection.set_discovery_wait(Duration::from_secs(5));
    let plugins = plugins();
    let widget = json!({"apiVersion": "example.test/v1", "kind": "Widget", "metadata": {"name": "w"},
                        "spec": {"size": 1}});
    // The definition settles a moment after the install starts, as it does on a real cluster.
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(300));
            cluster.define("example.test", "v1", "Widget", "widgets", true);
        });
        apply_install(
            &connection,
            &answer(vec![settings(), widget]),
            &options(SHOP, &plugins),
            &mut Steps::default(),
        )
        .unwrap();
    });
    assert!(
        cluster
            .object("example.test/v1", "Widget", Some("shop"), "w")
            .is_some()
    );
}

#[test]
fn unknown_kind_is_reported() {
    let cluster = FakeCluster::start();
    let connection = connection(&cluster);
    let plugins = plugins();
    let nope = json!({"apiVersion": "example.test/v1", "kind": "Nope", "metadata": {"name": "n"}});
    let error = apply_install(
        &connection,
        &answer(vec![nope]),
        &options(SHOP, &plugins),
        &mut Steps::default(),
    )
    .unwrap_err();
    assert!(
        error.contains("Nope") && error.contains("example.test/v1"),
        "{error}"
    );
    assert!(cluster.writes().is_empty());
}

#[test]
fn reports_progress_without_bodies() {
    let cluster = FakeCluster::start();
    let connection = connection(&cluster);
    let plugins = plugins();
    let mut steps = Steps::default();
    apply_install(
        &connection,
        &answer(vec![settings(), web()]),
        &options(SHOP, &plugins),
        &mut steps,
    )
    .unwrap();
    assert!(
        steps
            .0
            .contains(&"applying shop/ConfigMap/settings".to_owned()),
        "{:?}",
        steps.0
    );
    assert!(
        steps.0.contains(&"applying shop/Deployment/web".to_owned()),
        "{:?}",
        steps.0
    );
}
