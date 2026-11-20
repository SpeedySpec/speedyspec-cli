//! `speedyspec app db export | import | query | tables | size | check --install <name>`: the
//! database of an install, found through its app's plugin (`speedyspec_app_database`) and worked
//! on in Rust. The password is never printed.

use std::fs::File;
use std::io::{BufReader, BufWriter, IsTerminal, Write};
use std::path::Path;

use clap::{Arg, ArgAction, ArgMatches, Command as Clap};
use speedyspec_db::{Connection, ExportOptions, QueryOutcome};

use crate::app_target::{self, Target};
use crate::boot::Runtime;
use crate::prompt::confirm;
use crate::remote::RunnerOptions;

pub fn clap() -> Clap {
    let yes = Arg::new("yes")
        .long("yes")
        .action(ArgAction::SetTrue)
        .help("Do not ask for confirmation");
    Clap::new("db")
        .about("Work with an install's database")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Clap::new("export")
                .about("Write a dump of the install's tables")
                .args(crate::app_target::install_args())
                .arg(
                    Arg::new("to")
                        .long("to")
                        .value_name("FILE")
                        .help("Write it to this file (readable only by you) instead of stdout"),
                )
                .arg(
                    Arg::new("force")
                        .long("force")
                        .action(ArgAction::SetTrue)
                        .help("Replace the file if it exists"),
                ),
        )
        .subcommand(
            Clap::new("import")
                .about("Run a dump's statements against the install's database")
                .args(crate::app_target::install_args())
                .arg(
                    Arg::new("file")
                        .required(true)
                        .value_name("FILE")
                        .help("The dump, or - for stdin"),
                )
                .arg(yes),
        )
        .subcommand(
            Clap::new("query")
                .about("Run one SQL statement and print what it returns")
                .args(crate::app_target::install_args())
                .arg(Arg::new("sql").required(true).value_name("SQL")),
        )
        .subcommand(
            Clap::new("tables")
                .about("List the tables: name, rows, data bytes, index bytes")
                .args(crate::app_target::install_args()),
        )
        .subcommand(
            Clap::new("size")
                .about("Print the data and index bytes")
                .args(crate::app_target::install_args()),
        )
        .subcommand(
            Clap::new("check")
                .about("Check each table: name, status, message")
                .args(crate::app_target::install_args()),
        )
}

fn fail(message: impl std::fmt::Display) -> i32 {
    eprintln!("speedyspec: {message}");
    1
}

pub fn run(runtime: &Runtime, matches: &ArgMatches) -> i32 {
    let (command, sub) = matches.subcommand().expect("clap requires a db subcommand");
    let name = sub.get_one::<String>("install").expect("required");
    let target = match Target::from_matches(name, sub) {
        Ok(target) => target,
        Err(message) => return fail(message),
    };
    // A Kubernetes install's database is reached from inside its cluster, through a runner pod beside the app.
    let remote = match &target.cluster {
        None => None,
        Some(_) => match crate::remote::open(runtime, &target, &RunnerOptions::from_matches(sub)) {
            Ok(remote) => Some(remote),
            Err(message) => return fail(message),
        },
    };
    let settings = match app_target::database(runtime, &target, remote.as_ref()) {
        Ok(settings) => settings,
        Err(message) => return fail(message),
    };
    if command == "import" {
        let question = format!(
            "This replaces tables in database {} on {}. Continue? [y/N]",
            settings.database, settings.host
        );
        if let Err(code) = confirm(sub, &question) {
            return code;
        }
    }
    if let Some(remote) = &remote {
        return in_pod(remote, command, sub, &settings);
    }
    if let Some(warning) = speedyspec_db::unencrypted_warning(&settings) {
        eprintln!("speedyspec: warning: {warning}");
    }
    let mut connection = match Connection::open(&settings) {
        Ok(connection) => connection,
        Err(error) => return fail(error),
    };
    match command {
        "export" => export_to_file_or_stdout(sub, &settings.database, |out| {
            connection
                .export(out, &ExportOptions::default())
                .map_err(|e| e.to_string())
        }),
        "import" => {
            let source = sub.get_one::<String>("file").expect("required");
            match open_dump(source) {
                Ok(mut input) => import(&mut connection, &mut input, source, &settings.database),
                Err(message) => fail(message),
            }
        }
        command => {
            let sql = sub
                .try_get_one::<String>("sql")
                .ok()
                .flatten()
                .map(String::as_str);
            let mut out = BufWriter::new(std::io::stdout().lock());
            operate(&mut connection, command, sql, &mut out)
        }
    }
}

/// The dump to import: a file, or stdin for `-`.
fn open_dump(source: &str) -> Result<Box<dyn std::io::BufRead + Send>, String> {
    if source == "-" {
        if std::io::stdin().is_terminal() {
            return Err("give a dump file, or pipe one to stdin with -".to_owned());
        }
        return Ok(Box::new(BufReader::new(std::io::stdin())));
    }
    File::open(source)
        .map(|file| Box::new(BufReader::new(file)) as Box<dyn std::io::BufRead + Send>)
        .map_err(|e| format!("cannot read {source}: {e}"))
}

/// The read-only operations (`query`, `tables`, `size`, `check`), their output written to `out`. Shared with the
/// runner pod's `self db`.
pub fn operate(
    connection: &mut Connection,
    command: &str,
    sql: Option<&str>,
    out: &mut dyn Write,
) -> i32 {
    match command {
        "query" => query(connection, sql.unwrap_or_default(), out),
        "tables" => tables(connection, out),
        "size" => size(connection, out),
        "check" => check(connection, out),
        _ => fail(format!("app db has no operation {command}")),
    }
}

/// `app db` on a Kubernetes install: the operation runs in the runner pod, which reads the connection (and the SQL)
/// from a private request file it deletes at once. A dump streams in and out through exec.
fn in_pod(
    remote: &crate::remote::Remote<'_>,
    command: &str,
    matches: &ArgMatches,
    settings: &speedyspec_driver::DatabaseConnection,
) -> i32 {
    let sql = matches.try_get_one::<String>("sql").ok().flatten().cloned();
    let request = match remote.connection_request(settings) {
        Ok(mut request) => {
            request["sql"] = serde_json::json!(sql);
            request
        }
        Err(message) => return fail(message),
    };
    let path = remote.private_path("db.json");
    if let Err(error) = remote
        .pod
        .put(&path, &mut request.to_string().as_bytes(), 0o600)
    {
        return fail(error.0);
    }
    let args = ["self", "db", "--request", path.as_str(), command];
    let mut stderr = std::io::stderr();
    let code = match command {
        "export" => export_to_file_or_stdout(matches, &settings.database, |out| {
            match remote.pod.run(&args, None, out, &mut stderr) {
                Ok(0) => Ok(()),
                Ok(code) => Err(format!("the export in the runner pod failed (exit {code})")),
                Err(error) => Err(error.0),
            }
        }),
        "import" => {
            let source = matches.get_one::<String>("file").expect("required");
            match open_dump(source) {
                Ok(mut input) => {
                    let mut out = std::io::stdout();
                    match remote
                        .pod
                        .run(&args, Some(&mut input), &mut out, &mut stderr)
                    {
                        Ok(code) => code,
                        Err(error) => fail(format!("{source}: {}", error.0)),
                    }
                }
                Err(message) => fail(message),
            }
        }
        _ => {
            let mut out = BufWriter::new(std::io::stdout().lock());
            match remote.pod.run(&args, None, &mut out, &mut stderr) {
                Ok(code) => code,
                Err(error) => fail(error.0),
            }
        }
    };
    // `self db` deletes it on reading; this is for when it never ran.
    let _ = remote.pod.remove(&path);
    code
}

/// Writes a dump with `write` to the command's `--to` file (private, put in place only when complete) or to stdout.
fn export_to_file_or_stdout(
    matches: &ArgMatches,
    database: &str,
    write: impl FnOnce(&mut dyn Write) -> Result<(), String>,
) -> i32 {
    let Some(to) = matches.get_one::<String>("to") else {
        let mut out = BufWriter::new(std::io::stdout().lock());
        return match write(&mut out).and_then(|()| out.flush().map_err(|e| e.to_string())) {
            Ok(()) => 0,
            Err(error) => fail(error),
        };
    };
    let path = Path::new(to);
    if path.exists() && !matches.get_flag("force") {
        return fail(format!("{to} already exists; give --force to replace it"));
    }
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Written beside the target and renamed into place, so a failed export leaves nothing half
    // written and an existing file is replaced only by a complete dump.
    let file = match tempfile::Builder::new()
        .prefix(".speedyspec-export-")
        .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))
        .tempfile_in(dir)
    {
        Ok(file) => file,
        Err(e) => return fail(format!("cannot write in {}: {e}", dir.display())),
    };
    let mut out = BufWriter::new(file);
    if let Err(error) = write(&mut out) {
        return fail(error);
    }
    let file = match out.into_inner() {
        Ok(file) => file,
        Err(e) => return fail(format!("could not write {to}: {}", e.error())),
    };
    if let Err(e) = file.persist(path) {
        return fail(format!("could not write {to}: {}", e.error));
    }
    eprintln!("speedyspec: exported database {database} to {to}; it contains the site's data");
    0
}

/// Runs a dump's statements; shared with the runner pod's `self db`.
pub fn import(
    connection: &mut Connection,
    input: &mut dyn std::io::BufRead,
    source: &str,
    database: &str,
) -> i32 {
    match connection.import(input) {
        Ok(applied) => {
            eprintln!("speedyspec: imported {applied} statement(s) into database {database}");
            0
        }
        Err(error) => fail(format!("{source}: {error}")),
    }
}

fn query(connection: &mut Connection, sql: &str, out: &mut dyn Write) -> i32 {
    let code = match connection.query(sql, out) {
        Ok(QueryOutcome::Rows(_)) => 0,
        Ok(QueryOutcome::Affected(n)) => {
            let _ = writeln!(out, "{n} rows affected");
            0
        }
        Err(error) => fail(error),
    };
    let _ = out.flush();
    code
}

fn tables(connection: &mut Connection, out: &mut dyn Write) -> i32 {
    match connection.tables() {
        Ok(tables) => {
            for t in tables {
                let _ = writeln!(
                    out,
                    "{}\t{}\t{}\t{}",
                    t.name, t.rows_estimate, t.data_bytes, t.index_bytes
                );
            }
            let _ = out.flush();
            0
        }
        Err(error) => fail(error),
    }
}

fn size(connection: &mut Connection, out: &mut dyn Write) -> i32 {
    match connection.size() {
        Ok((data, index)) => {
            let _ = writeln!(out, "data\t{data}\nindex\t{index}\ntotal\t{}", data + index);
            let _ = out.flush();
            0
        }
        Err(error) => fail(error),
    }
}

fn check(connection: &mut Connection, out: &mut dyn Write) -> i32 {
    match connection.check() {
        Ok(rows) => {
            let mut code = 0;
            for (table, status, message) in rows {
                if status != "OK" {
                    code = 1;
                }
                let _ = writeln!(out, "{table}\t{status}\t{message}");
            }
            let _ = out.flush();
            code
        }
        Err(error) => fail(error),
    }
}
