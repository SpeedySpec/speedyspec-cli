//! Downloading and swapping in a release binary, against a loopback server and temporary
//! directories. The "binaries" are small shell scripts.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use speedyspec_fetch::HttpFetch;
use speedyspec_selfupdate::{
    Asset, Release, SUMS, Version, install, target_executable, writable_problem,
};
use speedyspec_testkit::{Route, Server};

const ASSET: &str = "speedyspec-linux-x86_64";
const NEW: &str = "#!/bin/sh\necho \"speedyspec 0.3.0\"\n";
const WRONG_VERSION: &str = "#!/bin/sh\necho \"speedyspec 0.2.9\"\n";
const FAILING: &str = "#!/bin/sh\nexit 3\n";

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn sums_for(name: &str, bytes: &[u8]) -> String {
    format!("{}  {name}\n", sha256(bytes))
}

/// A server with `/asset` and `/sums`, and a release pointing at them.
fn serve(asset: Route, sums: Route, declared_size: u64) -> (Server, Release) {
    let server = Server::start(vec![("/asset", asset), ("/sums", sums)]);
    let release = Release {
        tag: "v0.3.0".to_owned(),
        version: Version::parse("0.3.0").unwrap(),
        assets: vec![
            Asset {
                name: ASSET.to_owned(),
                url: format!("{}/asset", server.url()),
                size: declared_size,
            },
            Asset {
                name: SUMS.to_owned(),
                url: format!("{}/sums", server.url()),
                size: 100,
            },
        ],
    };
    (server, release)
}

fn good(script: &str) -> (Server, Release) {
    serve(
        Route::ok(script),
        Route::ok(sums_for(ASSET, script.as_bytes())),
        script.len() as u64,
    )
}

/// `<tmp>/bin/speedyspec`, holding `old`.
fn old_binary() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("bin")).unwrap();
    let exe = dir.path().join("bin/speedyspec");
    fs::write(&exe, "old").unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    (dir, exe)
}

fn leftovers(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".speedyspec-update-"))
        .collect()
}

fn run(release: &Release, exe: &Path) -> (Result<(), String>, Vec<String>) {
    let mut steps = Vec::new();
    let result = install(&HttpFetch::new(), release, ASSET, exe, &mut |step: &str| {
        steps.push(step.to_owned())
    })
    .map_err(|error| error.0);
    (result, steps)
}

fn assert_unchanged(exe: &Path) {
    assert_eq!(fs::read_to_string(exe).unwrap(), "old");
    assert!(leftovers(exe.parent().unwrap()).is_empty());
}

#[test]
fn replaces_the_executable() {
    let (_server, release) = good(NEW);
    let (_dir, exe) = old_binary();
    let (result, steps) = run(&release, &exe);
    result.unwrap();
    assert_eq!(fs::read_to_string(&exe).unwrap(), NEW);
    assert_eq!(
        fs::metadata(&exe).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(leftovers(exe.parent().unwrap()).is_empty());
    assert_eq!(
        steps,
        vec![
            "downloading speedyspec-linux-x86_64 (v0.3.0)".to_owned(),
            "verifying".to_owned()
        ]
    );
}

#[test]
fn follows_a_symlink() {
    let (_server, release) = good(NEW);
    let (dir, exe) = old_binary();
    fs::create_dir(dir.path().join("link")).unwrap();
    let link = dir.path().join("link/speedyspec");
    std::os::unix::fs::symlink("../bin/speedyspec", &link).unwrap();
    let target = target_executable(&link).unwrap();
    assert_eq!(target, exe.canonicalize().unwrap());
    let (result, _) = run(&release, &target);
    result.unwrap();
    assert_eq!(fs::read_to_string(&exe).unwrap(), NEW);
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn checksum_mismatch_changes_nothing() {
    let (_server, release) = serve(
        Route::ok(NEW),
        Route::ok(sums_for(ASSET, b"something else")),
        NEW.len() as u64,
    );
    let (_dir, exe) = old_binary();
    let error = run(&release, &exe).0.unwrap_err();
    assert_eq!(
        error,
        format!(
            "the download of speedyspec-linux-x86_64 does not match SHA256SUMS (expected {}, got \
             {}); nothing was changed",
            sha256(b"something else"),
            sha256(NEW.as_bytes())
        )
    );
    assert_unchanged(&exe);
}

#[test]
fn unlisted_asset_changes_nothing() {
    let (_server, release) = serve(
        Route::ok(NEW),
        Route::ok(sums_for("speedyspec-macos-aarch64", NEW.as_bytes())),
        NEW.len() as u64,
    );
    let (_dir, exe) = old_binary();
    assert_eq!(
        run(&release, &exe).0.unwrap_err(),
        "SHA256SUMS of release v0.3.0 does not list speedyspec-linux-x86_64; nothing was changed"
    );
    assert_unchanged(&exe);
}

#[test]
fn missing_sums_changes_nothing() {
    let (_server, mut release) = good(NEW);
    release.assets.retain(|asset| asset.name != SUMS);
    let (_dir, exe) = old_binary();
    assert_eq!(
        run(&release, &exe).0.unwrap_err(),
        "release v0.3.0 has no SHA256SUMS, so its download cannot be verified; nothing was changed"
    );
    assert_unchanged(&exe);
}

#[test]
fn missing_asset_says_to_wait() {
    let (_server, mut release) = good(NEW);
    release.assets.clear();
    let (_dir, exe) = old_binary();
    assert_eq!(
        run(&release, &exe).0.unwrap_err(),
        "release v0.3.0 has no speedyspec-linux-x86_64 yet; if it was published in the last few \
         minutes its files are still being uploaded, so try again shortly"
    );
    assert_unchanged(&exe);
}

#[test]
fn larger_than_declared_changes_nothing() {
    let body = "x".repeat(40);
    let (_server, release) = serve(
        Route::ok(body.clone()),
        Route::ok(sums_for(ASSET, body.as_bytes())),
        10,
    );
    let (_dir, exe) = old_binary();
    assert_eq!(
        run(&release, &exe).0.unwrap_err(),
        "speedyspec-linux-x86_64 is larger than release v0.3.0 says (10 bytes); nothing was changed"
    );
    assert_unchanged(&exe);
}

#[test]
fn cut_short_changes_nothing() {
    let (_server, release) = serve(
        Route::truncated(NEW, 5),
        Route::ok(sums_for(ASSET, NEW.as_bytes())),
        NEW.len() as u64,
    );
    let (_dir, exe) = old_binary();
    let error = run(&release, &exe).0.unwrap_err();
    assert!(
        error.starts_with("could not download speedyspec-linux-x86_64:")
            && error.ends_with("; nothing was changed"),
        "{error}"
    );
    assert_unchanged(&exe);
}

#[test]
fn a_binary_that_does_not_start_is_not_installed() {
    for (script, reason) in [
        (FAILING, "exit status 3".to_owned()),
        (WRONG_VERSION, "it printed \"speedyspec 0.2.9\"".to_owned()),
    ] {
        let (_server, release) = good(script);
        let (_dir, exe) = old_binary();
        assert_eq!(
            run(&release, &exe).0.unwrap_err(),
            format!(
                "the downloaded speedyspec-linux-x86_64 did not run ({reason}); nothing was changed"
            )
        );
        assert_unchanged(&exe);
    }
}

#[test]
fn unwritable_directory_is_reported_before_downloading() {
    let (server, release) = good(NEW);
    let (dir, exe) = old_binary();
    let bin = dir.path().join("bin");
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o555)).unwrap();
    let probe = bin.join("probe");
    if fs::write(&probe, "").is_ok() {
        let _ = fs::remove_file(&probe);
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        println!("skipped: running as root");
        return;
    }
    let expected = format!(
        "cannot replace {}: {} is not writable by you. Run `sudo speedyspec self update`, or \
         download https://github.com/SpeedySpec/speedyspec-cli/releases/latest/download/{ASSET} \
         yourself",
        exe.display(),
        bin.display()
    );
    assert_eq!(writable_problem(&exe, ASSET), Some(expected.clone()));
    let error = run(&release, &exe).0.unwrap_err();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(error, expected);
    assert!(server.requests().is_empty(), "nothing was downloaded");
    assert_unchanged(&exe);
}

#[test]
fn release_sums_script_output_is_read_back() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("speedyspec-linux-x86_64"), "a").unwrap();
    fs::write(dir.path().join("speedyspec-macos-aarch64"), "bb").unwrap();
    fs::write(dir.path().join("notes.txt"), "not a binary").unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/release-sums.sh");
    let status = std::process::Command::new(&script)
        .arg(dir.path())
        .status()
        .unwrap();
    assert!(status.success(), "{status}");
    let text = fs::read_to_string(dir.path().join(SUMS)).unwrap();
    let sums = speedyspec_selfupdate::parse_sums(&text).unwrap();
    assert_eq!(sums.len(), 2, "{text}");
    assert_eq!(sums["speedyspec-linux-x86_64"], sha256(b"a"));
    assert_eq!(sums["speedyspec-macos-aarch64"], sha256(b"bb"));
    let names: Vec<&str> = text.lines().map(|line| &line[66..]).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
}
