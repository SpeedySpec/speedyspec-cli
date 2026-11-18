//! Embeds the PHP contracts package into the binary.
//!
//! The contracts are the one piece of PHP every plugin can rely on, so they are evaluated at
//! start-up and no plugin needs Composer or an autoloader to find them. The source comes from
//! the sibling checkout `speedyspec-cli-contracts` (override with `SPEEDYSPEC_CONTRACTS_DIR`);
//! fetching a pinned release instead is a later plan's job.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

const DIR_ENV: &str = "SPEEDYSPEC_CONTRACTS_DIR";
const DEFAULT_DIR: &str = "../../../speedyspec-cli-contracts";

fn main() {
    println!("cargo:rerun-if-env-changed={DIR_ENV}");
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let configured = env::var(DIR_ENV).ok().filter(|v| !v.is_empty());
    let dir = configured
        .as_deref()
        .map_or_else(|| manifest_dir.join(DEFAULT_DIR), PathBuf::from);
    let dir = dir.canonicalize().unwrap_or_else(|e| {
        fail(&format!(
            "the contracts directory {} cannot be read: {e}",
            dir.display()
        ))
    });

    let composer = dir.join("composer.json");
    let version = read_version(&composer);
    println!("cargo:rerun-if-changed={}", composer.display());
    println!("cargo:rustc-env=CONTRACTS_VERSION={version}");

    let src = dir.join("src");
    if !src.is_dir() {
        fail(&format!("{} has no src/ directory", dir.display()));
    }
    // The directory itself, so that a contract file added later (anywhere under src/, which
    // cargo scans for changes) is embedded too, not only the files seen now.
    println!("cargo:rerun-if-changed={}", src.display());
    let mut files = Vec::new();
    collect(&src, &mut files);
    files.sort();
    if files.is_empty() {
        fail(&format!("{} contains no PHP files", src.display()));
    }

    let mut generated = String::from("pub const CONTRACTS: &[(&str, &str)] = &[\n");
    for file in &files {
        println!("cargo:rerun-if-changed={}", file.display());
        let relative = file
            .strip_prefix(&src)
            .expect("collected under src")
            .to_string_lossy()
            .replace('\\', "/");
        let label = format!("speedyspec://contracts/{relative}");
        let absolute = file.to_string_lossy().into_owned();
        writeln!(generated, "    ({label:?}, include_str!({absolute:?})),")
            .expect("writing to a String");
    }
    generated.push_str("];\n");
    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets it")).join("contracts.rs");
    fs::write(&out, generated).expect("OUT_DIR is writable");
}

fn read_version(composer: &Path) -> String {
    let text = fs::read_to_string(composer)
        .unwrap_or_else(|e| fail(&format!("cannot read {}: {e}", composer.display())));
    let json: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| fail(&format!("{} is not valid JSON: {e}", composer.display())));
    match json.get("version").and_then(serde_json::Value::as_str) {
        Some(version) => version.to_owned(),
        None => fail(&format!("{} has no \"version\" string", composer.display())),
    }
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|e| fail(&format!("cannot read {}: {e}", dir.display())));
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "php") {
            files.push(path);
        }
    }
}

fn fail(message: &str) -> ! {
    eprintln!("error: speedyspec-cli cannot embed the PHP contracts: {message}");
    eprintln!(
        "       check out speedyspec-cli-contracts next to this repository, or set {DIR_ENV} to it"
    );
    std::process::exit(1)
}
