//! `speedyspec app backup` and `app restore`: an install's files and database in one archive, and back. What the
//! backup holds comes from the app's plugin (`speedyspec_app_backup`, a `BackupSpec`); the archive, the dump and the
//! move to a new URL and prefix are done in Rust (`speedyspec-backup`). Zip installs only, for now.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use clap::{Arg, ArgAction, ArgMatches, Command as Clap};
use speedyspec_backup::flow::{SiteDatabase, backup};
use speedyspec_backup::manifest::Manifest;
use speedyspec_backup::rewrite::{Move, rewrite};
use speedyspec_db::{Connection, ExportOptions};
use speedyspec_driver::{
    BackupSpec, DatabaseConnection, Install, Kind, ObjectReader, SiteUrl, decode_backup_spec,
};
use speedyspec_hooks::Value;

use crate::app_target::{self, Target, ask_with};
use crate::boot::{PhpReader, Runtime};

const HOOK_BACKUP: &str = "speedyspec_app_backup";

fn install_arg() -> Arg {
    Arg::new("install")
        .long("install")
        .required(true)
        .value_name("NAME")
        .help("The install, as app installs lists it")
}

pub fn backup_clap() -> Clap {
    Clap::new("backup")
        .about("Write the install's files and database to one archive")
        .arg(install_arg())
        .arg(Arg::new("to").long("to").value_name("FILE").help(
            "The archive to write (default: <speedyspec home>/backups/<install>-<time>.tar.gz)",
        ))
        .arg(
            Arg::new("force")
                .long("force")
                .action(ArgAction::SetTrue)
                .help("Replace the file if it exists"),
        )
}

/// An open connection, as backups and restores use it.
pub struct LiveDatabase {
    connection: Connection,
    prefix: String,
}

impl SiteDatabase for LiveDatabase {
    fn export(&mut self, out: &mut dyn Write) -> Result<(), String> {
        self.connection
            .export(out, &ExportOptions::default())
            .map_err(|e| e.to_string())
    }

    fn import(
        &mut self,
        input: &mut dyn BufRead,
        plan: &Move,
        warnings: &mut Vec<String>,
    ) -> Result<u64, String> {
        let mut each = |statement: &[u8]| rewrite(statement, plan, warnings).into_owned();
        self.connection
            .import_rewriting(input, &mut each)
            .map_err(|e| e.to_string())
    }

    fn has_prefixed_tables(&mut self) -> Result<bool, String> {
        Ok(!self
            .connection
            .tables()
            .map_err(|e| e.to_string())?
            .is_empty())
    }

    fn site_url(&mut self, at: &SiteUrl) -> Result<Option<String>, String> {
        self.connection
            .site_value(&at.table, &at.key_column, &at.key, &at.value_column)
            .map_err(|e| e.to_string())
    }

    fn prefix(&self) -> String {
        self.prefix.clone()
    }
}

/// What the install's plugin says its backups hold.
pub fn spec(runtime: &Runtime, install: &Install) -> Result<BackupSpec, String> {
    let check =
        |value: &Value, reader: &dyn ObjectReader| decode_backup_spec(value, reader).map(drop);
    let answer = ask_with(runtime, install, &[], HOOK_BACKUP, Value::Null, check)?;
    if matches!(answer, Value::Null) {
        return Err(format!(
            "no plugin says what a backup of {} holds; install the plugin for that app",
            install.app
        ));
    }
    decode_backup_spec(&answer, &PhpReader(&runtime.php)).map_err(|e| e.to_string())
}

/// Opens the install's database, with the same warning as `app db` for a connection that leaves this machine.
pub fn open(settings: &DatabaseConnection) -> Result<LiveDatabase, String> {
    if !speedyspec_db::stays_on_this_machine(settings) {
        eprintln!(
            "speedyspec: warning: the connection to {}:{} is not encrypted (speedyspec does not use TLS yet); \
             the login and the data cross the network as they are",
            settings.host, settings.port
        );
    }
    let connection = Connection::open(settings).map_err(|e| e.to_string())?;
    Ok(LiveDatabase {
        connection,
        prefix: settings.table_prefix.clone(),
    })
}

/// The install's plugins and themes, as far as its app's plugin can tell; nothing when it cannot.
fn extensions(runtime: &Runtime, name: &str) -> Vec<(String, String, Option<String>)> {
    let mut found = Vec::new();
    for kind in [Kind::Plugin, Kind::Theme] {
        if let Ok(shelf) = crate::extensions::Shelf::open(runtime, name, kind)
            && let Ok(all) = shelf.list()
        {
            found.extend(
                all.into_iter()
                    .map(|e| (kind.as_str().to_owned(), e.slug, e.version)),
            );
        }
    }
    found
}

/// `<home>/backups/<name>-<time>.tar.gz`, or `None` when there is no speedyspec home.
pub fn default_path(name: &str, label: &str) -> Option<PathBuf> {
    let home = speedyspec_plugin::home_dir().dir?;
    let stamp = crate::installs::now_rfc3339().replace(':', "-");
    Some(
        home.join("backups")
            .join(format!("{name}{label}-{stamp}.tar.gz")),
    )
}

/// Makes the directory a backup goes into, readable only by its owner when it is new.
pub fn backup_dir(path: &Path) -> Result<(), String> {
    let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return Ok(());
    };
    if !dir.exists() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// Backs up install `name` to `to`.
pub fn backup_to(runtime: &Runtime, name: &str, to: &Path) -> Result<(), String> {
    let target = Target::find(name)?;
    let root = target.install.root.clone().ok_or_else(|| {
        format!("install {name} is not on this machine; backups are for zip installs for now")
    })?;
    let spec = spec(runtime, &target.install)?;
    let settings = app_target::database(runtime, &target)?;
    let mut db = open(&settings)?;
    let record = crate::installs::read(name)?;
    let manifest = Manifest {
        app: record.app.clone(),
        parts: record.parts.clone(),
        cli_version: crate::self_cmd::VERSION.to_owned(),
        app_version: spec.app_version.clone(),
        extensions: extensions(runtime, name),
        engine: settings.engine.clone(),
        table_prefix: String::new(),
        site_url: None,
        source_install: record.name.clone(),
        source_driver: record.driver.clone(),
        created_at: crate::installs::now_rfc3339(),
    };
    backup_dir(to)?;
    backup(&root, &spec, &mut db, manifest, to)
}

/// `app backup --install <name> [--to <file>] [--force]`.
pub fn run_backup(runtime: &Runtime, matches: &ArgMatches) -> i32 {
    let name = matches.get_one::<String>("install").expect("required");
    let to = match matches.get_one::<String>("to") {
        Some(to) => PathBuf::from(to),
        None => match default_path(name, "") {
            Some(path) => path,
            None => {
                eprintln!(
                    "speedyspec: there is no speedyspec home to keep backups in; give --to or set SPEEDYSPEC_HOME"
                );
                return 1;
            }
        },
    };
    if to.exists() && !matches.get_flag("force") {
        eprintln!(
            "speedyspec: {} already exists; use --force to replace it",
            to.display()
        );
        return 1;
    }
    match backup_to(runtime, name, &to) {
        Ok(()) => {
            eprintln!(
                "speedyspec: the backup holds the site's database and secrets (such as its configuration); keep it private"
            );
            let _ = writeln!(std::io::stdout().lock(), "{}", to.display());
            0
        }
        Err(message) => {
            eprintln!("speedyspec: {message}");
            1
        }
    }
}
