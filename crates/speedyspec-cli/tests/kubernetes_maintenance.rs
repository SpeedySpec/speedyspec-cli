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

fn record(cluster: &FakeCluster) -> serde_json::Value {
    let secret = cluster
        .object("v1", "Secret", Some("shop"), "speedyspec-install-shop")
        .expect("the install record exists");
    serde_json::from_str(secret["stringData"]["record"].as_str().unwrap()).unwrap()
}

fn install_with_setup(cluster: &FakeCluster) -> Run {
    let url = cluster.url();
    with_controllers(cluster, || {
        speedyspec(cluster).env("K8S_SETUP", "1").run(&[
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
    })
}

#[test]
fn setup_progress_is_recorded_in_the_cluster_and_resumed() {
    let cluster = FakeCluster::start();
    // The fake cluster runs no pods, so the runner pod cannot be placed and the first task stops the setup.
    let out = install_with_setup(&cluster);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("setup: one.php (1/2)"),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("no ready pod"), "{}", out.stderr);
    assert!(
        out.stderr.contains("run the same command again"),
        "{}",
        out.stderr
    );
    let first = record(&cluster);
    assert_eq!(first["status"], "files", "{first}");
    assert_eq!(first["setupDone"], 0, "{first}");

    // As if the first task had finished before the CLI stopped: the next run starts at the second.
    let mut secret = cluster
        .object("v1", "Secret", Some("shop"), "speedyspec-install-shop")
        .unwrap();
    let mut progressed = first.clone();
    progressed["setupDone"] = json!(1);
    secret["stringData"]["record"] = json!(progressed.to_string());
    cluster.put(secret);
    let out = install_with_setup(&cluster);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("setup: two.php (2/2)"),
        "{}",
        out.stderr
    );
    assert!(!out.stderr.contains("(1/2)"), "{}", out.stderr);
    assert_eq!(record(&cluster)["setupDone"], 1);
    assert!(!out.stderr.contains("hunter2-secret"));
}

#[test]
fn an_install_without_setup_is_recorded_as_installed() {
    let cluster = FakeCluster::start();
    installed(&cluster);
    assert_eq!(record(&cluster)["status"], "installed");
}

// Against the throwaway k3s from `scripts/k3s.sh up`, never anything else, with a static Linux build of this CLI for
// its node as the runner binary:
//
//   SPEEDYSPEC_E2E_KUBECONFIG="$(scripts/k3s.sh kubeconfig)" SPEEDYSPEC_E2E_RUNNER_BINARY=<linux build> \
//     cargo test -p speedyspec-cli --test kubernetes_maintenance -- --ignored --test-threads=1

const E2E_NAMESPACE: &str = "speedyspec-cli-e2e";

fn needed(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!("set {name}; these tests use only the throwaway k3s from scripts/k3s.sh and a Linux build of the CLI")
    })
}

/// A `speedyspec` that reaches only the throwaway k3s.
fn on_k3s(plugins: &[&str]) -> Speedyspec {
    Speedyspec::new(plugins)
        .env("KUBECONFIG", needed("SPEEDYSPEC_E2E_KUBECONFIG"))
        .env_remove("SPEEDYSPEC_KUBE_TOKEN")
}

/// `kubectl` inside the k3s container.
fn kubectl(args: &[&str]) -> String {
    let output = std::process::Command::new("docker")
        .args(["exec", "speedyspec-k3s", "kubectl"])
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// `command` in the site's own pod.
fn in_site(name: &str, command: &[&str]) -> String {
    let mut args = vec![
        "exec",
        "-n",
        E2E_NAMESPACE,
        &format!("deploy/{name}"),
        "-c",
        "web",
        "--",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    args.extend(command.iter().map(|part| (*part).to_owned()));
    kubectl(&args.iter().map(String::as_str).collect::<Vec<_>>())
}

fn e2e_record(name: &str) -> serde_json::Value {
    let encoded = kubectl(&[
        "get",
        "secret",
        "-n",
        E2E_NAMESPACE,
        &format!("speedyspec-install-{name}"),
        "-o",
        "jsonpath={.data.record}",
    ]);
    let output = std::process::Command::new("base64")
        .arg("-d")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    output
        .stdin
        .as_ref()
        .unwrap()
        .write_all(encoded.as_bytes())
        .unwrap();
    let decoded = output.wait_with_output().unwrap();
    serde_json::from_slice(&decoded.stdout).unwrap_or(serde_json::Value::Null)
}

fn install_site(name: &str, speedyspec: Speedyspec) -> Run {
    let binary = needed("SPEEDYSPEC_E2E_RUNNER_BINARY");
    speedyspec.run(&[
        "--no-interaction",
        "app",
        "install",
        "site",
        "--name",
        name,
        "--namespace",
        E2E_NAMESPACE,
        "--timeout",
        "4m",
        "--runner-binary",
        &binary,
    ])
}

/// Removes an earlier run's site, so each test starts fresh.
fn fresh_site(name: &str) {
    let _ = on_k3s(&["k8s-site"]).run(&[
        "--no-interaction",
        "app",
        "uninstall",
        name,
        "--namespace",
        E2E_NAMESPACE,
        "--yes",
    ]);
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up and SPEEDYSPEC_E2E_RUNNER_BINARY"]
fn setup_tasks_run_in_the_runner_pod() {
    fresh_site("setup-ok");
    let out = install_site("setup-ok", on_k3s(&["k8s-site"]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr.contains("setup: config.php (1/3)"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("config written in /srv/app"),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("requests left: 0"), "{}", out.stderr);
    assert_eq!(
        in_site("setup-ok", &["cat", "/srv/app/config.txt"]),
        "secret=hunter2-secret\n"
    );
    let record = e2e_record("setup-ok");
    assert_eq!(record["status"], "installed", "{record}");
    assert_eq!(record["setupDone"], 3, "{record}");
    assert!(
        kubectl(&[
            "get",
            "jobs",
            "-n",
            E2E_NAMESPACE,
            "-l",
            "speedyspec.install=setup-ok",
            "-o",
            "name"
        ])
        .is_empty()
    );
    assert!(!out.stderr.contains("hunter2-secret") && !out.stdout.contains("hunter2-secret"));
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up and SPEEDYSPEC_E2E_RUNNER_BINARY"]
fn a_failed_task_resumes_on_re_run() {
    fresh_site("setup-again");
    let out = install_site(
        "setup-again",
        on_k3s(&["k8s-site"]).env("K8S_SITE_FAIL", "1"),
    );
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("the second task failed this time"),
        "{}",
        out.stderr
    );
    let record = e2e_record("setup-again");
    assert_eq!(record["status"], "files", "{record}");
    assert_eq!(record["setupDone"], 1, "{record}");
    let out = install_site("setup-again", on_k3s(&["k8s-site"]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr.contains("setup: second.php (2/3)"),
        "{}",
        out.stderr
    );
    assert!(!out.stderr.contains("(1/3)"), "{}", out.stderr);
    assert_eq!(
        in_site("setup-again", &["cat", "/srv/app/order.txt"]),
        "second"
    );
    assert_eq!(e2e_record("setup-again")["status"], "installed");
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up and SPEEDYSPEC_E2E_RUNNER_BINARY"]
fn the_request_file_never_stays() {
    fresh_site("setup-secret");
    // While the setup runs, the runner Job and pod as the cluster has them, and the runner's logs.
    let done = std::sync::atomic::AtomicBool::new(false);
    let seen = std::sync::Mutex::new(String::new());
    let out = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                let selector = "speedyspec.install=setup-secret";
                let mut text = kubectl(&[
                    "get",
                    "jobs,pods",
                    "-n",
                    E2E_NAMESPACE,
                    "-l",
                    selector,
                    "-o",
                    "json",
                ]);
                text.push_str(&kubectl(&[
                    "logs",
                    "-n",
                    E2E_NAMESPACE,
                    "-l",
                    selector,
                    "--all-containers",
                ]));
                seen.lock().unwrap().push_str(&text);
                std::thread::sleep(Duration::from_millis(500));
            }
        });
        let out = install_site("setup-secret", on_k3s(&["k8s-site"]));
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        out
    });
    assert_eq!(out.code, 0, "{}", out.stderr);
    let seen = seen.into_inner().unwrap();
    assert!(
        seen.contains("speedyspec-run-setup-secret"),
        "the runner Job was never seen"
    );
    assert!(
        !seen.contains("hunter2-secret"),
        "the secret is in the runner's spec or logs"
    );
    assert!(out.stderr.contains("requests left: 0"), "{}", out.stderr);
}
