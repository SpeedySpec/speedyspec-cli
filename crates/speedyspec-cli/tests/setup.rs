//! The setup phase of `app install`: after the files are in place, the app's plugin's setup tasks run in order
//! through the runner, and a failed setup is finished by running the same command again.

mod common;

use std::fs;
use std::path::Path;

use common::{Run, Speedyspec};
use speedyspec_testkit::{Entry, Route, Server, build_zip};
use tempfile::TempDir;

const SECRET: &str = "s3cr3t-'\"$";

fn server() -> Server {
    Server::start(vec![(
        "/web.zip",
        Route::ok(build_zip(&[
            ("site/", Entry::Dir),
            ("site/index.html", Entry::file("<html>")),
        ])),
    )])
}

fn install(home: &Path, server: &Server, base: &Path, fail_task2: bool) -> Run {
    let mut run = Speedyspec::with_home(&["setup-app"], home).env("ZIP_FIXTURE_URL", server.url());
    if fail_task2 {
        run = run.env("SETUP_FAIL_TASK2", "1");
    }
    run.run(&[
        "--no-interaction",
        "app",
        "install",
        "setup-site",
        "--db-password",
        SECRET,
        "--name",
        "blog",
        "--base",
        base.to_str().unwrap(),
    ])
}

fn record(home: &Path) -> serde_json::Value {
    let text = fs::read_to_string(home.join("installs/blog.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn order(base: &Path) -> String {
    fs::read_to_string(base.join("order.txt")).unwrap_or_default()
}

fn downloads(server: &Server) -> usize {
    server
        .requests()
        .iter()
        .filter(|r| r.path == "/web.zip")
        .count()
}

#[test]
fn setup_tasks_run_in_order_after_the_files() {
    let (home, base, server) = (TempDir::new().unwrap(), TempDir::new().unwrap(), server());
    let out = install(home.path(), &server, base.path(), false);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(base.path().join("index.html").is_file());
    assert_eq!(order(base.path()), "1 2 3");
    assert!(
        out.stderr.contains("setup: task1.php (1/3)"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("setup: task one done"),
        "{}",
        out.stderr
    );
    assert!(out.stdout.contains("after: 1"), "{}", out.stdout);
    let r = record(home.path());
    assert_eq!(r["status"], "installed");
    assert_eq!(r["setup_done"], 3);
}

#[test]
fn a_failing_task_stops_setup_and_is_named() {
    let (home, base, server) = (TempDir::new().unwrap(), TempDir::new().unwrap(), server());
    let out = install(home.path(), &server, base.path(), true);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("setup stopped at task2.php"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("task two was told to fail"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr
            .contains("run the same command again to finish it"),
        "{}",
        out.stderr
    );
    assert!(!out.stdout.contains("after:"), "{}", out.stdout);
    assert_eq!(order(base.path()), "1");
    let r = record(home.path());
    assert_eq!(r["status"], "files");
    assert_eq!(r["setup_done"], 1);
}

#[test]
fn running_again_finishes_from_the_failed_task() {
    let (home, base, server) = (TempDir::new().unwrap(), TempDir::new().unwrap(), server());
    assert_eq!(install(home.path(), &server, base.path(), true).code, 1);
    assert_eq!(downloads(&server), 1);
    let out = install(home.path(), &server, base.path(), false);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("files already in place; finishing setup"),
        "{}",
        out.stderr
    );
    assert_eq!(downloads(&server), 1, "the files were downloaded again");
    assert_eq!(order(base.path()), "1 2 3");
    assert!(out.stdout.contains("after: 0"), "{}", out.stdout);
    let r = record(home.path());
    assert_eq!(r["status"], "installed");
    assert_eq!(r["setup_done"], 3);
}

#[test]
fn the_secret_option_reaches_the_task_and_nowhere_else() {
    let (home, base, server) = (TempDir::new().unwrap(), TempDir::new().unwrap(), server());
    let out = install(home.path(), &server, base.path(), true);
    assert_eq!(
        fs::read_to_string(base.path().join("secret.txt")).unwrap(),
        SECRET
    );
    for (what, text) in [
        ("stdout", out.stdout.as_str()),
        ("stderr", out.stderr.as_str()),
        (
            "the record",
            &fs::read_to_string(home.path().join("installs/blog.json")).unwrap(),
        ),
    ] {
        assert!(!text.contains("s3cr3t"), "the secret is in {what}: {text}");
    }
}

#[test]
fn no_setup_plan_means_no_setup() {
    let (home, base) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let server = Server::start(vec![(
        "/web.zip",
        Route::ok(build_zip(&[
            ("site/", Entry::Dir),
            ("site/index.html", Entry::file("<html>")),
        ])),
    )]);
    let out = Speedyspec::with_home(&["zip-app"], home.path())
        .env("ZIP_FIXTURE_URL", server.url())
        .run(&[
            "--no-interaction",
            "app",
            "install",
            "site",
            "--title",
            "Hi",
            "--base",
            base.path().to_str().unwrap(),
        ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(!out.stderr.contains("setup:"), "{}", out.stderr);
    let text = fs::read_to_string(home.path().join("installs/site.json")).unwrap();
    let r: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(r["status"], "installed");
}

#[test]
fn a_deleted_directory_is_installed_again_rather_than_resumed() {
    let (home, base, server) = (TempDir::new().unwrap(), TempDir::new().unwrap(), server());
    assert_eq!(install(home.path(), &server, base.path(), true).code, 1);
    fs::remove_dir_all(base.path()).unwrap();
    let out = install(home.path(), &server, base.path(), false);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        !out.stderr.contains("files already in place"),
        "{}",
        out.stderr
    );
    assert_eq!(downloads(&server), 2);
    assert_eq!(order(base.path()), "1 2 3");
    assert_eq!(record(home.path())["status"], "installed");
}

#[test]
fn finishing_a_setup_keeps_the_install_s_history() {
    let (home, base, server) = (TempDir::new().unwrap(), TempDir::new().unwrap(), server());
    assert_eq!(install(home.path(), &server, base.path(), true).code, 1);
    let path = home.path().join("installs/blog.json");
    let mut first = record(home.path());
    first["installed_at"] = serde_json::json!("2026-01-01T00:00:00Z");
    first["cli_version"] = serde_json::json!("0.9.0");
    fs::write(&path, first.to_string()).unwrap();
    assert_eq!(install(home.path(), &server, base.path(), false).code, 0);
    let r = record(home.path());
    assert_eq!(r["installed_at"], "2026-01-01T00:00:00Z");
    assert_eq!(r["cli_version"], "0.9.0");
    assert_eq!(r["status"], "installed");
}
