//! `speedyspec app backup` and `app restore`: an install's files and database in one archive, and back. What the
//! backup holds comes from the app's plugin (`speedyspec_app_backup`, a `BackupSpec`); the archive, the dump and the
//! move to a new URL and prefix are done in Rust (`speedyspec-backup`). Zip installs only, for now.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use clap::{Arg, ArgAction, ArgMatches, Command as Clap};
use speedyspec_backup::flow::{SiteDatabase, backup, has_files, plan_move, read_manifest, restore};
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
        // A database without the table (an install whose app has not set itself up yet) has no site URL.
        let table = format!("{}{}", self.prefix, at.table);
        let tables = self.connection.tables().map_err(|e| e.to_string())?;
        if !tables.iter().any(|t| t.name == table) {
            return Ok(None);
        }
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

/// Backs up install `name` to `to`; `prefix` backs up the tables under another prefix than the install's own.
pub fn backup_to(
    runtime: &Runtime,
    name: &str,
    to: &Path,
    prefix: Option<&str>,
) -> Result<(), String> {
    let target = Target::find(name)?;
    let root = target.install.root.clone().ok_or_else(|| {
        format!("install {name} is not on this machine; backups are for zip installs for now")
    })?;
    let spec = spec(runtime, &target.install)?;
    let mut settings = app_target::database(runtime, &target)?;
    if let Some(prefix) = prefix {
        settings.table_prefix = prefix.to_owned();
    }
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
    for warning in backup(&root, &spec, &mut db, manifest, to)? {
        eprintln!("speedyspec: warning: {warning}");
    }
    Ok(())
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
    match backup_to(runtime, name, &to, None) {
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

pub fn restore_clap() -> Clap {
    Clap::new("restore")
        .about("Put a backup back, onto the same site or onto another URL and table prefix")
        .arg(
            Arg::new("file")
                .required(true)
                .value_name("FILE")
                .help("The backup, as app backup wrote it"),
        )
        .arg(install_arg())
        .arg(
            Arg::new("url")
                .long("url")
                .value_name("URL")
                .help("The target site's URL, when it differs from the backup's"),
        )
        .arg(
            Arg::new("table-prefix")
                .long("table-prefix")
                .value_name("PREFIX")
                .help("The table prefix to restore under (default: the target's own)"),
        )
        .arg(
            Arg::new("yes")
                .long("yes")
                .action(ArgAction::SetTrue)
                .help("Do not ask before restoring over a site that has content"),
        )
}

fn fail(message: &str) -> i32 {
    eprintln!("speedyspec: {message}");
    1
}

/// `app restore <file> --install <name> [--url <new>] [--table-prefix <prefix>] [--yes]`.
pub fn run_restore(runtime: &Runtime, matches: &ArgMatches) -> i32 {
    let name = matches.get_one::<String>("install").expect("required");
    let archive = PathBuf::from(matches.get_one::<String>("file").expect("required"));
    // 1. The archive.
    let manifest = match read_manifest(&archive) {
        Ok(manifest) => manifest,
        Err(message) => return fail(&message),
    };
    // 2. The target.
    let target = match Target::find(name) {
        Ok(target) => target,
        Err(message) => return fail(&message),
    };
    let Some(root) = target.install.root.clone() else {
        return fail(&format!(
            "install {name} is not on this machine; restores are for zip installs for now"
        ));
    };
    if manifest.app != target.install.app {
        return fail(&format!(
            "the backup is of a {} install, and {name} is {}",
            manifest.app, target.install.app
        ));
    }
    if manifest.parts != target.install.parts {
        eprintln!(
            "speedyspec: warning: the backup has parts [{}] and {name} has [{}]",
            manifest.parts.join(", "),
            target.install.parts.join(", ")
        );
    }
    let spec = match spec(runtime, &target.install) {
        Ok(spec) => spec,
        Err(message) => return fail(&message),
    };
    let settings = match app_target::database(runtime, &target) {
        Ok(settings) => settings,
        Err(message) => return fail(&message),
    };
    // 3. The move.
    let to_prefix = match matches.get_one::<String>("table-prefix") {
        Some(prefix) if !speedyspec_driver::is_sql_name(prefix) => {
            eprintln!("speedyspec: --table-prefix must be letters, digits and underscores");
            return 2;
        }
        Some(prefix) => {
            if *prefix != settings.table_prefix {
                eprintln!(
                    "speedyspec: warning: {name}'s configuration names the prefix {}; it is kept as it is, so update it to {prefix}",
                    settings.table_prefix
                );
            }
            prefix.clone()
        }
        None => settings.table_prefix.clone(),
    };
    let plan = match plan_move(
        &manifest,
        &to_prefix,
        matches.get_one::<String>("url").map(String::as_str),
        &spec,
    ) {
        Ok(plan) => plan,
        Err(message) => return fail(&message),
    };
    // The tables the restore writes are those under the prefix it restores to.
    let mut written = settings.clone();
    written.table_prefix = to_prefix.clone();
    let mut db = match open(&written) {
        Ok(db) => db,
        Err(message) => return fail(&message),
    };
    // Restored onto a site with another URL and no --url, the backup's URL would come back with it.
    if matches.get_one::<String>("url").is_none()
        && let (Some(at), Some(theirs)) = (&spec.site_url, &manifest.site_url)
        && let Ok(Some(ours)) = db.site_url(at)
        && ours.trim_end_matches('/') != theirs.trim_end_matches('/')
    {
        eprintln!(
            "speedyspec: warning: the backup's site URL is {theirs} and {name}'s is {ours}; it is restored as {theirs} \
             unless you pass --url {ours}"
        );
    }
    // 4. Ask, when anything the restore replaces is there: the tables under that prefix, or the install's files.
    let has_content = match db.has_prefixed_tables() {
        Ok(found) => found || has_files(&root, &spec),
        Err(message) => return fail(&message),
    };
    if has_content {
        let question =
            format!("Restore over the site in {name}? Its files and database are replaced. [y/N]");
        if let Err(code) = crate::prompt::confirm(matches, &question) {
            return code;
        }
    }
    // 5. What is there now, so the restore can be undone.
    let undo = if has_content {
        let Some(path) = default_path(name, "-before-restore") else {
            return fail(
                "there is no speedyspec home to keep the backup of what is there in; set SPEEDYSPEC_HOME",
            );
        };
        if let Err(message) = backup_to(runtime, name, &path, Some(&to_prefix)) {
            return fail(&format!(
                "could not back up what is there first, so nothing was restored: {message}"
            ));
        }
        eprintln!(
            "speedyspec: what was there is backed up to {}",
            path.display()
        );
        Some(path)
    } else {
        None
    };
    // 6 to 8. Unpack, import, swap.
    let mut warnings = Vec::new();
    let result = restore(&archive, &root, &spec, &mut db, &plan, &mut warnings);
    for warning in &warnings {
        eprintln!("speedyspec: warning: {warning}");
    }
    match result {
        Ok(()) => {
            let _ = writeln!(
                std::io::stdout().lock(),
                "restored {} into {name}",
                archive.display()
            );
            0
        }
        Err(error) => {
            let undo = match (&undo, error.database_changed) {
                (Some(path), true) => format!(
                    "; to undo it, run speedyspec app restore {} --install {name} --yes",
                    path.display()
                ),
                _ => String::new(),
            };
            fail(&format!("{}{undo}", error.message))
        }
    }
}
