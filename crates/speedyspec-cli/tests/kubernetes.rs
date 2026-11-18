//! `speedyspec app install --driver kubernetes`, against the fake cluster only. The user's own
//! kubeconfig is always put out of reach.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use common::{Run, Speedyspec};
use serde_json::json;
use speedyspec_testkit::FakeCluster;

const NO_KUBECONFIG: &str = "/nonexistent/speedyspec-test-kubeconfig";

/// A `speedyspec` that can only ever reach `cluster`.
fn speedyspec(cluster: &FakeCluster, plugins: &[&str]) -> Speedyspec {
    Speedyspec::new(plugins)
        .env("KUBECONFIG", NO_KUBECONFIG)
        .env("SPEEDYSPEC_KUBE_TOKEN", cluster.token())
}

/// Runs `app install` non-interactively against `cluster` in namespace `shop`.
fn install(cluster: &FakeCluster, plugins: &[&str], args: &[&str]) -> Run {
    let url = cluster.url();
    let mut all = vec!["--no-interaction", "app", "install"];
    all.extend_from_slice(args);
    all.extend_from_slice(&["--server", &url, "--namespace", "shop", "--timeout", "3s"]);
    speedyspec(cluster, plugins).run(&all)
}

/// Plays the workload controllers while `run` executes: every Deployment that appears is marked
/// available.
fn with_controllers<T>(cluster: &FakeCluster, run: impl FnOnce() -> T) -> T {
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !stop.load(Ordering::SeqCst) {
                for deployment in cluster.objects_of("Deployment") {
                    if deployment["status"].is_null() {
                        cluster.set_status(
                            "apps/v1",
                            "Deployment",
                            deployment["metadata"]["namespace"].as_str(),
                            deployment["metadata"]["name"].as_str().unwrap(),
                            json!({"observedGeneration": 1, "updatedReplicas": 1, "availableReplicas": 1}),
                        );
                    }
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let result = run();
        stop.store(true, Ordering::SeqCst);
        result
    })
}

fn record(cluster: &FakeCluster, name: &str) -> serde_json::Value {
    let secret = cluster
        .object(
            "v1",
            "Secret",
            Some("shop"),
            &format!("speedyspec-install-{name}"),
        )
        .expect("the install record exists");
    serde_json::from_str(secret["stringData"]["record"].as_str().unwrap()).unwrap()
}

#[test]
fn installs_default_parts() {
    let cluster = FakeCluster::start();
    let out = with_controllers(&cluster, || {
        install(&cluster, &["k8s-app"], &["shop", "--title", "Hi"])
    });
    assert_eq!(out.code, 0, "{}", out.stderr);
    let settings = cluster
        .object("v1", "ConfigMap", Some("shop"), "settings")
        .unwrap();
    assert_eq!(settings["data"]["title"], "Hi");
    assert!(
        cluster
            .object("apps/v1", "Deployment", Some("shop"), "web")
            .is_some()
    );
    assert!(
        cluster
            .object("apps/v1", "Deployment", Some("shop"), "worker")
            .is_none()
    );
    assert_eq!(
        out.stdout,
        "after:2 shop shop\nshop\tConfigMap/settings\nshop\tDeployment/web\n"
    );
    assert_eq!(record(&cluster, "shop")["status"], "installed");
}

#[test]
fn installs_selected_parts_under_a_name() {
    let cluster = FakeCluster::start();
    let args = [
        "shop", "--part", "web", "--part", "worker", "--name", "prod", "--title", "Hi",
    ];
    let out = with_controllers(&cluster, || install(&cluster, &["k8s-app"], &args));
    assert_eq!(out.code, 0, "{}", out.stderr);
    for name in ["web", "worker"] {
        let deployment = cluster
            .object("apps/v1", "Deployment", Some("shop"), name)
            .unwrap();
        assert_eq!(
            deployment["metadata"]["labels"]["speedyspec.com/install"],
            "prod"
        );
        assert_eq!(
            deployment["metadata"]["labels"]["speedyspec.com/app"],
            "shop"
        );
    }
    assert_eq!(record(&cluster, "prod")["parts"], json!(["web", "worker"]));
    assert!(
        out.stdout.starts_with("after:3 shop prod\n"),
        "{}",
        out.stdout
    );
}

#[test]
fn record_names_the_answering_plugins() {
    let cluster = FakeCluster::start();
    let out = with_controllers(&cluster, || {
        install(
            &cluster,
            &["k8s-app", "k8s-adds"],
            &["shop", "--title", "Hi"],
        )
    });
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        cluster
            .object("v1", "Service", Some("shop"), "web")
            .is_some()
    );
    assert_eq!(
        record(&cluster, "shop")["plugins"],
        json!([
            {"name": "speedyspec-fixture/k8s-app", "version": "1.0.0"},
            {"name": "speedyspec-fixture/k8s-adds", "version": "1.0.0"},
        ])
    );
}

#[test]
fn bad_answer_refuses_the_install() {
    let cluster = FakeCluster::start();
    let out = install(
        &cluster,
        &["k8s-app", "k8s-bad"],
        &["shop", "--title", "Hi"],
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains(
            "speedyspec: speedyspec-fixture/k8s-bad: speedyspec_driver_kubernetes: \
             resources[1].metadata.name"
        ),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("speedyspec: nothing was installed"),
        "{}",
        out.stderr
    );
    assert!(cluster.writes().is_empty(), "{:?}", cluster.writes());
}

#[test]
fn empty_objects_and_empty_lists_survive() {
    let cluster = FakeCluster::start();
    let out = install(&cluster, &["k8s-empty-map"], &["maps"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let config_map = cluster
        .object("v1", "ConfigMap", Some("shop"), "empty")
        .unwrap();
    assert_eq!(config_map["data"], json!({}));
    let pod = cluster
        .object("v1", "Pod", Some("shop"), "scratch")
        .unwrap();
    assert_eq!(pod["spec"]["volumes"][0]["emptyDir"], json!({}));
    assert_eq!(pod["spec"]["containers"][0]["args"], json!([]));
}

#[test]
fn wait_without_a_rule_is_refused() {
    let cluster = FakeCluster::start();
    cluster.define("example.test", "v1", "Widget", "widgets", true);
    let out = install(&cluster, &["k8s-no-rule"], &["odd"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("waitFor[0]") && out.stderr.contains("Widget"),
        "{}",
        out.stderr
    );
    assert!(cluster.writes().is_empty(), "{:?}", cluster.writes());
}

#[test]
fn not_ready_in_time_fails_and_says_what() {
    let cluster = FakeCluster::start();
    // Nobody plays the controllers, so the Deployment never becomes available.
    let url = cluster.url();
    let out = speedyspec(&cluster, &["k8s-app"]).run(&[
        "--no-interaction",
        "app",
        "install",
        "shop",
        "--title",
        "Hi",
        "--server",
        &url,
        "--namespace",
        "shop",
        "--timeout",
        "1s",
    ]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("shop/Deployment/web"), "{}", out.stderr);
    assert_eq!(record(&cluster, "shop")["status"], "failed");
    assert!(
        cluster
            .object("apps/v1", "Deployment", Some("shop"), "web")
            .is_some(),
        "left in place"
    );
    assert!(
        !out.stdout.contains("after:"),
        "the after-hook must not run: {}",
        out.stdout
    );
}

#[test]
fn missing_namespace_flag_is_reported() {
    let cluster = FakeCluster::start();
    let url = cluster.url();
    let out = speedyspec(&cluster, &["k8s-app"]).run(&[
        "--no-interaction",
        "app",
        "install",
        "shop",
        "--title",
        "Hi",
        "--server",
        &url,
    ]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("--namespace"), "{}", out.stderr);
    assert!(cluster.requests().is_empty());
}

#[test]
fn wrong_token_is_reported_without_it() {
    let cluster = FakeCluster::start();
    let url = cluster.url();
    let out = Speedyspec::new(&["k8s-app"])
        .env("KUBECONFIG", NO_KUBECONFIG)
        .env("SPEEDYSPEC_KUBE_TOKEN", "s3cr3t-wrong")
        .run(&[
            "--no-interaction",
            "app",
            "install",
            "shop",
            "--title",
            "Hi",
            "--server",
            &url,
            "--namespace",
            "shop",
        ]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.to_lowercase().contains("unauthorized"),
        "{}",
        out.stderr
    );
    assert!(!out.stderr.contains("s3cr3t-wrong") && !out.stdout.contains("s3cr3t-wrong"));
    assert!(cluster.writes().is_empty());
}

#[test]
fn token_from_a_file_and_from_stdin() {
    let cluster = FakeCluster::start();
    let url = cluster.url();
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), format!("{}\n", cluster.token())).unwrap();
    let path = file.path().to_str().unwrap().to_owned();

    let from_file = with_controllers(&cluster, || {
        Speedyspec::new(&["k8s-app"])
            .env("KUBECONFIG", NO_KUBECONFIG)
            .env_remove("SPEEDYSPEC_KUBE_TOKEN")
            .run(&[
                "--no-interaction",
                "app",
                "install",
                "shop",
                "--title",
                "Hi",
                "--server",
                &url,
                "--namespace",
                "shop",
                "--timeout",
                "3s",
                "--token-file",
                &path,
            ])
    });
    assert_eq!(from_file.code, 0, "{}", from_file.stderr);

    let from_stdin = with_controllers(&cluster, || {
        Speedyspec::new(&["k8s-app"])
            .env("KUBECONFIG", NO_KUBECONFIG)
            .env_remove("SPEEDYSPEC_KUBE_TOKEN")
            .run_with_stdin(
                &[
                    "--no-interaction",
                    "app",
                    "install",
                    "shop",
                    "--title",
                    "Hi",
                    "--server",
                    &url,
                    "--namespace",
                    "shop",
                    "--timeout",
                    "3s",
                    "--token-file",
                    "-",
                ],
                &format!("{}\n", cluster.token()),
            )
    });
    assert_eq!(from_stdin.code, 0, "{}", from_stdin.stderr);
}

#[test]
fn token_and_server_never_reach_the_plugin() {
    let cluster = FakeCluster::start();
    let url = cluster.url();
    let out = with_controllers(&cluster, || {
        speedyspec(&cluster, &["k8s-app"])
            .env("K8S_FIXTURE_DUMP_REQUEST", "1")
            .run(&[
                "--no-interaction",
                "app",
                "install",
                "shop",
                "--title",
                "Hi",
                "--server",
                &url,
                "--namespace",
                "shop",
                "--timeout",
                "3s",
            ])
    });
    assert_eq!(out.code, 0, "{}", out.stderr);
    let dumped = out
        .stdout
        .lines()
        .find(|line| line.starts_with('{'))
        .expect("the request was dumped");
    let request: serde_json::Value = serde_json::from_str(dumped).unwrap();
    assert_eq!(
        request["settings"],
        json!({"namespace": "shop", "name": "shop"})
    );
    assert!(!out.stdout.contains(cluster.token()) && !out.stderr.contains(cluster.token()));
    assert!(!dumped.contains("127.0.0.1"), "{dumped}");
}

#[test]
fn app_list_shows_kubernetes_as_usable() {
    let out = Speedyspec::new(&["k8s-app"])
        .env("KUBECONFIG", NO_KUBECONFIG)
        .run(&["app", "list"]);
    assert_eq!(out.stdout, "shop\tShop\tkubernetes\n");
}

#[test]
fn zip_flags_are_refused_with_kubernetes() {
    let cluster = FakeCluster::start();
    let out = install(
        &cluster,
        &["k8s-app"],
        &["shop", "--title", "Hi", "--base", "x"],
    );
    assert_eq!(out.code, 2);
    assert!(
        out.stderr.contains("--base belongs to the zip driver"),
        "{}",
        out.stderr
    );
    assert!(cluster.requests().is_empty());
}

#[test]
fn help_lists_both_drivers_flags() {
    let out = Speedyspec::new(&["k8s-app"])
        .env("KUBECONFIG", NO_KUBECONFIG)
        .run(&["app", "install", "shop", "--help"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    for flag in [
        "--namespace",
        "--server",
        "--token-file",
        "--force-conflicts",
        "--base",
        "--title",
    ] {
        assert!(
            out.stdout.contains(flag),
            "{flag} missing from:\n{}",
            out.stdout
        );
    }
}

#[test]
fn reinstall_reports_what_is_left_running() {
    let cluster = FakeCluster::start();
    let first = with_controllers(&cluster, || {
        install(
            &cluster,
            &["k8s-app"],
            &["shop", "--part", "web", "--part", "worker", "--title", "Hi"],
        )
    });
    assert_eq!(first.code, 0, "{}", first.stderr);
    let second = with_controllers(&cluster, || {
        install(&cluster, &["k8s-app"], &["shop", "--title", "Hi"])
    });
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert!(
        second.stderr.contains("shop/Deployment/worker") && second.stderr.contains("left running"),
        "{}",
        second.stderr
    );
    assert!(
        cluster
            .object("apps/v1", "Deployment", Some("shop"), "worker")
            .is_some()
    );
    assert!(
        !second.stdout.contains("worker"),
        "only what was applied is listed: {}",
        second.stdout
    );
}

// ---- uninstall ----------------------------------------------------------------------------

fn uninstall(cluster: &FakeCluster, plugins: &[&str], args: &[&str]) -> Run {
    let url = cluster.url();
    let mut all = vec!["--no-interaction", "app", "uninstall"];
    all.extend_from_slice(args);
    all.extend_from_slice(&["--server", &url, "--namespace", "shop"]);
    speedyspec(cluster, plugins).run(&all)
}

fn installed(cluster: &FakeCluster) {
    let out = with_controllers(cluster, || {
        install(cluster, &["k8s-app"], &["shop", "--title", "Hi"])
    });
    assert_eq!(out.code, 0, "{}", out.stderr);
}

#[test]
fn uninstall_removes_the_install() {
    let cluster = FakeCluster::start();
    installed(&cluster);
    let out = uninstall(&cluster, &["k8s-app"], &["shop", "--yes"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        cluster
            .object("v1", "ConfigMap", Some("shop"), "settings")
            .is_none()
    );
    assert!(
        cluster
            .object("apps/v1", "Deployment", Some("shop"), "web")
            .is_none()
    );
    assert!(cluster.object("v1", "Namespace", None, "shop").is_none());
    assert_eq!(
        out.stdout,
        "shop\tDeployment/web\tdeleted\nshop\tConfigMap/settings\tdeleted\n-\tNamespace/shop\tdeleted\n"
    );
}

#[test]
fn uninstall_needs_confirmation() {
    let cluster = FakeCluster::start();
    installed(&cluster);
    let writes = cluster.writes().len();
    let out = uninstall(&cluster, &["k8s-app"], &["shop"]);
    assert_eq!(out.code, 2);
    assert!(
        out.stderr
            .contains("uninstalling shop (app shop) from namespace shop"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("speedyspec: confirm with --yes"),
        "{}",
        out.stderr
    );
    assert_eq!(out.stdout, "");
    assert_eq!(cluster.writes().len(), writes, "nothing was deleted");
}

#[test]
fn uninstall_works_without_plugins() {
    let cluster = FakeCluster::start();
    installed(&cluster);
    let url = cluster.url();
    let out = speedyspec(&cluster, &[]).run(&[
        "--no-plugins",
        "--no-interaction",
        "app",
        "uninstall",
        "shop",
        "--yes",
        "--server",
        &url,
        "--namespace",
        "shop",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        cluster
            .object("apps/v1", "Deployment", Some("shop"), "web")
            .is_none()
    );
}

#[test]
fn uninstall_of_nothing_is_reported() {
    let cluster = FakeCluster::start();
    cluster.put(json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "shop"}}));
    let out = uninstall(&cluster, &[], &["nope", "--yes"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .contains("speedyspec: no install named nope in namespace shop"),
        "{}",
        out.stderr
    );
}

#[test]
fn uninstall_driver_may_be_named_and_zip_cannot() {
    let cluster = FakeCluster::start();
    installed(&cluster);
    let out = uninstall(&cluster, &[], &["shop", "--yes", "--driver", "zip"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(
        out.stderr.contains("the zip driver cannot uninstall"),
        "{}",
        out.stderr
    );
    let out = uninstall(&cluster, &[], &["shop", "--yes", "--driver", "kubernetes"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
}

#[test]
fn uninstall_reports_what_it_skipped() {
    let cluster = FakeCluster::start();
    installed(&cluster);
    cluster.put(json!({"apiVersion": "v1", "kind": "ConfigMap",
                       "metadata": {"name": "settings", "namespace": "shop"}, "data": {"theirs": "yes"}}));
    let out = uninstall(&cluster, &[], &["shop", "--yes"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains("shop\tConfigMap/settings\tskipped\n"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("-\tNamespace/shop\tkept\n"),
        "{}",
        out.stdout
    );
    assert!(
        out.stderr.contains("no longer part of this install"),
        "{}",
        out.stderr
    );
    assert!(
        cluster
            .object("v1", "ConfigMap", Some("shop"), "settings")
            .is_some()
    );
}

#[test]
fn uninstall_does_not_wait_for_a_kind_the_cluster_no_longer_has() {
    use speedyspec_driver_kubernetes::{Record, Recorded, ResourceId, SCHEMA, Status};

    let cluster = FakeCluster::start();
    cluster.put(json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "shop"}}));
    // An install whose custom resource went away with its definition.
    let record = Record {
        schema: SCHEMA,
        name: "shop".to_owned(),
        app: "shop".to_owned(),
        parts: Vec::new(),
        status: Status::Installed,
        message: None,
        cli_version: "0.1.0".to_owned(),
        plugins: Vec::new(),
        created_namespaces: Vec::new(),
        resources: vec![Recorded {
            id: ResourceId {
                api_version: "example.test/v1".to_owned(),
                kind: "Widget".to_owned(),
                namespace: Some("shop".to_owned()),
                name: "w".to_owned(),
            },
            retained: false,
        }],
    };
    cluster.put(record.to_secret("shop"));

    let started = std::time::Instant::now();
    let out = uninstall(&cluster, &[], &["shop", "--yes"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert!(
        cluster
            .object("v1", "Secret", Some("shop"), "speedyspec-install-shop")
            .is_none()
    );
}
