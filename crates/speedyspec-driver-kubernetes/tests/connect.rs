//! Connecting to a cluster: only ever the fake one on loopback.

use std::io::Cursor;
use std::sync::{Mutex, MutexGuard};

use speedyspec_driver_kubernetes::{ConnectError, ConnectFlags, Connection, connect};
use speedyspec_testkit::FakeCluster;

/// `KUBECONFIG` is process-wide, so the tests that depend on it take turns. Every test points it
/// away from the user's own configuration before doing anything.
static ENVIRONMENT: Mutex<()> = Mutex::new(());

fn no_kubeconfig() -> MutexGuard<'static, ()> {
    let guard = ENVIRONMENT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // SAFETY: every test that reads or writes the environment holds ENVIRONMENT.
    unsafe { std::env::set_var("KUBECONFIG", "/nonexistent/speedyspec-test-kubeconfig") };
    guard
}

fn flags<'a>() -> ConnectFlags<'a> {
    ConnectFlags {
        server: None,
        token_file: None,
        ca_cert: None,
        insecure_skip_tls_verify: false,
        context: None,
    }
}

fn open(
    flags: &ConnectFlags<'_>,
    env_token: Option<&str>,
    stdin: &str,
) -> Result<Connection, ConnectError> {
    connect(
        flags,
        env_token,
        &mut Cursor::new(stdin.as_bytes().to_vec()),
    )
}

fn usage(result: Result<Connection, ConnectError>) -> String {
    match result {
        Err(ConnectError::Usage(message)) => message,
        Err(ConnectError::Failure(message)) => {
            panic!("expected a usage error, got failure: {message}")
        }
        Ok(_) => panic!("expected a usage error, got a connection"),
    }
}

#[test]
fn reads_the_server_version() {
    let _env = no_kubeconfig();
    let cluster = FakeCluster::start();
    let url = cluster.url();
    let connection = open(
        &ConnectFlags {
            server: Some(&url),
            ..flags()
        },
        Some(cluster.token()),
        "",
    )
    .unwrap_or_else(|_| panic!("could not connect"));
    assert_eq!(connection.server_version().unwrap(), "v1.34.1");
}

#[test]
fn token_file_beats_the_environment() {
    let _env = no_kubeconfig();
    let cluster = FakeCluster::start();
    let url = cluster.url();
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), format!("{}\n", cluster.token())).unwrap();
    let path = file.path().to_str().unwrap().to_owned();
    let connection = open(
        &ConnectFlags {
            server: Some(&url),
            token_file: Some(&path),
            ..flags()
        },
        Some("the-wrong-token"),
        "",
    )
    .unwrap_or_else(|_| panic!("could not connect"));
    assert!(connection.server_version().is_ok());
}

#[test]
fn token_from_stdin() {
    let _env = no_kubeconfig();
    let cluster = FakeCluster::start();
    let url = cluster.url();
    let connection = open(
        &ConnectFlags {
            server: Some(&url),
            token_file: Some("-"),
            ..flags()
        },
        None,
        &format!("  {}\n", cluster.token()),
    )
    .unwrap_or_else(|_| panic!("could not connect"));
    assert!(connection.server_version().is_ok());
}

#[test]
fn empty_token_is_usage_error() {
    let _env = no_kubeconfig();
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "\n").unwrap();
    let path = file.path().to_str().unwrap().to_owned();
    let message = usage(open(
        &ConnectFlags {
            server: Some("https://cluster.test:6443"),
            token_file: Some(&path),
            ..flags()
        },
        None,
        "",
    ));
    assert!(message.contains("empty"), "{message}");

    let message = usage(open(
        &ConnectFlags {
            server: Some("https://cluster.test:6443"),
            token_file: Some("-"),
            ..flags()
        },
        None,
        "",
    ));
    assert!(message.contains("empty"), "{message}");
}

#[test]
fn missing_token_file_is_usage_error() {
    let _env = no_kubeconfig();
    let message = usage(open(
        &ConnectFlags {
            server: Some("https://cluster.test:6443"),
            token_file: Some("/nonexistent/token"),
            ..flags()
        },
        None,
        "",
    ));
    assert!(message.contains("/nonexistent/token"), "{message}");
}

#[test]
fn server_without_token_is_usage_error() {
    let _env = no_kubeconfig();
    let message = usage(open(
        &ConnectFlags {
            server: Some("https://cluster.test:6443"),
            ..flags()
        },
        None,
        "",
    ));
    assert!(
        message.contains("SPEEDYSPEC_KUBE_TOKEN") && message.contains("--token-file"),
        "{message}"
    );
}

#[test]
fn token_without_server_is_usage_error() {
    let _env = no_kubeconfig();
    let message = usage(open(&flags(), Some("a-token"), ""));
    assert!(message.contains("--server"), "{message}");
    assert!(!message.contains("a-token"), "{message}");
}

#[test]
fn plain_http_only_to_loopback() {
    let _env = no_kubeconfig();
    let message = usage(open(
        &ConnectFlags {
            server: Some("http://example.com:6443"),
            ..flags()
        },
        Some("a-token"),
        "",
    ));
    assert!(message.contains("https://"), "{message}");
    // An https server is accepted by the rule; nothing is contacted until a request is made.
    assert!(
        open(
            &ConnectFlags {
                server: Some("https://cluster.test:6443"),
                ..flags()
            },
            Some("a-token"),
            "",
        )
        .is_ok()
    );
}

#[test]
fn ca_cert_and_insecure_together_is_usage_error() {
    let _env = no_kubeconfig();
    let message = usage(open(
        &ConnectFlags {
            server: Some("https://cluster.test:6443"),
            ca_cert: Some("/some/ca.pem"),
            insecure_skip_tls_verify: true,
            ..flags()
        },
        Some("a-token"),
        "",
    ));
    assert!(
        message.contains("--ca-cert") && message.contains("--insecure-skip-tls-verify"),
        "{message}"
    );
}

#[test]
fn no_kubeconfig_and_no_server_is_usage_error() {
    let _env = no_kubeconfig();
    let message = usage(open(&flags(), None, ""));
    assert!(message.contains("--server"), "{message}");
}

#[test]
fn wrong_token_is_reported_without_the_token() {
    let _env = no_kubeconfig();
    let cluster = FakeCluster::start();
    let url = cluster.url();
    let connection = open(
        &ConnectFlags {
            server: Some(&url),
            ..flags()
        },
        Some("s3cr3t-wrong"),
        "",
    )
    .unwrap_or_else(|_| panic!("building the client must not fail"));
    let message = connection.server_version().unwrap_err();
    assert!(message.to_lowercase().contains("unauthorized"), "{message}");
    assert!(!message.contains("s3cr3t-wrong"), "{message}");
}

#[test]
fn uses_a_kubeconfig_when_nothing_else_is_given() {
    let _env = no_kubeconfig();
    let cluster = FakeCluster::start();
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        file.path(),
        format!(
            "apiVersion: v1\nkind: Config\ncurrent-context: fake\n\
             clusters:\n- name: fake\n  cluster:\n    server: {}\n\
             users:\n- name: tester\n  user:\n    token: {}\n\
             contexts:\n- name: fake\n  context:\n    cluster: fake\n    user: tester\n",
            cluster.url(),
            cluster.token()
        ),
    )
    .unwrap();
    // SAFETY: ENVIRONMENT is held.
    unsafe { std::env::set_var("KUBECONFIG", file.path()) };
    let connection = open(&flags(), None, "").unwrap_or_else(|_| panic!("could not connect"));
    assert_eq!(connection.server_version().unwrap(), "v1.34.1");

    // A context that does not exist is a usage error, not a fallback to another one.
    let message = usage(open(
        &ConnectFlags {
            context: Some("nope"),
            ..flags()
        },
        None,
        "",
    ));
    assert!(message.contains("nope"), "{message}");
}
