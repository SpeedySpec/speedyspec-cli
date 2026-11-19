//! The maintenance commands on Kubernetes installs, against the fake cluster only. The user's own kubeconfig is
//! always put out of reach.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use common::{Run, Speedyspec};
use serde_json::json;
use speedyspec_testkit::FakeCluster;

const NO_KUBECONFIG: &str = "/nonexistent/speedyspec-test-kubeconfig";
const PLUGINS: &[&str] = &["k8s-app", "k8s-runtime"];

/// A `speedyspec` that can only ever reach `cluster`.
fn speedyspec(cluster: &FakeCluster) -> Speedyspec {
    Speedyspec::new(PLUGINS)
        .env("KUBECONFIG", NO_KUBECONFIG)
        .env("SPEEDYSPEC_KUBE_TOKEN", cluster.token())
}

/// Plays the workload controllers while `run` executes: every Deployment that appears is marked available.
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

/// The shop installed in namespace `shop` of `cluster`.
fn installed(cluster: &FakeCluster) {
    let url = cluster.url();
    let out = with_controllers(cluster, || {
        speedyspec(cluster).run(&[
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
}

/// `args`, then the flags that reach `cluster`'s namespace `shop`.
fn on_cluster(cluster: &FakeCluster, args: &[&str]) -> Run {
    on_cluster_with(speedyspec(cluster), cluster, args)
}

fn on_cluster_with(speedyspec: Speedyspec, cluster: &FakeCluster, args: &[&str]) -> Run {
    let url = cluster.url();
    let mut all = vec!["--no-interaction"];
    all.extend_from_slice(args);
    all.extend_from_slice(&["--server", &url, "--namespace", "shop"]);
    speedyspec.run(&all)
}

#[test]
fn a_kubernetes_install_is_found_by_name_and_namespace() {
    let cluster = FakeCluster::start();
    installed(&cluster);
    let out = on_cluster(&cluster, &["self", "database", "--install", "shop"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("database asked for shop on kubernetes, root NULL"),
        "{}",
        out.stderr
    );
    assert!(out.stdout.contains("mysql.shop.svc"), "{}", out.stdout);
    assert!(!out.stdout.contains("hunter2-secret"));
}

#[test]
fn one_that_does_not_exist_is_named() {
    let cluster = FakeCluster::start();
    installed(&cluster);
    for args in [
        vec!["plugin", "list", "--install", "nope"],
        vec!["app", "db", "tables", "--install", "nope"],
        vec!["app", "backup", "--install", "nope"],
    ] {
        let out = on_cluster(&cluster, &args);
        assert_eq!(out.code, 1, "{args:?}: {}", out.stderr);
        assert!(
            out.stderr
                .contains("no install named nope in namespace shop"),
            "{args:?}: {}",
            out.stderr
        );
    }
}

#[test]
fn the_flags_of_another_driver_are_refused() {
    let cluster = FakeCluster::start();
    let out = on_cluster(
        &cluster,
        &["plugin", "list", "--install", "shop", "--base", "/tmp"],
    );
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("--base"), "{}", out.stderr);
}

#[test]
fn the_cluster_flags_need_a_namespace() {
    let cluster = FakeCluster::start();
    let url = cluster.url();
    let out = speedyspec(&cluster).run(&[
        "--no-interaction",
        "plugin",
        "list",
        "--install",
        "shop",
        "--server",
        &url,
    ]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("--namespace"), "{}", out.stderr);
}

#[test]
fn an_install_whose_plugin_names_no_workload_is_refused() {
    let cluster = FakeCluster::start();
    installed(&cluster);
    let out = on_cluster_with(
        speedyspec(&cluster).env("K8S_NO_WORKLOAD", "1"),
        &cluster,
        &["plugin", "list", "--install", "shop"],
    );
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("shop's plugin does not say which workload serves it on Kubernetes"),
        "{}",
        out.stderr
    );
}
