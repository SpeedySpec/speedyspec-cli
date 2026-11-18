//! `speedyspec self update`, end to end: a copy of the built binary updates itself from a
//! release served by a loopback server. Nothing here talks to GitHub.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Run, Speedyspec};
use sha2::{Digest, Sha256};
use speedyspec_testkit::{Route, Server};

const NEW: &str = "#!/bin/sh\necho \"speedyspec 9.9.9\"\n";

/// The version of the binary under test: what `version.txt` says, which release-please changes.
fn version() -> &'static str {
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../version.txt")).trim()
}

fn asset() -> String {
    format!(
        "speedyspec-{}-{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A release `tag` whose assets are served by the same server (none when `with_assets` is false).
fn release(tag: &str, with_assets: bool) -> Server {
    let server = Server::start(vec![]);
    let url = server.url();
    let assets = if with_assets {
        format!(
            r#"[{{"name":"{}","browser_download_url":"{url}/download/asset","size":{}}},
                {{"name":"SHA256SUMS","browser_download_url":"{url}/download/sums","size":100}}]"#,
            asset(),
            NEW.len()
        )
    } else {
        "[]".to_owned()
    };
    server.route(
        "/repos/SpeedySpec/speedyspec-cli/releases/latest",
        Route::ok(format!(r#"{{"tag_name":"{tag}","assets":{assets}}}"#)),
    );
    server.route("/download/asset", Route::ok(NEW));
    server.route(
        "/download/sums",
        Route::ok(format!("{}  {}\n", sha256(NEW.as_bytes()), asset())),
    );
    server
}

/// `<tmp>/bin/speedyspec`, a copy of the built binary.
fn copy() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("bin")).unwrap();
    let exe = dir.path().join("bin/speedyspec");
    fs::copy(env!("CARGO_BIN_EXE_speedyspec"), &exe).unwrap();
    (dir, exe)
}

fn run(exe: &Path, server: &Server, plugins: &[&str], args: &[&str]) -> Run {
    let home = tempfile::tempdir().unwrap();
    Speedyspec::at(exe, plugins, home.path())
        .env("SPEEDYSPEC_GITHUB_API", server.url())
        .run(args)
}

fn first_line(exe: &Path) -> String {
    let output = std::process::Command::new(exe)
        .args(["--no-plugins", "--version"])
        .output()
        .unwrap();
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn paths(server: &Server) -> Vec<String> {
    server.requests().into_iter().map(|r| r.path).collect()
}

#[test]
fn check_reports_a_newer_release() {
    let server = release("v9.9.9", true);
    let (_dir, exe) = copy();
    let out = run(&exe, &server, &[], &["self", "update", "--check"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        out.stdout,
        format!(
            "speedyspec 9.9.9 is available (this is {}); run `speedyspec self update`\n",
            version()
        )
    );
    assert_eq!(first_line(&exe), format!("speedyspec {}", version()));
    assert_eq!(
        paths(&server),
        vec!["/repos/SpeedySpec/speedyspec-cli/releases/latest"]
    );
}

#[test]
fn updates_the_copy() {
    let server = release("v9.9.9", true);
    let (_dir, exe) = copy();
    let out = run(&exe, &server, &[], &["self", "update"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        out.stdout,
        format!(
            "updated {} from {} to 9.9.9\n",
            exe.canonicalize().unwrap().display(),
            version()
        )
    );
    assert!(
        out.stderr.contains("speedyspec: downloading "),
        "{}",
        out.stderr
    );
    assert_eq!(first_line(&exe), "speedyspec 9.9.9");
}

#[test]
fn updates_through_a_symlink() {
    let server = release("v9.9.9", true);
    let (dir, exe) = copy();
    fs::create_dir(dir.path().join("link")).unwrap();
    let link = dir.path().join("link/speedyspec");
    std::os::unix::fs::symlink("../bin/speedyspec", &link).unwrap();
    let out = run(&link, &server, &[], &["self", "update"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(first_line(&exe), "speedyspec 9.9.9");
}

#[test]
fn up_to_date_is_not_an_error() {
    let server = release(&format!("v{}", version()), true);
    let (_dir, exe) = copy();
    let out = run(&exe, &server, &[], &["self", "update"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        out.stdout,
        format!("speedyspec {} is the latest release\n", version())
    );
    assert_eq!(paths(&server).len(), 1, "nothing was downloaded");
}

#[test]
fn no_release_yet() {
    let server = Server::start(vec![]);
    let (_dir, exe) = copy();
    let out = run(&exe, &server, &[], &["self", "update"]);
    assert_eq!(out.code, 1);
    assert_eq!(
        out.stderr,
        "speedyspec: no release of speedyspec has been published yet\n"
    );
}

#[test]
fn assets_not_uploaded_yet() {
    let server = release("v9.9.9", false);
    let (_dir, exe) = copy();
    let out = run(&exe, &server, &[], &["self", "update"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .contains("its files are still being uploaded, so try again shortly"),
        "{}",
        out.stderr
    );
    assert_eq!(first_line(&exe), format!("speedyspec {}", version()));
}

#[test]
fn works_with_a_broken_plugin() {
    let server = release("v9.9.9", true);
    let (_dir, exe) = copy();
    let out = run(&exe, &server, &["fatal"], &["self", "update", "--check"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.starts_with("speedyspec 9.9.9 is available"),
        "{}",
        out.stdout
    );
}

#[test]
fn version_comes_from_version_txt() {
    let (_dir, exe) = copy();
    assert_eq!(first_line(&exe), format!("speedyspec {}", version()));
}
