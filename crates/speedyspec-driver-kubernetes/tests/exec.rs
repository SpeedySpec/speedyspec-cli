//! Exec through the API against the throwaway k3s that `scripts/k3s.sh up` starts, never anything else:
//!
//!   SPEEDYSPEC_E2E_KUBECONFIG="$(scripts/k3s.sh kubeconfig)" \
//!     cargo test -p speedyspec-driver-kubernetes --test exec -- --ignored --test-threads=1

use std::io::Read;
use std::time::{Duration, Instant};

use serde_json::json;
use speedyspec_driver_kubernetes::{ConnectFlags, Connection, ResourceId, connect};

const NAMESPACE: &str = "speedyspec-exec-e2e";
const IMAGE: &str = "busybox:1.37";

/// The kubeconfig of the throwaway cluster. Never the user's.
fn connection() -> Connection {
    let kubeconfig = std::env::var("SPEEDYSPEC_E2E_KUBECONFIG").expect(
        "set SPEEDYSPEC_E2E_KUBECONFIG to the kubeconfig `scripts/k3s.sh up` wrote; this test never uses your own \
         cluster",
    );
    // SAFETY: the tests in this file run one at a time (`--test-threads=1`) and all set the same value.
    unsafe { std::env::set_var("KUBECONFIG", kubeconfig) };
    let flags = ConnectFlags {
        server: None,
        token_file: None,
        ca_cert: None,
        insecure_skip_tls_verify: false,
        context: None,
    };
    connect(&flags, None, &mut std::io::empty()).unwrap_or_else(|_| panic!("could not connect"))
}

fn pod_id(name: &str) -> ResourceId {
    ResourceId {
        api_version: "v1".to_owned(),
        kind: "Pod".to_owned(),
        namespace: Some(NAMESPACE.to_owned()),
        name: name.to_owned(),
    }
}

/// A running busybox pod named `name`, made fresh.
fn pod(connection: &Connection, name: &str) {
    connection
        .apply(
            &json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": NAMESPACE}}),
            true,
        )
        .unwrap();
    let _ = connection.delete(&pod_id(name));
    let started = Instant::now();
    while connection.get(&pod_id(name)).is_ok() {
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "the old {name} did not go"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
    connection
        .apply(
            &json!({"apiVersion": "v1", "kind": "Pod",
                    "metadata": {"name": name, "namespace": NAMESPACE},
                    "spec": {"terminationGracePeriodSeconds": 0,
                             "containers": [{"name": "box", "image": IMAGE, "command": ["sleep", "3600"]}]}}),
            true,
        )
        .unwrap();
    let started = Instant::now();
    loop {
        let phase = connection
            .get(&pod_id(name))
            .ok()
            .and_then(|p| p["status"]["phase"].as_str().map(str::to_owned));
        if phase.as_deref() == Some("Running") {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(180),
            "{name} is not running: {phase:?}"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up"]
fn exec_streams_both_ways() {
    let connection = connection();
    pod(&connection, "streams");
    // 20 MB that is not all one byte, so a dropped or repeated chunk shows.
    let input: Vec<u8> = (0..20 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = connection
        .exec(
            NAMESPACE,
            "streams",
            None,
            &["cat"],
            Some(&mut input.as_slice()),
            &mut out,
            &mut err,
        )
        .unwrap();
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    assert_eq!(out.len(), input.len());
    assert!(out == input, "the bytes that came back differ");
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up"]
fn exec_gives_the_exit_status() {
    let connection = connection();
    pod(&connection, "status");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = connection
        .exec(
            NAMESPACE,
            "status",
            Some("box"),
            &["sh", "-c", "echo out; echo err >&2; exit 7"],
            None,
            &mut out,
            &mut err,
        )
        .unwrap();
    assert_eq!(code, 7);
    assert_eq!(out, b"out\n");
    assert_eq!(err, b"err\n");
    let code = connection
        .exec(
            NAMESPACE,
            "status",
            None,
            &["true"],
            None,
            &mut out,
            &mut err,
        )
        .unwrap();
    assert_eq!(code, 0);
}

/// Input that never ends, so the command is still running when its pod goes.
struct Endless;

impl Read for Endless {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::thread::sleep(Duration::from_millis(5));
        buf.fill(b'x');
        Ok(buf.len())
    }
}

#[test]
#[ignore = "needs the cluster from scripts/k3s.sh up"]
fn a_broken_stream_is_an_error() {
    let connection = connection();
    pod(&connection, "broken");
    let deleter = std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(3));
        connection_for_deleting().delete(&pod_id("broken")).unwrap();
    });
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let result = connection.exec(
        NAMESPACE,
        "broken",
        None,
        &["cat"],
        Some(&mut Endless),
        &mut out,
        &mut err,
    );
    deleter.join().unwrap();
    let error = result.expect_err("a stream whose pod was deleted ended as a success");
    let message = error.message("exec");
    assert!(
        message.contains("broken") && message.contains("broke"),
        "{message}"
    );
}

fn connection_for_deleting() -> Connection {
    connection()
}
