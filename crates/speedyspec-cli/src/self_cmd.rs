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
        // The composer plugins' child side: started by `composer install --allow-plugins`, never by hand.
        .subcommand(
            Clap::new("composer-plugins")
                .hide(true)
                .arg(Arg::new("dir").long("dir").required(true)),
        )
        // The runner's child side: started by the CLI itself, never by hand.
        .subcommand(
            Clap::new("run-task")
                .hide(true)
                .arg(Arg::new("request").long("request").required(true))
                .arg(Arg::new("result").long("result").required(true)),
        )
        // The database connection a plugin gives for an install, without its password: for the tests.
        .subcommand(
            Clap::new("database")
                .hide(true)
                .args(crate::app_target::install_args()),
        )
        // Runs one task through the runner and prints what came back: for the tests.
        .subcommand(
            Clap::new("task")
                .hide(true)
                .arg(Arg::new("root").long("root").required(true))
                .arg(Arg::new("file").long("file").required(true))
                .arg(Arg::new("entry").long("entry"))
                .arg(Arg::new("no-app").long("no-app").action(ArgAction::SetTrue))
                .arg(Arg::new("input").long("input"))
                .arg(
                    Arg::new("timeout")
                        .long("timeout")
                        .value_parser(clap::value_parser!(u64)),
                ),
        )
}

/// Whether the arguments name a `self` command that runs before plugins are loaded (global flags
/// may come first): `self update`, and the runner's `self run-task` and `self task`. `self
/// plugin` needs the loaded plugins.
pub fn runs_before_plugins(args: &[String]) -> bool {
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
    matches!(
        words[..],
        ["self", "update" | "run-task" | "task" | "composer-plugins"]
    )
}

fn say(line: &str) {
    // Never println!: it panics when stdout is a closed pipe.
    let _ = writeln!(std::io::stdout().lock(), "{line}");
}

pub fn run(matches: &ArgMatches) -> i32 {
    match matches.subcommand() {
        Some(("update", update)) => self_update(update.get_flag("check")),
        Some(("run-task", run)) => crate::runner_child::run(
            std::path::Path::new(run.get_one::<String>("request").expect("required")),
            std::path::Path::new(run.get_one::<String>("result").expect("required")),
        ),
        Some(("task", task)) => run_one_task(task),
        Some(("composer-plugins", plugins)) => crate::composer_child::run_child(
            std::path::Path::new(plugins.get_one::<String>("dir").expect("required")),
        ),
        _ => unreachable!("commands.rs dispatches every other self subcommand"),
    }
}

/// `self task`: one task through the runner, its outcome printed as JSON.
fn run_one_task(matches: &ArgMatches) -> i32 {
    let text = |name: &str| matches.get_one::<String>(name).cloned();
    let input = match text("input").map(|input| serde_json::from_str(&input)) {
        None => serde_json::json!({}),
        Some(Ok(input)) => input,
        Some(Err(e)) => {
            eprintln!("speedyspec: --input is not JSON: {e}");
            return 2;
        }
    };
    let runtime = speedyspec_driver::AppRuntime {
        entry: text("entry"),
        ..Default::default()
    };
    let task = speedyspec_driver::AppTask {
        file: text("file").expect("required").into(),
        input,
        loads_app: !matches.get_flag("no-app"),
        timeout: std::time::Duration::from_secs(*matches.get_one::<u64>("timeout").unwrap_or(&600)),
    };
    let root = std::path::PathBuf::from(text("root").expect("required"));
    match crate::runner::run(&root, &runtime, &task) {
        Ok(outcome) => {
            say(&outcome.to_json().to_string());
            0
        }
        Err(error) => {
            eprintln!("speedyspec: {error}");
            1
        }
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
    fn finds_the_commands_that_run_before_plugins() {
        assert!(runs_before_plugins(&args(&["self", "update"])));
        assert!(runs_before_plugins(&args(&[
            "--no-interaction",
            "self",
            "update"
        ])));
        assert!(runs_before_plugins(&args(&[
            "--plugin-dir",
            "self",
            "self",
            "update"
        ])));
        assert!(!runs_before_plugins(&args(&[
            "--plugin-dir",
            "self",
            "app",
            "list"
        ])));
        assert!(!runs_before_plugins(&args(&["app", "self"])));
        assert!(!runs_before_plugins(&args(&["--version"])));
        assert!(!runs_before_plugins(&args(&["self", "plugin", "list"])));
        assert!(!runs_before_plugins(&args(&["self"])));
        assert!(runs_before_plugins(&args(&[
            "self",
            "run-task",
            "--request",
            "r"
        ])));
        assert!(runs_before_plugins(&args(&["self", "task"])));
    }
}
