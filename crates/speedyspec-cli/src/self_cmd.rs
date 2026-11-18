//! `speedyspec self …`: the CLI looking after itself. `self update [--check]` is here; `self
//! plugin` (the CLI's own plugins) is dispatched from `commands.rs`, since it needs them loaded.
//!
//! `self update` runs before PHP starts and before any plugin is loaded, so a broken plugin can
//! never stand in the way of getting a fixed binary.

use std::io::Write;

use clap::{Arg, ArgAction, ArgMatches, Command as Clap};
use speedyspec_fetch::{HttpFetch, github_api_base};
use speedyspec_selfupdate::{Check, Version, check, current_asset, install, target_executable};

/// The version of this binary, from `version.txt`.
pub const VERSION: &str = env!("SPEEDYSPEC_VERSION");

pub fn clap() -> Clap {
    Clap::new("self")
        .about("Manage speedyspec itself")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Clap::new("update")
                .about(
                    "Replace this binary with the latest release, checked against its SHA256SUMS",
                )
                .arg(
                    Arg::new("check")
                        .long("check")
                        .action(ArgAction::SetTrue)
                        .help("Only say whether a newer release exists"),
                ),
        )
}

/// Whether the arguments name `self update` (global flags may come first). Only that command
/// runs before PHP starts; `self plugin` needs the loaded plugins.
pub fn is_self_update(args: &[String]) -> bool {
    let mut words = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--plugin-dir" {
            args.next();
        } else if !arg.starts_with('-') {
            words.push(arg.as_str());
            if words.len() == 2 {
                break;
            }
        }
    }
    words == ["self", "update"]
}

fn say(line: &str) {
    // Never println!: it panics when stdout is a closed pipe.
    let _ = writeln!(std::io::stdout().lock(), "{line}");
}

pub fn run(matches: &ArgMatches) -> i32 {
    match matches.subcommand() {
        Some(("update", update)) => self_update(update.get_flag("check")),
        _ => unreachable!("commands.rs dispatches every other self subcommand"),
    }
}

fn self_update(check_only: bool) -> i32 {
    let current = Version::parse(VERSION).expect("build.rs checks version.txt");
    let Some(asset) = current_asset() else {
        eprintln!(
            "speedyspec: there is no release build for {} {}; build speedyspec from source",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        return 1;
    };
    let api_base = match github_api_base() {
        Ok(base) => base,
        Err(message) => {
            eprintln!("speedyspec: {message}");
            return 2;
        }
    };
    let fetch = HttpFetch::new();
    let release = match check(&fetch, &api_base, &current) {
        Err(error) => {
            eprintln!("speedyspec: {error}");
            return 1;
        }
        Ok(Check::UpToDate(version)) => {
            say(&format!("speedyspec {version} is the latest release"));
            return 0;
        }
        Ok(Check::Ahead { latest }) => {
            say(&format!(
                "speedyspec {current} is newer than the latest release, {latest}"
            ));
            return 0;
        }
        Ok(Check::Newer(release)) => release,
    };
    if check_only {
        say(&format!(
            "speedyspec {} is available (this is {current}); run `speedyspec self update`",
            release.version
        ));
        return 0;
    }
    let exe = match std::env::current_exe()
        .map_err(|e| format!("cannot find the running speedyspec: {e}"))
        .and_then(|invoked| target_executable(&invoked).map_err(|e| e.0))
    {
        Ok(exe) => exe,
        Err(message) => {
            eprintln!("speedyspec: {message}");
            return 1;
        }
    };
    let mut progress = |step: &str| eprintln!("speedyspec: {step}");
    match install(&fetch, &release, &asset, &exe, &mut progress) {
        Ok(()) => {
            say(&format!(
                "updated {} from {current} to {}",
                exe.display(),
                release.version
            ));
            0
        }
        Err(error) => {
            eprintln!("speedyspec: {error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn finds_self_update_after_global_flags() {
        assert!(is_self_update(&args(&["self", "update"])));
        assert!(is_self_update(&args(&[
            "--no-interaction",
            "self",
            "update"
        ])));
        assert!(is_self_update(&args(&[
            "--plugin-dir",
            "self",
            "self",
            "update"
        ])));
        assert!(!is_self_update(&args(&[
            "--plugin-dir",
            "self",
            "app",
            "list"
        ])));
        assert!(!is_self_update(&args(&["app", "self"])));
        assert!(!is_self_update(&args(&["--version"])));
        assert!(!is_self_update(&args(&["self", "plugin", "list"])));
        assert!(!is_self_update(&args(&["self"])));
    }
}
