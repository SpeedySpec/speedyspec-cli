//! `speedyspec composer <subcommand>`: the composer subcommands speedyspec's Rust composer supports, each acting on
//! `--working-dir` (the current directory by default), as composer's own flag does. Nothing is resolved, so the
//! subcommands that need resolving (`update`, `require` and the rest) are refused with a pointer to composer itself.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command as Process;

use clap::{Arg, ArgAction, ArgMatches, Command as Clap};
use speedyspec_composer::{Lock, Maps, Platform, install};
use speedyspec_driver_zip::HttpFetch;

const SUPPORTED: &str = "install, dump-autoload, show and validate";

fn working_dir() -> Arg {
    Arg::new("working-dir")
        .long("working-dir")
        .short('d')
        .value_name("DIR")
        .help("The directory holding composer.json (the current directory by default)")
}

pub fn clap() -> Clap {
    Clap::new("composer")
        .about("Install a plugin's composer packages from its composer.lock, in Rust")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .allow_external_subcommands(true)
        .subcommand(
            Clap::new("install")
                .about("Install exactly the packages composer.lock lists (never dev ones)")
                .arg(working_dir())
                .arg(
                    Arg::new("no-dev")
                        .long("no-dev")
                        .action(ArgAction::SetTrue)
                        .help("Accepted for composer's sake; dev packages are never installed"),
                ),
        )
        .subcommand(
            Clap::new("dump-autoload")
                .about("Rebuild the autoload maps from vendor/ and composer.json")
                .arg(working_dir()),
        )
        .subcommand(
            Clap::new("show")
                .about("List the installed packages: name, version, type")
                .arg(working_dir()),
        )
        .subcommand(
            Clap::new("validate")
                .about("Check composer.json and composer.lock the way speedyspec reads them")
                .arg(working_dir()),
        )
}

/// The embedded PHP's version and extensions, from a run of this binary's own `php` command, so it works even
/// before (or without) the CLI's PHP has started.
pub fn platform() -> Result<Platform, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot find the speedyspec executable: {e}"))?;
    let dir = tempfile::tempdir().map_err(|e| format!("cannot make a temporary directory: {e}"))?;
    let probe = dir.path().join("platform.php");
    std::fs::write(
        &probe,
        "<?php echo json_encode(['php' => PHP_VERSION, 'extensions' => array_map('strtolower', get_loaded_extensions())]);",
    )
    .map_err(|e| format!("cannot write {}: {e}", probe.display()))?;
    let out = Process::new(exe)
        .arg("--no-plugins")
        .arg("php")
        .arg(&probe)
        .output()
        .map_err(|e| format!("cannot ask the embedded PHP for its extensions: {e}"))?;
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|_| "cannot read the embedded PHP's version and extensions".to_owned())?;
    Ok(Platform {
        php_version: json["php"].as_str().unwrap_or_default().to_owned(),
        extensions: json["extensions"]
            .as_array()
            .map(|all| {
                all.iter()
                    .filter_map(|e| e.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// Installs the packages of the plugin in `dir`, if it has any; prints the warnings. `Ok(false)` when there was
/// nothing to install.
pub fn install_in(dir: &Path) -> Result<bool, String> {
    let Some(lock) = Lock::read(dir).map_err(|e| e.to_string())? else {
        return Ok(false);
    };
    if lock.packages.is_empty() && lock.root == Default::default() {
        return Ok(false);
    }
    let platform = platform()?;
    let installed = install(dir, &lock, &HttpFetch::new(), &platform).map_err(|e| e.to_string())?;
    for warning in &installed.warnings {
        eprintln!("speedyspec: warning: {warning}");
    }
    let plugins: Vec<&str> = lock
        .packages
        .iter()
        .filter(|p| p.kind == "composer-plugin")
        .map(|p| p.name.as_str())
        .collect();
    if !plugins.is_empty() {
        eprintln!(
            "speedyspec: composer plugins were not run: {} (speedyspec does not run composer plugins yet)",
            plugins.join(", ")
        );
    }
    Ok(true)
}

fn dir_of(matches: &ArgMatches) -> PathBuf {
    matches
        .get_one::<String>("working-dir")
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

fn failure(message: &str) -> i32 {
    eprintln!("speedyspec: {message}");
    1
}

fn read(dir: &Path) -> Result<Lock, String> {
    Lock::read(dir)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("there is no composer.json in {}", dir.display()))
}

pub fn run(matches: &ArgMatches) -> i32 {
    let (name, sub) = matches.subcommand().expect("clap requires a subcommand");
    let mut out = std::io::stdout().lock();
    match name {
        "install" => {
            let dir = dir_of(sub);
            if let Err(message) = read(&dir) {
                return failure(&message);
            }
            match install_in(&dir) {
                Ok(_) => 0,
                Err(message) => failure(&message),
            }
        }
        "dump-autoload" => {
            let dir = dir_of(sub);
            match read(&dir) {
                Ok(lock) => match Maps::build(&dir, &lock).write(&dir) {
                    Ok(()) => 0,
                    Err(e) => failure(&e.to_string()),
                },
                Err(message) => failure(&message),
            }
        }
        "show" => {
            let dir = dir_of(sub);
            match read(&dir) {
                Ok(lock) => {
                    for p in lock
                        .packages
                        .iter()
                        .filter(|p| dir.join("vendor").join(&p.name).is_dir())
                    {
                        let _ = writeln!(out, "{}\t{}\t{}", p.name, p.version, p.kind);
                    }
                    0
                }
                Err(message) => failure(&message),
            }
        }
        "validate" => {
            let dir = dir_of(sub);
            match read(&dir) {
                Ok(_) => {
                    let _ = writeln!(out, "composer.json and composer.lock are valid");
                    0
                }
                Err(message) => failure(&message),
            }
        }
        other => {
            eprintln!(
                "speedyspec: speedyspec composer supports {SUPPORTED}; run composer itself for {other}"
            );
            2
        }
    }
}
