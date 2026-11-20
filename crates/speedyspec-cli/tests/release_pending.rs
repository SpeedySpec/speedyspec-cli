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

fn head(dir: &Path) -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
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
    let commit = head(repo.path());
    assert_eq!(
        pending(repo.path()),
        format!("pending=false\ntag=v0.0.0\ncommit={commit}\n")
    );
}

#[test]
fn an_untagged_version_is_pending() {
    let repo = repository("0.1.0");
    let commit = head(repo.path());
    assert_eq!(
        pending(repo.path()),
        format!("pending=true\ntag=v0.1.0\ncommit={commit}\n")
    );
}

#[test]
fn a_tagged_version_is_not_pending() {
    let repo = repository("0.1.0");
    git(repo.path(), &["tag", "v0.1.0"]);
    let commit = head(repo.path());
    assert_eq!(
        pending(repo.path()),
        format!("pending=false\ntag=v0.1.0\ncommit={commit}\n")
    );
}

/// A release whose build failed is retried on a later push. The retry must build the commit
/// release-please tags (the one that set version.txt), not the later code on main, or the
/// binaries would carry code the tag and changelog do not.
#[test]
fn the_release_commit_is_the_one_that_set_the_version() {
    let repo = repository("0.1.0");
    let release = head(repo.path());
    fs::write(
        repo.path().join("later.txt"),
        "a fix pushed after the release\n",
    )
    .unwrap();
    git(repo.path(), &["add", "later.txt"]);
    git(repo.path(), &["commit", "-q", "-m", "fix: later"]);
    assert_ne!(head(repo.path()), release);
    assert_eq!(
        pending(repo.path()),
        format!("pending=true\ntag=v0.1.0\ncommit={release}\n")
    );
}

/// In a shallow clone the last commit to touch version.txt would look like HEAD, which is the
/// very mistake the commit line exists to prevent, so the script refuses.
#[test]
fn a_shallow_clone_is_refused() {
    let repo = repository("0.1.0");
    fs::write(repo.path().join("later.txt"), "later\n").unwrap();
    git(repo.path(), &["add", "later.txt"]);
    git(repo.path(), &["commit", "-q", "-m", "fix: later"]);
    let shallow = tempfile::tempdir().unwrap();
    let url = format!("file://{}", repo.path().display());
    git(
        shallow.path(),
        &["clone", "-q", "--depth", "1", &url, "clone"],
    );
    let clone = shallow.path().join("clone");
    let output = Command::new(clone.join("scripts/release-pending.sh"))
        .current_dir(&clone)
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("fetch-depth 0"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
