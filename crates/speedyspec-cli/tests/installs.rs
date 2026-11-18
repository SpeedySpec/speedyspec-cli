//! Install records: a zip install is findable by name afterwards, and `app installs` lists them.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Run, Speedyspec};
use speedyspec_testkit::{Entry, Route, Server, build_zip};
use tempfile::TempDir;

fn server() -> Server {
    Server::start(vec![(
        "/web.zip",
        Route::ok(build_zip(&[
            ("site/", Entry::Dir),
            ("site/index.html", Entry::file("<html>")),
        ])),
    )])
}

/// `app install site` into `base`, with `home` as SPEEDYSPEC_HOME.
fn install(home: &Path, server: &Server, base: &Path, extra: &[&str]) -> Run {
    let mut args = vec![
        "--no-interaction",
        "app",
        "install",
        "site",
        "--title",
        "Hi",
    ];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["--base", base.to_str().unwrap()]);
    Speedyspec::with_home(&["zip-app"], home)
        .env("ZIP_FIXTURE_URL", server.url())
        .run(&args)
}

fn record(home: &Path, name: &str) -> serde_json::Value {
    let path = home.join("installs").join(format!("{name}.json"));
    serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap()
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap()
}

#[test]
fn zip_install_writes_a_record() {
    let (home, base, server) = (TempDir::new().unwrap(), TempDir::new().unwrap(), server());
    let out = install(home.path(), &server, base.path(), &["--name", "blog"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let r = record(home.path(), "blog");
    assert_eq!(r["schema"], 1);
    assert_eq!(r["name"], "blog");
    assert_eq!(r["app"], "site");
    assert_eq!(r["parts"], serde_json::json!(["web"]));
    assert_eq!(r["driver"], "zip");
    assert_eq!(
        canonical(Path::new(r["location"].as_str().unwrap())),
        canonical(base.path())
    );
    assert_eq!(r["status"], "installed");
    assert!(r["installed_at"].as_str().unwrap().ends_with('Z'), "{r}");
    assert!(r["cli_version"].is_string());
}

#[test]
fn install_name_defaults_to_the_app() {
    let (home, base, server) = (TempDir::new().unwrap(), TempDir::new().unwrap(), server());
    let out = install(home.path(), &server, base.path(), &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(record(home.path(), "site")["name"], "site");
}

#[test]
fn bad_install_name_is_a_usage_error() {
    let (home, base, server) = (TempDir::new().unwrap(), TempDir::new().unwrap(), server());
    let out = install(home.path(), &server, base.path(), &["--name", "Not_Valid"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("the install name"), "{}", out.stderr);
    assert!(server.requests().is_empty());
}

#[test]
fn same_name_elsewhere_is_refused_before_downloading() {
    let (home, server) = (TempDir::new().unwrap(), server());
    let (first, second) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    assert_eq!(
        install(home.path(), &server, first.path(), &["--name", "blog"]).code,
        0
    );
    let requests = server.requests().len();
    let out = install(home.path(), &server, second.path(), &["--name", "blog"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("an install named blog already exists (app site, at ")
            && out.stderr.contains("choose another --name"),
        "{}",
        out.stderr
    );
    assert_eq!(server.requests().len(), requests, "nothing was downloaded");
    assert!(fs::read_dir(second.path()).unwrap().next().is_none());
}

#[test]
fn reinstall_replaces_the_record() {
    let (home, base, server) = (TempDir::new().unwrap(), TempDir::new().unwrap(), server());
    assert_eq!(
        install(home.path(), &server, base.path(), &["--name", "blog"]).code,
        0
    );
    let first = record(home.path(), "blog")["installed_at"].clone();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let out = install(
        home.path(),
        &server,
        base.path(),
        &["--name", "blog", "--force"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_ne!(record(home.path(), "blog")["installed_at"], first);
}

#[test]
fn failed_install_writes_no_record() {
    let home = TempDir::new().unwrap();
    let base = TempDir::new().unwrap();
    let server = Server::start(vec![("/web.zip", Route::status(404))]);
    let out = install(home.path(), &server, base.path(), &["--name", "blog"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(!home.path().join("installs/blog.json").exists());
}

#[test]
fn app_installs_lists_records_sorted() {
    let (home, server) = (TempDir::new().unwrap(), server());
    let (a, b) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    assert_eq!(
        install(home.path(), &server, b.path(), &["--name", "zeta"]).code,
        0
    );
    assert_eq!(
        install(home.path(), &server, a.path(), &["--name", "alpha"]).code,
        0
    );
    let out = Speedyspec::with_home(&[], home.path()).run(&["app", "installs"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let lines: Vec<&str> = out.stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{}", out.stdout);
    assert!(lines[0].starts_with("alpha\tsite\tzip\t"), "{}", lines[0]);
    assert!(lines[1].starts_with("zeta\tsite\tzip\t"), "{}", lines[1]);
    assert!(
        out.stderr
            .contains("Kubernetes installs are kept in their cluster"),
        "{}",
        out.stderr
    );
}

#[test]
fn unreadable_record_is_skipped_with_a_warning() {
    let (home, base, server) = (TempDir::new().unwrap(), TempDir::new().unwrap(), server());
    assert_eq!(
        install(home.path(), &server, base.path(), &["--name", "blog"]).code,
        0
    );
    fs::write(home.path().join("installs/broken.json"), "{not json").unwrap();
    let out = Speedyspec::with_home(&[], home.path()).run(&["app", "installs"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout.lines().count(), 1, "{}", out.stdout);
    assert!(out.stderr.contains("broken.json"), "{}", out.stderr);
}

#[test]
fn relative_home_warns_like_the_plugins_dir() {
    let out = Speedyspec::new(&[])
        .env("SPEEDYSPEC_HOME", "relative/home")
        .run(&["app", "installs"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("SPEEDYSPEC_HOME must be an absolute path"),
        "{}",
        out.stderr
    );
}
