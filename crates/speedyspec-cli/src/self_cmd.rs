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
        // In a runner pod: an app db operation on the connection a request file holds (deleted once read). A dump
        // comes in on stdin and goes out on stdout.
        .subcommand(
            Clap::new("db")
                .hide(true)
                .arg(Arg::new("request").long("request").required(true))
                .arg(Arg::new("operation").required(true)),
        )
        // In a runner pod: a shelf's entries, as far as an app's plugin reads them, as a tar on stdout.
        .subcommand(
            Clap::new("shelf-index")
                .hide(true)
                .arg(Arg::new("dir").long("dir").required(true)),
        )
        // In a runner pod: puts the directory tarred on stdin in place of a plugin or theme, or removes one.
        .subcommand(
            Clap::new("shelf-swap")
                .hide(true)
                .arg(Arg::new("dir").long("dir").required(true))
                .arg(Arg::new("slug").long("slug").required(true))
                .arg(Arg::new("mode").long("mode").required(true)),
        )
        // In a runner pod: a backup, the look before a restore, and a restore, of the install there; each reads its
        // request file (deleted once read) and prints its outcome as JSON.
        .subcommand(
            Clap::new("backup-local")
                .hide(true)
                .arg(Arg::new("request").long("request").required(true)),
        )
        .subcommand(
            Clap::new("restore-probe")
                .hide(true)
                .arg(Arg::new("request").long("request").required(true)),
        )
        .subcommand(
            Clap::new("restore-local")
                .hide(true)
                .arg(Arg::new("request").long("request").required(true)),
        )
        // In a runner pod: one task from a request file (deleted once read), its outcome printed as JSON.
        .subcommand(
            Clap::new("task-request")
                .hide(true)
                .arg(Arg::new("request").long("request").required(true)),
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
        [
            "self",
            "update" | "run-task" | "task" | "task-request" | "composer-plugins"
        ]
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
        Some((which @ ("backup-local" | "restore-probe" | "restore-local"), request)) => {
            let path =
                std::path::Path::new(request.get_one::<String>("request").expect("required"));
            match which {
                "backup-local" => crate::backup_cmd::pod_backup(path),
                "restore-probe" => crate::backup_cmd::pod_restore_probe(path),
                _ => crate::backup_cmd::pod_restore(path),
            }
        }
        Some(("shelf-index", index)) => {
            let dir = std::path::Path::new(index.get_one::<String>("dir").expect("required"));
            let mut out = std::io::BufWriter::new(std::io::stdout().lock());
            match crate::extensions::write_index(dir, &mut out)
                .and_then(|()| std::io::Write::flush(&mut out).map_err(|e| e.to_string()))
            {
                Ok(()) => 0,
                Err(message) => {
                    eprintln!("speedyspec: {message}");
                    1
                }
            }
        }
        Some(("shelf-swap", swap)) => shelf_swap(
            std::path::Path::new(swap.get_one::<String>("dir").expect("required")),
            swap.get_one::<String>("slug").expect("required"),
            swap.get_one::<String>("mode").expect("required"),
        ),
        Some(("db", db)) => run_db_request(
            std::path::Path::new(db.get_one::<String>("request").expect("required")),
            db.get_one::<String>("operation").expect("required"),
        ),
        Some(("task-request", request)) => run_task_request(std::path::Path::new(
            request.get_one::<String>("request").expect("required"),
        )),
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

/// `self task-request`: in a runner pod, the task a request file describes. The file holds the task's input, which
/// may be a secret, so it is deleted as soon as it is read; the runner's own files go to the pod's private directory.
/// The outcome is printed as one line of JSON: `{"ok": true, "outcome": ...}` or `{"ok": false, "error": ...}`.
fn run_task_request(path: &std::path::Path) -> i32 {
    let text = std::fs::read_to_string(path);
    let _ = std::fs::remove_file(path);
    let reply = |reply: serde_json::Value| {
        say(&reply.to_string());
        0
    };
    let request: serde_json::Value = match text
        .map_err(|e| e.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
    {
        Ok(request) => request,
        Err(e) => {
            return reply(
                serde_json::json!({"ok": false, "error": format!("the task request: {e}")}),
            );
        }
    };
    if let Some(dir) = path.parent() {
        // SAFETY: nothing else runs yet; the runner's temporary files then stay in the pod's private directory.
        unsafe { std::env::set_var("TMPDIR", dir) };
    }
    let pairs = |value: &serde_json::Value| -> Vec<(String, String)> {
        value
            .as_object()
            .map(|map| {
                map.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let runtime = speedyspec_driver::AppRuntime {
        entry: request["entry"].as_str().map(str::to_owned),
        ini: pairs(&request["ini"]),
        server: pairs(&request["server"]),
        ..Default::default()
    };
    let task = speedyspec_driver::AppTask {
        file: request["file"].as_str().unwrap_or_default().into(),
        input: request["input"].clone(),
        loads_app: request["loads_app"].as_bool().unwrap_or(false),
        timeout: std::time::Duration::from_secs(request["timeout"].as_u64().unwrap_or(600)),
    };
    let root = std::path::PathBuf::from(request["root"].as_str().unwrap_or("/"));
    match crate::runner::run(&root, &runtime, &task) {
        Ok(outcome) => reply(serde_json::json!({"ok": true, "outcome": outcome.to_json()})),
        Err(error) => reply(serde_json::json!({"ok": false, "error": error.0})),
    }
}

/// `self db`: in a runner pod, an `app db` operation on the connection the request file holds. The file holds the
/// password, so it is deleted as soon as it is read.
fn run_db_request(path: &std::path::Path, operation: &str) -> i32 {
    let text = std::fs::read_to_string(path);
    let _ = std::fs::remove_file(path);
    let request: serde_json::Value = match text
        .map_err(|e| e.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
    {
        Ok(request) => request,
        Err(e) => {
            eprintln!("speedyspec: the database request: {e}");
            return 1;
        }
    };
    let settings = match speedyspec_driver::database_connection_from_json(&request) {
        Ok(settings) => settings,
        Err(e) => {
            eprintln!("speedyspec: the database request: {e}");
            return 1;
        }
    };
    let mut connection = match speedyspec_db::Connection::open(&settings) {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("speedyspec: {error}");
            return 1;
        }
    };
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    match operation {
        "export" => match connection.export(&mut out, &speedyspec_db::ExportOptions::default()) {
            Ok(()) => match std::io::Write::flush(&mut out) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("speedyspec: could not send the dump: {e}");
                    1
                }
            },
            Err(error) => {
                eprintln!("speedyspec: {error}");
                1
            }
        },
        "import" => crate::db_cmd::import(
            &mut connection,
            &mut std::io::BufReader::new(std::io::stdin().lock()),
            "the dump",
            &settings.database,
        ),
        operation => crate::db_cmd::operate(
            &mut connection,
            operation,
            request["sql"].as_str(),
            &mut out,
        ),
    }
}

/// `self shelf-swap`: in a runner pod, the file work of a plugin or theme install, update or delete on the shelf
/// `dir`. The new directory comes in as a tar on stdin, unpacked beside the others first, so a failure leaves what
/// was there.
fn shelf_swap(dir: &std::path::Path, slug: &str, mode: &str) -> i32 {
    use crate::extensions::{SwapMode, swap_in};
    let fail = |message: String| {
        eprintln!("speedyspec: {message}");
        1
    };
    let Some(mode) = SwapMode::parse(mode) else {
        return fail(format!("no swap mode {mode}"));
    };
    if let Some(problem) = speedyspec_driver::slug_problem(slug) {
        return fail(format!("{slug:?}: a slug {problem}"));
    }
    if mode == SwapMode::Delete {
        return match swap_in(dir, slug, mode, None) {
            Ok(()) => 0,
            Err(message) => fail(message),
        };
    }
    if let Err(e) = std::fs::create_dir_all(dir) {
        return fail(format!("cannot create {}: {e}", dir.display()));
    }
    let work = match tempfile::Builder::new()
        .prefix(".speedyspec-")
        .tempdir_in(dir)
    {
        Ok(work) => work,
        Err(e) => return fail(format!("cannot write in {}: {e}", dir.display())),
    };
    let new = work.path().join("new");
    // tar refuses entries that would land outside `new`.
    if let Err(e) = tar::Archive::new(std::io::stdin().lock()).unpack(&new) {
        return fail(format!("cannot unpack the new {slug}: {e}"));
    }
    match swap_in(dir, slug, mode, Some(&new)) {
        Ok(()) => 0,
        Err(message) => fail(message),
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
