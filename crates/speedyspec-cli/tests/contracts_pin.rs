//! `scripts/fetch-contracts.sh`, against an archive served by a loopback server. The script is
//! copied into a temporary "repository" so `.contracts` is created there, not in this checkout.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};
use speedyspec_testkit::{Route, Server};

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A `.tar.gz` holding `speedyspec-cli-contracts-1.0.0/{composer.json,src/X.php}`, as GitHub's
/// tag archives do.
fn archive() -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("speedyspec-cli-contracts-1.0.0");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("composer.json"),
        r#"{"name":"speedyspec/cli-contracts","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("src/X.php"), "<?php\n").unwrap();
    let out = dir.path().join("a.tar.gz");
    let status = Command::new("tar")
        .arg("-czf")
        .arg(&out)
        .arg("-C")
        .arg(dir.path())
        .arg("speedyspec-cli-contracts-1.0.0")
        .status()
        .unwrap();
    assert!(status.success());
    fs::read(out).unwrap()
}

/// A temporary repository with the script in `scripts/`, and a pin naming `url` and `sha256`.
fn repository(url: &str, sha256: &str) -> (tempfile::TempDir, PathBuf) {
    let repo = tempfile::tempdir().unwrap();
    fs::create_dir(repo.path().join("scripts")).unwrap();
    let script = repo.path().join("scripts/fetch-contracts.sh");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/fetch-contracts.sh"),
        &script,
    )
    .unwrap();
    let pin = repo.path().join("pin");
    fs::write(&pin, format!("version=1.0.0\nurl={url}\nsha256={sha256}\n")).unwrap();
    (repo, pin)
}

fn fetch(repo: &Path, pin: &Path) -> Output {
    Command::new(repo.join("scripts/fetch-contracts.sh"))
        .env("SPEEDYSPEC_CONTRACTS_PIN", pin)
        .output()
        .unwrap()
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn fetches_and_unpacks_the_pinned_archive() {
    let body = archive();
    let server = Server::start(vec![("/a.tar.gz", Route::ok(body.clone()))]);
    let (repo, pin) = repository(&format!("{}/a.tar.gz", server.url()), &sha256(&body));
    let out = fetch(repo.path(), &pin);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let unpacked = repo.path().join(".contracts/1.0.0");
    assert!(unpacked.join("composer.json").is_file());
    assert!(unpacked.join("src/X.php").is_file());
    assert_eq!(entries(&repo.path().join(".contracts")), vec!["1.0.0"]);
}

#[test]
fn a_second_run_downloads_nothing() {
    let body = archive();
    let server = Server::start(vec![("/a.tar.gz", Route::ok(body.clone()))]);
    let (repo, pin) = repository(&format!("{}/a.tar.gz", server.url()), &sha256(&body));
    assert!(fetch(repo.path(), &pin).status.success());
    let again = fetch(repo.path(), &pin);
    assert!(again.status.success());
    assert!(
        String::from_utf8_lossy(&again.stdout).contains("is up to date"),
        "{}",
        String::from_utf8_lossy(&again.stdout)
    );
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn checksum_mismatch_leaves_nothing() {
    let body = archive();
    let server = Server::start(vec![("/a.tar.gz", Route::ok(body))]);
    let (repo, pin) = repository(&format!("{}/a.tar.gz", server.url()), &"0".repeat(64));
    let out = fetch(repo.path(), &pin);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("checksum mismatch"), "{stderr}");
    assert!(
        entries(&repo.path().join(".contracts")).is_empty(),
        "{:?}",
        entries(&repo.path().join(".contracts"))
    );
}
