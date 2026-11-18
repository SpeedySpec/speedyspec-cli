//! Embeds the PHP contracts package into the binary.
//!
//! The contracts are the one piece of PHP every plugin can rely on, so they are evaluated at
//! start-up and no plugin needs Composer or an autoloader to find them. They come from the
//! release pinned in `contracts-version`, which `scripts/fetch-contracts.sh` unpacks into
//! `.contracts/<version>/`; `SPEEDYSPEC_CONTRACTS_DIR` names a checkout to use instead, for
//! working on the contracts themselves.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

const DIR_ENV: &str = "SPEEDYSPEC_CONTRACTS_DIR";

fn main() {
    embed_version();
    println!("cargo:rerun-if-env-changed={DIR_ENV}");
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let repo = manifest_dir.join("../..");
    let pinned = read_pin(&repo.join("contracts-version"));
    let configured = env::var(DIR_ENV).ok().filter(|v| !v.is_empty());
    let dir = match configured.as_deref() {
        Some(configured) => PathBuf::from(configured),
        None => repo.join(".contracts").join(&pinned),
    };
    let dir = dir.canonicalize().unwrap_or_else(|_| match configured {
        Some(_) => fail(&format!(
            "the contracts directory {} ({DIR_ENV}) cannot be read",
            dir.display()
        )),
        None => fail(&format!("contracts {pinned} have not been fetched")),
    });

    let composer = dir.join("composer.json");
    let version = read_version(&composer);
    println!("cargo:rerun-if-changed={}", composer.display());
    if version != pinned {
        if configured.is_some() {
            println!(
                "cargo:warning=building with contracts {version} from {}, not the pinned {pinned}",
                dir.display()
            );
        } else {
            fail(&format!(
                "{} holds contracts {version}, not the pinned {pinned}",
                dir.display()
            ));
        }
    }
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

/// The product version, from `version.txt` at the repository root (release-please keeps it).
fn embed_version() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let file = manifest_dir.join("../../version.txt");
    println!("cargo:rerun-if-changed={}", file.display());
    let text = fs::read_to_string(&file).unwrap_or_else(|e| {
        eprintln!("error: speedyspec-cli cannot read {}: {e}", file.display());
        std::process::exit(1)
    });
    let version = text.trim();
    if semver::Version::parse(version).is_err() {
        eprintln!(
            "error: {} holds {version:?}, which is not a version (major.minor.patch)",
            file.display()
        );
        std::process::exit(1);
    }
    println!("cargo:rustc-env=SPEEDYSPEC_VERSION={version}");
}

/// The contracts version `contracts-version` pins.
fn read_pin(file: &Path) -> String {
    println!("cargo:rerun-if-changed={}", file.display());
    let text = fs::read_to_string(file)
        .unwrap_or_else(|e| fail(&format!("cannot read {}: {e}", file.display())));
    text.lines()
        .find_map(|line| line.strip_prefix("version="))
        .map(|version| version.trim().to_owned())
        .filter(|version| !version.is_empty())
        .unwrap_or_else(|| fail(&format!("{} has no version= line", file.display())))
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
        "       run scripts/fetch-contracts.sh, or set {DIR_ENV} to a checkout of \
         speedyspec-cli-contracts"
    );
    std::process::exit(1)
}
