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
        "--driver",
        "kubernetes",
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

/// A MySQL 8.4 Service `mysql` in the test namespace, with database `site`; gives its root password.
fn mysql_in_cluster() -> String {
    let password = "e2e-db-pass-7c1f";
    kubectl(&["create", "namespace", E2E_NAMESPACE]);
    kubectl(&[
        "delete",
        "secret",
        "-n",
        E2E_NAMESPACE,
        "mysql",
        "--ignore-not-found",
    ]);
    kubectl(&[
        "create",
        "secret",
        "generic",
        "-n",
        E2E_NAMESPACE,
        "mysql",
        &format!("--from-literal=password={password}"),
    ]);
    let manifest = r#"
apiVersion: apps/v1
kind: Deployment
metadata: {name: mysql, namespace: speedyspec-cli-e2e}
spec:
  replicas: 1
  selector: {matchLabels: {app: mysql}}
  template:
    metadata: {labels: {app: mysql}}
    spec:
      containers:
        - name: mysql
          image: mysql:8.4
          env:
            - {name: MYSQL_DATABASE, value: site}
            - name: MYSQL_ROOT_PASSWORD
              valueFrom: {secretKeyRef: {name: mysql, key: password}}
          readinessProbe:
            exec: {command: [sh, -c, 'mysql -uroot -p"$MYSQL_ROOT_PASSWORD" -h127.0.0.1 -e "SELECT 1"']}
            periodSeconds: 2
---
apiVersion: v1
kind: Service
metadata: {name: mysql, namespace: speedyspec-cli-e2e}
spec: {selector: {app: mysql}, ports: [{port: 3306}]}
"#;
    let mut apply = std::process::Command::new("docker")
        .args([
            "exec",
            "-i",
            "speedyspec-k3s",
            "kubectl",
            "apply",
            "-f",
            "-",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    use std::io::Write;
    apply
        .stdin
        .take()
        .unwrap()
        .write_all(manifest.as_bytes())
        .unwrap();
    assert!(apply.wait().unwrap().success());
    kubectl(&[
        "rollout",
        "status",
        "-n",
        E2E_NAMESPACE,
        "deploy/mysql",
        "--timeout=300s",
    ]);
    password.to_owned()
}

/// The site installed (its setup done), and its database's password.
fn site_with_database(name: &str) -> String {
    let password = mysql_in_cluster();
    if e2e_record(name)["status"] != "installed" {
        fresh_site(name);
        let out = install_site(name, on_k3s(&["k8s-site"]));
        assert_eq!(out.code, 0, "{}", out.stderr);
    }
    password
}

fn db(name: &str, password: &str, args: &[&str]) -> Run {
    let binary = needed("SPEEDYSPEC_E2E_RUNNER_BINARY");
    let mut all = vec!["--no-interaction", "app", "db"];
    all.extend_from_slice(args);
    all.extend_from_slice(&[
        "--install",
        name,
        "--namespace",
        E2E_NAMESPACE,
        "--runner-binary",
        &binary,
    ]);
    on_k3s(&["k8s-site"])
        .env("K8S_SITE_DB_PASSWORD", password)
        .run(&all)
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up and SPEEDYSPEC_E2E_RUNNER_BINARY"]
fn query_prints_rows() {
    let password = site_with_database("db-site");
    let out = db("db-site", &password, &["query", "SELECT 6 * 7 AS answer"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "answer\n42\n");
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up and SPEEDYSPEC_E2E_RUNNER_BINARY"]
fn export_and_import_round_trip_through_the_cluster() {
    let password = site_with_database("db-site");
    for sql in [
        "DROP TABLE IF EXISTS wp_round",
        "CREATE TABLE wp_round (id INT PRIMARY KEY, body TEXT, raw BLOB)",
        "INSERT INTO wp_round VALUES (1, 'héllo ☃', X'00FF10'), (2, 'it''s', NULL)",
    ] {
        assert_eq!(db("db-site", &password, &["query", sql]).code, 0);
    }
    let checksum = || db("db-site", &password, &["query", "CHECKSUM TABLE wp_round"]).stdout;
    let before = checksum();
    let dir = tempfile::tempdir().unwrap();
    let dump = dir.path().join("dump.sql");
    let out = db(
        "db-site",
        &password,
        &["export", "--to", dump.to_str().unwrap()],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(std::fs::read_to_string(&dump).unwrap().contains("wp_round"));
    assert_eq!(
        db("db-site", &password, &["query", "DROP TABLE wp_round"]).code,
        0
    );
    let out = db(
        "db-site",
        &password,
        &["import", dump.to_str().unwrap(), "--yes"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stderr.contains("imported"), "{}", out.stderr);
    assert_eq!(checksum(), before);
}

/// The largest resident size of any child this test process has waited for, in bytes.
fn peak_child_memory() -> u64 {
    // SAFETY: getrusage only writes into the struct it is given.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage) };
    let peak = usage.ru_maxrss as u64;
    if cfg!(target_os = "macos") {
        peak
    } else {
        peak * 1024
    }
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up and SPEEDYSPEC_E2E_RUNNER_BINARY"]
fn a_50000_row_export_streams() {
    let password = site_with_database("db-site");
    for sql in [
        "DROP TABLE IF EXISTS wp_big",
        "CREATE TABLE wp_big (id INT PRIMARY KEY, body TEXT)",
    ] {
        assert_eq!(db("db-site", &password, &["query", sql]).code, 0);
    }
    // 50 000 rows of 2 000 bytes, from five tables of the digits 0 to 9 (each query is a session of its own, so no
    // recursion setting would carry over).
    let digits = "(SELECT 0 i UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL \
                  SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9)";
    let fill = format!(
        "INSERT INTO wp_big SELECT a.i * 10000 + b.i * 1000 + c.i * 100 + d.i * 10 + e.i, REPEAT('x', 2000) \
         FROM {digits} a, {digits} b, {digits} c, {digits} d, {digits} e WHERE a.i < 5"
    );
    let dir = tempfile::tempdir().unwrap();
    // The same export while the table is empty first: the most any child (this CLI among them) needs without the data.
    let small = dir.path().join("small.sql");
    let out = db(
        "db-site",
        &password,
        &["export", "--to", small.to_str().unwrap()],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    let baseline = peak_child_memory();
    let out = db("db-site", &password, &["query", &fill]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let dump = dir.path().join("big.sql");
    let out = db(
        "db-site",
        &password,
        &["export", "--to", dump.to_str().unwrap(), "--force"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    let size = std::fs::metadata(&dump).unwrap().len();
    assert!(size > 100 * 1000 * 1000, "the dump is only {size} bytes");
    let peak = peak_child_memory();
    assert!(
        peak < baseline + 32 * 1024 * 1024,
        "a {size}-byte dump took a child from {baseline} to {peak} bytes"
    );
    assert_eq!(
        db("db-site", &password, &["query", "DROP TABLE wp_big"]).code,
        0
    );
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up and SPEEDYSPEC_E2E_RUNNER_BINARY"]
fn the_database_password_is_nowhere_in_the_cluster() {
    let password = site_with_database("db-site");
    let done = std::sync::atomic::AtomicBool::new(false);
    let seen = std::sync::Mutex::new(String::new());
    let out = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                let selector = "speedyspec.install=db-site";
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
                std::thread::sleep(Duration::from_millis(300));
            }
        });
        let out = db("db-site", &password, &["query", "SELECT SLEEP(3) AS slept"]);
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        out
    });
    assert_eq!(out.code, 0, "{}", out.stderr);
    let seen = seen.into_inner().unwrap();
    assert!(
        seen.contains("speedyspec-run-db-site"),
        "the runner Job was never seen"
    );
    assert!(
        !seen.contains(&password),
        "the password is in the runner's spec or logs"
    );
    assert!(!out.stdout.contains(&password) && !out.stderr.contains(&password));
}

fn plugin_zip(name: &str, version: &str) -> Vec<u8> {
    use speedyspec_testkit::{Entry, build_zip};
    build_zip(&[
        ("akismet/", Entry::Dir),
        (
            "akismet/info.txt",
            Entry::file(&format!("Name: {name}\nVersion: {version}\n")),
        ),
        ("akismet/main.php", Entry::file("<?php\n")),
    ])
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up and SPEEDYSPEC_E2E_RUNNER_BINARY"]
fn plugins_are_installed_listed_updated_and_deleted_on_kubernetes() {
    use speedyspec_testkit::{Route, Server};
    fresh_site("ext-site");
    let out = install_site("ext-site", on_k3s(&["k8s-site"]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    let server = Server::start(vec![
        (
            "/plugin/akismet.1.0.zip",
            Route::ok(plugin_zip("Akismet", "1.0")),
        ),
        (
            "/plugin/akismet.2.0.zip",
            Route::ok(plugin_zip("Akismet", "2.0")),
        ),
    ]);
    let binary = needed("SPEEDYSPEC_E2E_RUNNER_BINARY");
    let plugin = |args: &[&str]| {
        let mut all = vec!["--no-interaction", "plugin"];
        all.extend_from_slice(args);
        all.extend_from_slice(&[
            "--install",
            "ext-site",
            "--namespace",
            E2E_NAMESPACE,
            "--runner-binary",
            &binary,
        ]);
        on_k3s(&["k8s-site", "ext-source"])
            .env("EXT_SOURCE_URL", server.url())
            .run(&all)
    };
    let info = || {
        in_site(
            "ext-site",
            &["cat", "/srv/app/ext/plugins/akismet/info.txt"],
        )
    };

    let out = plugin(&["install", "akismet@1.0"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(info().contains("Version: 1.0"), "{}", info());
    let out = plugin(&["list"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "akismet\tAkismet\t1.0\n");
    let out = plugin(&["update", "akismet"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("1.0 -> 2.0"), "{}", out.stdout);
    assert!(info().contains("Version: 2.0"), "{}", info());
    assert!(
        !in_site("ext-site", &["ls", "-a", "/srv/app"]).contains(".maintenance"),
        "left in maintenance"
    );
    let entries = in_site("ext-site", &["ls", "-a", "/srv/app/ext/plugins"]);
    assert!(
        !entries.contains(".speedyspec-"),
        "work left behind: {entries}"
    );
    let out = plugin(&["delete", "akismet", "--yes"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(info().is_empty());
    assert!(
        kubectl(&[
            "get",
            "jobs",
            "-n",
            E2E_NAMESPACE,
            "-l",
            "speedyspec.install=ext-site",
            "-o",
            "name"
        ])
        .is_empty()
    );
}

/// The site's tables, as an app would have them: options with its URL, and a row that holds the URL as well.
fn site_tables(run: &dyn Fn(&str) -> Run, url: &str) {
    for sql in [
        "DROP TABLE IF EXISTS wp_options".to_owned(),
        "CREATE TABLE wp_options (option_name VARCHAR(191) PRIMARY KEY, option_value LONGTEXT)"
            .to_owned(),
        format!("INSERT INTO wp_options VALUES ('siteurl', '{url}'), ('home_link', '{url}/home')"),
    ] {
        let out = run(&sql);
        assert_eq!(out.code, 0, "{sql}: {}", out.stderr);
    }
}

fn option(run: &dyn Fn(&str) -> Run, name: &str) -> String {
    let out = run(&format!(
        "SELECT option_value FROM wp_options WHERE option_name = '{name}'"
    ));
    out.stdout.lines().nth(1).unwrap_or_default().to_owned()
}

fn on_kubernetes(name: &str, password: &str, args: &[&str]) -> Run {
    let binary = needed("SPEEDYSPEC_E2E_RUNNER_BINARY");
    let mut all = vec!["--no-interaction", "app"];
    all.extend_from_slice(args);
    all.extend_from_slice(&[
        "--install",
        name,
        "--namespace",
        E2E_NAMESPACE,
        "--runner-binary",
        &binary,
    ]);
    on_k3s(&["k8s-site"])
        .env("K8S_SITE_DB_PASSWORD", password)
        .run(&all)
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up and SPEEDYSPEC_E2E_RUNNER_BINARY"]
fn backup_streams_out_and_restores_in() {
    let password = site_with_database("db-site");
    let query = |sql: &str| on_kubernetes("db-site", &password, &["db", "query", sql]);
    site_tables(&query, "https://k8s.example");
    in_site("db-site", &["sh", "-c", "echo kept > /srv/app/page.txt"]);
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("site.tar.gz");
    let out = on_kubernetes(
        "db-site",
        &password,
        &["backup", "--to", archive.to_str().unwrap()],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&archive).unwrap().permissions().mode() & 0o777,
        0o600
    );

    assert_eq!(
        query("DELETE FROM wp_options WHERE option_name = 'home_link'").code,
        0
    );
    in_site("db-site", &["rm", "/srv/app/page.txt"]);
    let out = on_kubernetes(
        "db-site",
        &password,
        &["restore", archive.to_str().unwrap(), "--yes"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(option(&query, "home_link"), "https://k8s.example/home");
    assert_eq!(in_site("db-site", &["cat", "/srv/app/page.txt"]), "kept\n");
    assert!(!in_site("db-site", &["ls", "-a", "/srv/app"]).contains(".maintenance"));
    assert!(
        kubectl(&[
            "get",
            "jobs",
            "-n",
            E2E_NAMESPACE,
            "-l",
            "speedyspec.install=db-site",
            "-o",
            "name"
        ])
        .is_empty()
    );
}

/// A zip install of the site on this machine, with its database in the throwaway MySQL from `scripts/mysql.sh`
/// (`SPEEDYSPEC_E2E_MYSQL`); the home that keeps its record, and the URL of its database.
fn zip_site() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    String,
    speedyspec_testkit::Server,
) {
    use speedyspec_testkit::{Entry, Route, Server, build_zip};
    let mysql = needed("SPEEDYSPEC_E2E_MYSQL");
    let admin = std::process::Command::new("docker")
        .args(["exec", "speedyspec-mysql", "sh", "-c",
               "mysql -uroot -p\"$MYSQL_ROOT_PASSWORD\" -e 'DROP DATABASE IF EXISTS site_zip; CREATE DATABASE site_zip'"])
        .output()
        .unwrap();
    assert!(
        admin.status.success(),
        "{}",
        String::from_utf8_lossy(&admin.stderr)
    );
    let server = Server::start(vec![(
        "/site.zip",
        Route::ok(build_zip(&[
            ("site/", Entry::Dir),
            ("site/index.html", Entry::file("<html>zip</html>")),
        ])),
    )]);
    let (home, base) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let out = zip_cli(&home, &server, &mysql).run(&[
        "--no-interaction",
        "app",
        "install",
        "site",
        "--driver",
        "zip",
        "--name",
        "zip-site",
        "--base",
        base.path().to_str().unwrap(),
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    (home, base, mysql, server)
}

fn zip_cli(
    home: &tempfile::TempDir,
    server: &speedyspec_testkit::Server,
    mysql: &str,
) -> Speedyspec {
    Speedyspec::with_home(&["k8s-site"], home.path())
        .env("K8S_SITE_ZIP_URL", server.url())
        .env("K8S_SITE_ZIP_DB", mysql)
}

#[test]
#[ignore = "needs scripts/k3s.sh up, scripts/mysql.sh up and SPEEDYSPEC_E2E_RUNNER_BINARY"]
fn a_zip_backup_restores_onto_kubernetes_with_a_move() {
    let password = site_with_database("db-site");
    let (home, base, mysql, server) = zip_site();
    let zip_query = |sql: &str| {
        zip_cli(&home, &server, &mysql).run(&[
            "--no-interaction",
            "app",
            "db",
            "query",
            sql,
            "--install",
            "zip-site",
        ])
    };
    site_tables(&zip_query, "https://zip.example");
    std::fs::write(base.path().join("from-zip.txt"), "zip file\n").unwrap();
    let archive = home.path().join("zip.tar.gz");
    let out = zip_cli(&home, &server, &mysql).run(&[
        "--no-interaction",
        "app",
        "backup",
        "--install",
        "zip-site",
        "--to",
        archive.to_str().unwrap(),
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);

    let out = on_kubernetes(
        "db-site",
        &password,
        &[
            "restore",
            archive.to_str().unwrap(),
            "--url",
            "https://k8s.example",
            "--yes",
        ],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    let query = |sql: &str| on_kubernetes("db-site", &password, &["db", "query", sql]);
    assert_eq!(option(&query, "siteurl"), "https://k8s.example");
    assert_eq!(option(&query, "home_link"), "https://k8s.example/home");
    assert_eq!(
        in_site("db-site", &["cat", "/srv/app/from-zip.txt"]),
        "zip file\n"
    );
    assert_eq!(
        in_site("db-site", &["cat", "/srv/app/index.html"]),
        "<html>zip</html>"
    );
}

#[test]
#[ignore = "needs scripts/k3s.sh up, scripts/mysql.sh up and SPEEDYSPEC_E2E_RUNNER_BINARY"]
fn a_kubernetes_backup_restores_onto_a_zip_install() {
    let password = site_with_database("db-site");
    let query = |sql: &str| on_kubernetes("db-site", &password, &["db", "query", sql]);
    site_tables(&query, "https://k8s.example");
    in_site(
        "db-site",
        &["sh", "-c", "echo from k8s > /srv/app/from-k8s.txt"],
    );
    let (home, base, mysql, server) = zip_site();
    let archive = home.path().join("k8s.tar.gz");
    let out = on_kubernetes(
        "db-site",
        &password,
        &["backup", "--to", archive.to_str().unwrap()],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);

    let out = zip_cli(&home, &server, &mysql).run(&[
        "--no-interaction",
        "app",
        "restore",
        archive.to_str().unwrap(),
        "--install",
        "zip-site",
        "--url",
        "https://zip.example",
        "--yes",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        std::fs::read_to_string(base.path().join("from-k8s.txt")).unwrap(),
        "from k8s\n"
    );
    let zip_query = |sql: &str| {
        zip_cli(&home, &server, &mysql).run(&[
            "--no-interaction",
            "app",
            "db",
            "query",
            sql,
            "--install",
            "zip-site",
        ])
    };
    assert_eq!(option(&zip_query, "home_link"), "https://zip.example/home");
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up and SPEEDYSPEC_E2E_RUNNER_BINARY"]
fn a_big_site_goes_out_and_in_in_flat_memory() {
    let password = site_with_database("db-site");
    let query = |sql: &str| on_kubernetes("db-site", &password, &["db", "query", sql]);
    site_tables(&query, "https://k8s.example");
    let dir = tempfile::tempdir().unwrap();
    // The same backup and restore of the small site first: the most any child needs without the data.
    let small = dir.path().join("small.tar.gz");
    let out = on_kubernetes(
        "db-site",
        &password,
        &["backup", "--to", small.to_str().unwrap()],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    let out = on_kubernetes(
        "db-site",
        &password,
        &["restore", small.to_str().unwrap(), "--yes"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    let baseline = peak_child_memory();
    // 20 MB of files that do not compress.
    in_site(
        "db-site",
        &[
            "sh",
            "-c",
            "head -c 20000000 /dev/urandom > /srv/app/big.bin",
        ],
    );
    let archive = dir.path().join("big.tar.gz");
    let out = on_kubernetes(
        "db-site",
        &password,
        &["backup", "--to", archive.to_str().unwrap()],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(std::fs::metadata(&archive).unwrap().len() > 20_000_000);
    in_site("db-site", &["rm", "/srv/app/big.bin"]);
    let out = on_kubernetes(
        "db-site",
        &password,
        &["restore", archive.to_str().unwrap(), "--yes"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(in_site("db-site", &["ls", "-l", "/srv/app/big.bin"]).contains("20000000"));
    let peak = peak_child_memory();
    assert!(
        peak < baseline + 16 * 1024 * 1024,
        "20 MB of files took a child from {baseline} to {peak} bytes"
    );
    in_site("db-site", &["rm", "/srv/app/big.bin"]);
}
