//! `SpeedySpec\Cli\http_get`: plugins download through the CLI's own downloader, with its rules.

mod common;

use common::{Speedyspec, fixture};
use speedyspec_testkit::{Route, Server};

fn get(args: &[&str]) -> String {
    let fixture = fixture("http-get");
    let mut all = vec!["get"];
    all.extend_from_slice(args);
    let out = Speedyspec::new(&[fixture.to_str().unwrap()]).run(&all);
    assert_eq!(out.code, 0, "{}", out.stderr);
    out.stdout
}

#[test]
fn fetches_a_body() {
    let server = Server::start(vec![("/info.json", Route::ok(r#"{"version":"5.3"}"#))]);
    assert_eq!(
        get(&[&format!("{}/info.json", server.url())]),
        r#"{"version":"5.3"}"#
    );
}

#[test]
fn a_status_error_is_an_exception() {
    let server = Server::start(vec![("/missing", Route::status(404))]);
    let out = get(&[&format!("{}/missing", server.url())]);
    assert!(out.starts_with("ERR: ") && out.contains("404"), "{out}");
}

#[test]
fn a_plain_http_url_elsewhere_is_refused() {
    let out = get(&["http://example.com/info.json"]);
    assert!(
        out.starts_with("ERR: ") && out.contains("https://"),
        "{out}"
    );
}

#[test]
fn a_body_over_the_limit_fails() {
    let server = Server::start(vec![("/big", Route::ok(vec![b'x'; 2048]))]);
    let url = format!("{}/big", server.url());
    let out = get(&[&url, "1024"]);
    assert!(out.starts_with("ERR: ") && out.contains("1024"), "{out}");
    assert_eq!(get(&[&url, "4096"]).len(), 2048);
}

#[test]
fn the_runner_has_no_http_get() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("task.php");
    std::fs::write(
        &file,
        "<?php\nreturn static fn (array $input): bool => function_exists('SpeedySpec\\Cli\\http_get');\n",
    )
    .unwrap();
    let out = Speedyspec::new(&[]).run(&[
        "self",
        "task",
        "--root",
        root.path().to_str().unwrap(),
        "--file",
        file.to_str().unwrap(),
        "--no-app",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let got: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(got["result"], false, "{got}");
}
