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

fn install_arg() -> Arg {
    Arg::new("install")
        .long("install")
        .required(true)
        .value_name("NAME")
        .help("The install, as app installs lists it")
}

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
                .arg(install_arg())
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
                .arg(install_arg())
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
                .arg(install_arg())
                .arg(Arg::new("sql").required(true).value_name("SQL")),
        )
        .subcommand(
            Clap::new("tables")
                .about("List the tables: name, rows, data bytes, index bytes")
                .arg(install_arg()),
        )
        .subcommand(
            Clap::new("size")
                .about("Print the data and index bytes")
                .arg(install_arg()),
        )
        .subcommand(
            Clap::new("check")
                .about("Check each table: name, status, message")
                .arg(install_arg()),
        )
}

fn fail(message: impl std::fmt::Display) -> i32 {
    eprintln!("speedyspec: {message}");
    1
}

pub fn run(runtime: &Runtime, matches: &ArgMatches) -> i32 {
    let (command, sub) = matches.subcommand().expect("clap requires a db subcommand");
    let name = sub.get_one::<String>("install").expect("required");
    let settings =
        match Target::find(name).and_then(|target| app_target::database(runtime, &target)) {
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
    if let Some(warning) = speedyspec_db::unencrypted_warning(&settings) {
        eprintln!("speedyspec: warning: {warning}");
    }
    let mut connection = match Connection::open(&settings) {
        Ok(connection) => connection,
        Err(error) => return fail(error),
    };
    match command {
        "export" => export(&mut connection, sub, &settings.database),
        "import" => import(&mut connection, sub, &settings.database),
        "query" => query(&mut connection, sub),
        "tables" => tables(&mut connection),
        "size" => size(&mut connection),
        "check" => check(&mut connection),
        _ => unreachable!("clap knows the db subcommands"),
    }
}

fn export(connection: &mut Connection, matches: &ArgMatches, database: &str) -> i32 {
    let Some(to) = matches.get_one::<String>("to") else {
        let mut out = BufWriter::new(std::io::stdout().lock());
        return match connection.export(&mut out, &ExportOptions::default()) {
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
    if let Err(error) = connection.export(&mut out, &ExportOptions::default()) {
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

fn import(connection: &mut Connection, matches: &ArgMatches, database: &str) -> i32 {
    let source = matches.get_one::<String>("file").expect("required");
    let result = if source == "-" {
        if std::io::stdin().is_terminal() {
            return fail("give a dump file, or pipe one to stdin with -");
        }
        connection.import(std::io::stdin().lock())
    } else {
        match File::open(source) {
            Ok(file) => connection.import(BufReader::new(file)),
            Err(e) => return fail(format!("cannot read {source}: {e}")),
        }
    };
    match result {
        Ok(applied) => {
            eprintln!("speedyspec: imported {applied} statement(s) into database {database}");
            0
        }
        Err(error) => fail(format!("{source}: {error}")),
    }
}

fn query(connection: &mut Connection, matches: &ArgMatches) -> i32 {
    let sql = matches.get_one::<String>("sql").expect("required");
    let mut out = BufWriter::new(std::io::stdout().lock());
    match connection.query(sql, &mut out) {
        Ok(QueryOutcome::Rows(_)) => 0,
        Ok(QueryOutcome::Affected(n)) => {
            let _ = writeln!(out, "{n} rows affected");
            0
        }
        Err(error) => fail(error),
    }
}

fn tables(connection: &mut Connection) -> i32 {
    match connection.tables() {
        Ok(tables) => {
            let mut out = std::io::stdout().lock();
            for t in tables {
                let _ = writeln!(
                    out,
                    "{}\t{}\t{}\t{}",
                    t.name, t.rows_estimate, t.data_bytes, t.index_bytes
                );
            }
            0
        }
        Err(error) => fail(error),
    }
}

fn size(connection: &mut Connection) -> i32 {
    match connection.size() {
        Ok((data, index)) => {
            let _ = writeln!(
                std::io::stdout().lock(),
                "data\t{data}\nindex\t{index}\ntotal\t{}",
                data + index
            );
            0
        }
        Err(error) => fail(error),
    }
}

fn check(connection: &mut Connection) -> i32 {
    match connection.check() {
        Ok(rows) => {
            let mut out = std::io::stdout().lock();
            let mut code = 0;
            for (table, status, message) in rows {
                if status != "OK" {
                    code = 1;
                }
                let _ = writeln!(out, "{table}\t{status}\t{message}");
            }
            code
        }
        Err(error) => fail(error),
    }
}
