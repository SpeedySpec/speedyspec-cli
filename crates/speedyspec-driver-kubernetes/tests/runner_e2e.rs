//! The runner pod against the throwaway k3s that `scripts/k3s.sh up` starts, never anything else:
//!
//!   SPEEDYSPEC_E2E_KUBECONFIG="$(scripts/k3s.sh kubeconfig)" \
//!     cargo test -p speedyspec-driver-kubernetes --test runner_e2e -- --ignored --test-threads=1

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;
use sha2::{Digest, Sha256};
use speedyspec_driver::{Workload, WorkloadKind};
use speedyspec_driver_kubernetes::{
    ConnectFlags, Connection, ResourceId, RunnerPod, RunnerSpec, connect, place_runner,
};

const NAMESPACE: &str = "speedyspec-runner-e2e";
const IMAGE: &str =
    "busybox@sha256:a7dbc319b1438115a207e11930dc4bb3e643dcb3ec0bdd694bb36cd8fad289ec";

fn kubeconfig() -> String {
    std::env::var("SPEEDYSPEC_E2E_KUBECONFIG").expect(
        "set SPEEDYSPEC_E2E_KUBECONFIG to the kubeconfig `scripts/k3s.sh up` wrote; this test never uses your own \
         cluster",
    )
}

fn connection() -> Connection {
    // SAFETY: the tests in this file run one at a time (`--test-threads=1`) and all set the same value.
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

fn id(api_version: &str, kind: &str, name: &str) -> ResourceId {
    ResourceId {
        api_version: api_version.to_owned(),
        kind: kind.to_owned(),
        namespace: Some(NAMESPACE.to_owned()),
        name: name.to_owned(),
    }
}

/// The app: a Deployment whose pod writes a file into its claim at start, then sleeps.
fn app(connection: &Connection) -> Workload {
    connection
        .apply(
            &json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": NAMESPACE}}),
            true,
        )
        .unwrap();
    connection
        .apply(
            &json!({"apiVersion": "v1", "kind": "PersistentVolumeClaim",
                    "metadata": {"name": "data", "namespace": NAMESPACE},
                    "spec": {"accessModes": ["ReadWriteOnce"], "resources": {"requests": {"storage": "64Mi"}}}}),
            true,
        )
        .unwrap();
    connection
        .apply(
            &json!({"apiVersion": "apps/v1", "kind": "Deployment", "metadata": {"name": "app", "namespace": NAMESPACE},
                    "spec": {"replicas": 1, "selector": {"matchLabels": {"app": "runner-e2e"}},
                             "template": {"metadata": {"labels": {"app": "runner-e2e"}},
                                          "spec": {"terminationGracePeriodSeconds": 0, "containers": [{
                                              "name": "app", "image": IMAGE,
                                              "command": ["sh", "-c", "echo hello-from-app > /srv/app/hello; sleep 3600"],
                                              "readinessProbe": {"exec": {"command": ["test", "-f", "/srv/app/hello"]},
                                                                 "periodSeconds": 1},
                                              "volumeMounts": [{"name": "data", "mountPath": "/srv/app"}]}],
                                                   "volumes": [{"name": "data", "persistentVolumeClaim": {"claimName": "data"}}]}}}}),
            true,
        )
        .unwrap();
    let workload = Workload {
        kind: WorkloadKind::Deployment,
        name: "app".into(),
        root: "/srv/app".into(),
        container: None,
    };
    let started = Instant::now();
    loop {
        match place_runner(connection, NAMESPACE, &workload) {
            Ok(_) => return workload,
            Err(error) if started.elapsed() > Duration::from_secs(240) => {
                panic!("the app never got ready: {}", error.0)
            }
            Err(_) => std::thread::sleep(Duration::from_secs(1)),
        }
    }
}

fn spec(install: &str, deadline: Duration) -> RunnerSpec {
    RunnerSpec {
        image: IMAGE.into(),
        deadline,
        start_wait: Duration::from_secs(180),
        install: install.into(),
    }
}

/// A stand-in for the CLI's binary that busybox can run.
fn script(dir: &Path) -> (std::path::PathBuf, String) {
    let path = dir.join("speedyspec");
    let body = b"#!/bin/sh\necho \"runner $*\"\n";
    std::fs::write(&path, body).unwrap();
    let digest: String = Sha256::digest(body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    (path, digest)
}

/// The runner Jobs of `install` the throwaway cluster still has, as its own kubectl lists them.
fn jobs(install: &str) -> Vec<String> {
    let output = Command::new("docker")
        .args([
            "exec",
            "speedyspec-k3s",
            "kubectl",
            "get",
            "jobs",
            "-n",
            NAMESPACE,
            "-o",
            "name",
            "-l",
        ])
        .arg(format!("speedyspec.install={install}"))
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

fn wait_until_gone(install: &str, within: Duration) {
    let started = Instant::now();
    while !jobs(install).is_empty() {
        assert!(
            started.elapsed() < within,
            "the runner Job of {install} is still there: {:?}",
            jobs(install)
        );
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up"]
fn a_runner_pod_sees_the_app_volume() {
    let connection = connection();
    let workload = app(&connection);
    let placement = place_runner(&connection, NAMESPACE, &workload).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (binary, digest) = script(dir.path());
    {
        let runner = RunnerPod::start(
            &connection,
            NAMESPACE,
            &placement,
            &binary,
            &digest,
            &spec("sees", Duration::from_secs(300)),
        )
        .unwrap_or_else(|e| panic!("{}", e.0));
        let mut out = Vec::new();
        assert_eq!(
            runner
                .command(
                    &["cat", "/srv/app/hello"],
                    None,
                    &mut out,
                    &mut std::io::sink()
                )
                .unwrap(),
            0
        );
        assert_eq!(out, b"hello-from-app\n");
        let mut out = Vec::new();
        assert_eq!(
            runner
                .run(&["self", "x"], None, &mut out, &mut std::io::sink())
                .unwrap(),
            0
        );
        assert_eq!(out, b"runner self x\n");
        let mut out = Vec::new();
        runner
            .command(
                &["stat", "-c", "%a", "/speedyspec"],
                None,
                &mut out,
                &mut std::io::sink(),
            )
            .unwrap();
        assert_eq!(out, b"700\n");
        runner
            .put("/speedyspec/request.json", &mut &b"{}"[..], 0o600)
            .unwrap();
        let mut out = Vec::new();
        runner
            .command(
                &["stat", "-c", "%a", "/speedyspec/request.json"],
                None,
                &mut out,
                &mut std::io::sink(),
            )
            .unwrap();
        assert_eq!(out, b"600\n");
    }
    wait_until_gone("sees", Duration::from_secs(60));
    // The app's pod is untouched.
    assert!(connection.get(&id("apps/v1", "Deployment", "app")).is_ok());
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up"]
fn the_binary_is_checked_in_the_pod() {
    let connection = connection();
    let workload = app(&connection);
    let placement = place_runner(&connection, NAMESPACE, &workload).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (binary, _) = script(dir.path());
    let tampered = "0".repeat(64);
    let error = match RunnerPod::start(
        &connection,
        NAMESPACE,
        &placement,
        &binary,
        &tampered,
        &spec("checked", Duration::from_secs(300)),
    ) {
        Ok(_) => panic!("a binary that does not match its checksum was used"),
        Err(error) => error.0,
    };
    assert!(error.contains("does not match its checksum"), "{error}");
    wait_until_gone("checked", Duration::from_secs(60));
}

/// Not a test of its own: the child process `ctrl_c_deletes_the_job` and `a_killed_cli_leaves_a_job_that_expires`
/// start. It holds a runner pod until it is stopped.
#[test]
#[ignore = "started by the other tests"]
fn hold_a_runner_pod() {
    let Ok(install) = std::env::var("SPEEDYSPEC_E2E_HOLD") else {
        return;
    };
    // As at a terminal: a process started in the background of a script has Ctrl-C ignored.
    // SAFETY: sets the default disposition for SIGINT before any guard is installed.
    unsafe { libc::signal(libc::SIGINT, libc::SIG_DFL) };
    let deadline = Duration::from_secs(
        std::env::var("SPEEDYSPEC_E2E_HOLD_DEADLINE")
            .unwrap()
            .parse()
            .unwrap(),
    );
    let connection = connection();
    let workload = app(&connection);
    let placement = place_runner(&connection, NAMESPACE, &workload).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (binary, digest) = script(dir.path());
    let runner = RunnerPod::start(
        &connection,
        NAMESPACE,
        &placement,
        &binary,
        &digest,
        &spec(&install, deadline),
    )
    .unwrap_or_else(|e| panic!("{}", e.0));
    println!("held {}", runner.pod());
    // A long command, as a dump or a restore would be.
    let _ = runner.command(
        &["sleep", "600"],
        None,
        &mut std::io::sink(),
        &mut std::io::sink(),
    );
}

/// Starts `hold_a_runner_pod` for `install` and waits until it holds its pod.
fn holder(install: &str, deadline: u64) -> std::process::Child {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "hold_a_runner_pod",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("SPEEDYSPEC_E2E_HOLD", install)
        .env("SPEEDYSPEC_E2E_HOLD_DEADLINE", deadline.to_string())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    loop {
        match lines.next() {
            // libtest prints the test's name first on the same line.
            Some(Ok(line)) if line.contains("held ") => break,
            Some(_) => {}
            None => panic!("the holder ended before it held a runner pod"),
        }
    }
    std::thread::spawn(move || lines.for_each(drop));
    child
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up"]
fn ctrl_c_deletes_the_job() {
    let mut child = holder("ctrlc", 300);
    assert_eq!(jobs("ctrlc").len(), 1);
    // SAFETY: signals the child this test started.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
    let status = child.wait().unwrap();
    assert!(!status.success());
    wait_until_gone("ctrlc", Duration::from_secs(30));
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up"]
fn a_killed_cli_leaves_a_job_that_expires() {
    let mut child = holder("killed", 20);
    child.kill().unwrap();
    child.wait().unwrap();
    let left = jobs("killed");
    assert_eq!(left.len(), 1, "{left:?}");
    // Its deadline stops the pod, and its time-to-live then removes the Job.
    wait_until_gone("killed", Duration::from_secs(240));
}
