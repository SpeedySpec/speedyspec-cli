//! `scripts/release-pending.sh`: whether version.txt names a version that has no tag yet, which
//! is how the Release workflow knows a merged release pull request is waiting for its binaries.

use std::fs;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.test",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A repository whose version.txt says `version`, with the script in scripts/.
fn repository(version: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("scripts")).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/release-pending.sh"),
        dir.path().join("scripts/release-pending.sh"),
    )
    .unwrap();
    fs::write(dir.path().join("version.txt"), format!("{version}\n")).unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

fn pending(dir: &Path) -> String {
    let output = Command::new(dir.join("scripts/release-pending.sh"))
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn nothing_is_pending_before_the_first_release() {
    let repo = repository("0.0.0");
    assert_eq!(pending(repo.path()), "pending=false\ntag=v0.0.0\n");
}

#[test]
fn an_untagged_version_is_pending() {
    let repo = repository("0.1.0");
    assert_eq!(pending(repo.path()), "pending=true\ntag=v0.1.0\n");
}

#[test]
fn a_tagged_version_is_not_pending() {
    let repo = repository("0.1.0");
    git(repo.path(), &["tag", "v0.1.0"]);
    assert_eq!(pending(repo.path()), "pending=false\ntag=v0.1.0\n");
}
