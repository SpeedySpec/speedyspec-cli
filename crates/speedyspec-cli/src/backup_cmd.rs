//! `speedyspec app backup` and `app restore`: an install's files and database in one archive, and back. What the
//! backup holds comes from the app's plugin (`speedyspec_app_backup`, a `BackupSpec`); the archive, the dump and the
//! move to a new URL and prefix are done in Rust (`speedyspec-backup`). A Kubernetes install's are done the same way
//! in a runner pod beside its app (`self backup-local`, `self restore-probe` and `self restore-local`), the archive
//! streamed out to this machine or in from it; a backup is the same whichever driver made it.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use clap::{Arg, ArgAction, ArgMatches, Command as Clap};
use serde_json::{Value as Json, json};
use speedyspec_backup::flow::{SiteDatabase, backup, has_files, plan_move, read_manifest, restore};
use speedyspec_backup::manifest::Manifest;
use speedyspec_backup::rewrite::{Move, rewrite};
use speedyspec_db::{Connection, ExportOptions};
use speedyspec_driver::{
    BackupSpec, DatabaseConnection, Install, Kind, ObjectReader, PrefixedColumn, SiteUrl,
    database_connection_from_json, decode_backup_spec,
};
use speedyspec_hooks::Value;

use crate::app_target::{self, Target, ask_with};
use crate::boot::{PhpReader, Runtime};
use crate::remote::{Remote, RunnerOptions};

const HOOK_BACKUP: &str = "speedyspec_app_backup";

pub fn backup_clap() -> Clap {
    Clap::new("backup")
        .about("Write the install's files and database to one archive")
        .args(crate::app_target::install_args())
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
    if let Some(warning) = speedyspec_db::unencrypted_warning(settings) {
        eprintln!("speedyspec: warning: {warning}");
    }
    let connection = Connection::open(settings).map_err(|e| e.to_string())?;
    Ok(LiveDatabase {
        connection,
        prefix: settings.table_prefix.clone(),
    })
}

/// The install's plugins and themes, as far as its app's plugin can tell; nothing when it cannot.
fn extensions(
    runtime: &Runtime,
    target: &Target,
    remote: Option<&crate::remote::Remote<'_>>,
) -> Vec<(String, String, Option<String>)> {
    let mut found = Vec::new();
    for kind in [Kind::Plugin, Kind::Theme] {
        if let Ok(shelf) = crate::extensions::Shelf::open(runtime, target, remote, kind)
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
    cluster: Option<&BTreeMap<String, String>>,
    options: &RunnerOptions,
    to: &Path,
    prefix: Option<&str>,
) -> Result<(), String> {
    let target = Target::find(name, cluster)?;
    if target.cluster.is_some() {
        let remote = crate::remote::open(runtime, &target, options)?;
        return backup_in_pod(runtime, &target, &remote, to, prefix);
    }
    let root = target.install.root.clone().ok_or_else(|| {
        format!("install {name} is not on this machine; backups are for zip installs for now")
    })?;
    let spec = spec(runtime, &target.install)?;
    let mut settings = app_target::database(runtime, &target, None)?;
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
        extensions: extensions(runtime, &target, None),
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
    match backup_to(
        runtime,
        name,
        app_target::given_cluster_flags(matches).as_ref(),
        &RunnerOptions::from_matches(matches),
        &to,
        None,
    ) {
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
        .args(crate::app_target::install_args())
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
    let cluster = app_target::given_cluster_flags(matches);
    let target = match Target::find(name, cluster.as_ref()) {
        Ok(target) => target,
        Err(message) => return fail(&message),
    };
    let remote = match &target.cluster {
        None => None,
        Some(_) => {
            match crate::remote::open(runtime, &target, &RunnerOptions::from_matches(matches)) {
                Ok(remote) => Some(remote),
                Err(message) => return fail(&message),
            }
        }
    };
    let root = match (&remote, &target.install.root) {
        (Some(_), _) => None,
        (None, Some(root)) => Some(root.clone()),
        (None, None) => {
            return fail(&format!(
                "install {name} is not on this machine; its files are where the {} driver put them",
                target.install.driver
            ));
        }
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
    let settings = match app_target::database(runtime, &target, remote.as_ref()) {
        Ok(settings) => settings,
        Err(message) => return fail(&message),
    };
    let maintenance = match app_target::runtime(runtime, &target) {
        Ok(app) => app.maintenance,
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
    let (has_content, ours) = match (&remote, &root) {
        (Some(remote), _) => match probe_in_pod(remote, &spec, &written) {
            Ok(found) => found,
            Err(message) => return fail(&message),
        },
        (None, Some(root)) => {
            let mut db = match open(&written) {
                Ok(db) => db,
                Err(message) => return fail(&message),
            };
            let ours = match &spec.site_url {
                Some(at) => db.site_url(at).ok().flatten(),
                None => None,
            };
            match db.has_prefixed_tables() {
                Ok(found) => (found || has_files(root, &spec), ours),
                Err(message) => return fail(&message),
            }
        }
        (None, None) => unreachable!("a local install has a root"),
    };
    // Restored onto a site with another URL and no --url, the backup's URL would come back with it.
    if matches.get_one::<String>("url").is_none()
        && let (Some(theirs), Some(ours)) = (&manifest.site_url, &ours)
        && ours.trim_end_matches('/') != theirs.trim_end_matches('/')
    {
        eprintln!(
            "speedyspec: warning: the backup's site URL is {theirs} and {name}'s is {ours}; it is restored as {theirs} \
             unless you pass --url {ours}"
        );
    }
    // 4. Ask, when anything the restore replaces is there: the tables under that prefix, or the install's files.
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
        if let Err(message) = backup_to(
            runtime,
            name,
            cluster.as_ref(),
            &RunnerOptions::from_matches(matches),
            &path,
            Some(&to_prefix),
        ) {
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
    // 6 to 8. Unpack, import, swap, with the site in maintenance.
    let mut warnings = Vec::new();
    let result = match (&remote, &root) {
        (Some(remote), _) => restore_in_pod(
            remote,
            &archive,
            &spec,
            &plan,
            &written,
            maintenance.as_ref(),
            &mut warnings,
        ),
        (None, Some(root)) => match open(&written) {
            Ok(mut db) => {
                let path = maintenance.as_ref().map(|file| root.join(&file.path));
                if let (Some(path), Some(file)) = (&path, &maintenance)
                    && let Err(e) = std::fs::write(path, &file.contents)
                {
                    return fail(&format!("cannot write {}: {e}", path.display()));
                }
                let result = restore(&archive, root, &spec, &mut db, &plan, &mut warnings);
                if let Some(path) = &path {
                    let _ = std::fs::remove_file(path);
                }
                result
            }
            Err(message) => return fail(&message),
        },
        (None, None) => unreachable!("a local install has a root"),
    };
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

/// A `BackupSpec` as the runner pod's request carries it.
fn spec_to_json(spec: &BackupSpec) -> Json {
    json!({
        "include": spec.include,
        "exclude": spec.exclude,
        "preserve": spec.preserve,
        "siteUrl": spec.site_url.as_ref().map(|at| json!({
            "table": at.table, "keyColumn": at.key_column, "key": at.key, "valueColumn": at.value_column,
        })),
        "prefixedColumns": spec.prefixed_columns.iter()
            .map(|c| json!({"table": c.table, "column": c.column, "keys": c.keys}))
            .collect::<Vec<_>>(),
        "appVersion": spec.app_version,
        "keepUrls": spec.keep_urls,
    })
}

fn strings(json: &Json) -> Vec<String> {
    json.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn pairs(json: &Json) -> Vec<(String, String)> {
    json.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|pair| {
                    Some((pair[0].as_str()?.to_owned(), pair[1].as_str()?.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn spec_from_json(json: &Json) -> Result<BackupSpec, String> {
    let text = |value: &Json, what: &str| {
        value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("the backup request has no {what}"))
    };
    let site_url = match &json["siteUrl"] {
        Json::Null => None,
        at => Some(SiteUrl {
            table: text(&at["table"], "siteUrl table")?,
            key_column: text(&at["keyColumn"], "siteUrl keyColumn")?,
            key: text(&at["key"], "siteUrl key")?,
            value_column: text(&at["valueColumn"], "siteUrl valueColumn")?,
        }),
    };
    let mut prefixed_columns = Vec::new();
    for column in json["prefixedColumns"].as_array().into_iter().flatten() {
        prefixed_columns.push(PrefixedColumn {
            table: text(&column["table"], "prefixed column table")?,
            column: text(&column["column"], "prefixed column")?,
            keys: column["keys"].as_array().map(|_| strings(&column["keys"])),
        });
    }
    Ok(BackupSpec {
        include: strings(&json["include"]),
        exclude: strings(&json["exclude"]),
        preserve: strings(&json["preserve"]),
        site_url,
        prefixed_columns,
        app_version: json["appVersion"].as_str().map(str::to_owned),
        keep_urls: pairs(&json["keepUrls"]),
    })
}

fn move_to_json(plan: &Move) -> Json {
    json!({
        "fromPrefix": plan.from_prefix,
        "toPrefix": plan.to_prefix,
        "urls": plan.urls.as_ref().map(|(old, new)| [old, new]),
        "prefixedColumns": plan.prefixed_columns.iter()
            .map(|(table, column, keys)| json!([table, column, keys]))
            .collect::<Vec<_>>(),
        "keepUrls": plan.keep_urls,
    })
}

fn move_from_json(json: &Json) -> Result<Move, String> {
    let text = |value: &Json| {
        value
            .as_str()
            .map(str::to_owned)
            .ok_or("the restore request's move is incomplete")
    };
    Ok(Move {
        from_prefix: text(&json["fromPrefix"])?,
        to_prefix: text(&json["toPrefix"])?,
        urls: match &json["urls"] {
            Json::Null => None,
            urls => Some((text(&urls[0])?, text(&urls[1])?)),
        },
        prefixed_columns: json["prefixedColumns"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                Ok((
                    text(&c[0])?,
                    text(&c[1])?,
                    c[2].as_array().map(|_| strings(&c[2])),
                ))
            })
            .collect::<Result<Vec<_>, &str>>()?,
        keep_urls: pairs(&json["keepUrls"]),
    })
}

/// Where the archive goes in the runner pod while it is made or read: the runner container's own disk, so a large one
/// takes no memory there. It is deleted when the step ends, and goes with the pod in any case.
fn archive_in_pod(remote: &Remote<'_>) -> String {
    remote
        .private_path("archive")
        .replace(speedyspec_driver_kubernetes::RUNNER_DIR, "/tmp")
}

/// Puts `request` in the pod as a private file, runs the CLI there with `args` and the file's path after
/// `--request`, and gives back its stdout as JSON. The request file is deleted when it is read, and again after.
fn pod_request(remote: &Remote<'_>, request: &Json, args: &[&str]) -> Result<Json, String> {
    let path = remote.private_path("request.json");
    remote
        .pod
        .put(&path, &mut request.to_string().as_bytes(), 0o600)
        .map_err(|e| e.0)?;
    let mut all = args.to_vec();
    all.extend_from_slice(&["--request", &path]);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let ran = remote.pod.run(&all, None, &mut out, &mut err);
    let _ = remote.pod.remove(&path);
    let code = ran.map_err(|e| e.0)?;
    serde_json::from_slice(&out).map_err(|_| {
        format!(
            "the runner pod gave no answer (exit {code}): {}",
            String::from_utf8_lossy(&err).trim()
        )
    })
}

/// A backup of a Kubernetes install, made in its runner pod and streamed to `to` on this machine.
fn backup_in_pod(
    runtime: &Runtime,
    target: &Target,
    remote: &Remote<'_>,
    to: &Path,
    prefix: Option<&str>,
) -> Result<(), String> {
    let spec = spec(runtime, &target.install)?;
    let mut settings = app_target::database(runtime, target, Some(remote))?;
    if let Some(prefix) = prefix {
        settings.table_prefix = prefix.to_owned();
    }
    let manifest = Manifest {
        app: target.install.app.clone(),
        parts: target.install.parts.clone(),
        cli_version: crate::self_cmd::VERSION.to_owned(),
        app_version: spec.app_version.clone(),
        extensions: extensions(runtime, target, Some(remote)),
        engine: settings.engine.clone(),
        table_prefix: String::new(),
        site_url: None,
        source_install: target.install.name.clone(),
        source_driver: target.install.driver.clone(),
        created_at: crate::installs::now_rfc3339(),
    };
    let there = archive_in_pod(remote);
    let request = json!({
        "root": remote.workload.root,
        "spec": spec_to_json(&spec),
        "manifest": manifest.to_json(),
        "connection": remote.connection_request(&settings)?,
        "to": there,
    });
    let reply = pod_request(remote, &request, &["self", "backup-local"])?;
    for warning in strings(&reply["warnings"]) {
        eprintln!("speedyspec: warning: {warning}");
    }
    if reply["ok"] != Json::Bool(true) {
        let _ = remote.pod.remove(&there);
        return Err(reply["error"]
            .as_str()
            .unwrap_or("the backup failed")
            .to_owned());
    }
    backup_dir(to)?;
    let dir = to
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let file = tempfile::Builder::new()
        .prefix(".speedyspec-backup-")
        .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))
        .tempfile_in(dir)
        .map_err(|e| format!("cannot write in {}: {e}", dir.display()))?;
    let mut out = std::io::BufWriter::new(file);
    let fetched = remote.pod.get(&there, &mut out).map_err(|e| e.0);
    let _ = remote.pod.remove(&there);
    fetched?;
    let file = out
        .into_inner()
        .map_err(|e| format!("could not write {}: {}", to.display(), e.error()))?;
    file.persist(to)
        .map_err(|e| format!("could not write {}: {}", to.display(), e.error))?;
    Ok(())
}

/// Whether anything a restore replaces is there (the tables under the prefix, or the install's files), and the
/// site's own URL: asked in the runner pod.
fn probe_in_pod(
    remote: &Remote<'_>,
    spec: &BackupSpec,
    written: &DatabaseConnection,
) -> Result<(bool, Option<String>), String> {
    let request = json!({
        "root": remote.workload.root,
        "spec": spec_to_json(spec),
        "connection": remote.connection_request(written)?,
    });
    let reply = pod_request(remote, &request, &["self", "restore-probe"])?;
    if reply["ok"] != Json::Bool(true) {
        return Err(reply["error"]
            .as_str()
            .unwrap_or("the runner pod could not look at the site")
            .to_owned());
    }
    Ok((
        reply["hasContent"].as_bool().unwrap_or(true),
        reply["siteUrl"].as_str().map(str::to_owned),
    ))
}

/// A restore of `archive` (on this machine) into a Kubernetes install, done in its runner pod.
fn restore_in_pod(
    remote: &Remote<'_>,
    archive: &Path,
    spec: &BackupSpec,
    plan: &Move,
    written: &DatabaseConnection,
    maintenance: Option<&speedyspec_driver::MaintenanceFile>,
    warnings: &mut Vec<String>,
) -> Result<(), speedyspec_backup::flow::RestoreError> {
    let plain = |message: String| speedyspec_backup::flow::RestoreError {
        message,
        database_changed: false,
    };
    let there = archive_in_pod(remote);
    let mut file = std::fs::File::open(archive)
        .map_err(|e| plain(format!("cannot read {}: {e}", archive.display())))?;
    remote
        .pod
        .put(&there, &mut file, 0o600)
        .map_err(|e| plain(e.0))?;
    let request = json!({
        "root": remote.workload.root,
        "spec": spec_to_json(spec),
        "move": move_to_json(plan),
        "connection": remote.connection_request(written).map_err(plain)?,
        "archive": there,
        "maintenance": maintenance.map(|file| json!({"path": file.path, "contents": file.contents})),
    });
    let reply = pod_request(remote, &request, &["self", "restore-local"]);
    let _ = remote.pod.remove(&there);
    let reply = reply.map_err(plain)?;
    warnings.extend(strings(&reply["warnings"]));
    if reply["ok"] == Json::Bool(true) {
        return Ok(());
    }
    Err(speedyspec_backup::flow::RestoreError {
        message: reply["error"]
            .as_str()
            .unwrap_or("the restore failed")
            .to_owned(),
        database_changed: reply["databaseChanged"].as_bool().unwrap_or(true),
    })
}

/// Reads a runner pod's request file, deleting it at once (it holds the database password).
fn read_request(path: &Path) -> Result<(Json, LiveDatabase), String> {
    let text = std::fs::read_to_string(path);
    let _ = std::fs::remove_file(path);
    let request: Json =
        serde_json::from_str(&text.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let settings = database_connection_from_json(&request["connection"])?;
    let connection = Connection::open(&settings).map_err(|e| e.to_string())?;
    Ok((
        request,
        LiveDatabase {
            connection,
            prefix: settings.table_prefix.clone(),
        },
    ))
}

/// Prints a runner pod's reply, as one line of JSON.
fn reply(json: Json) -> i32 {
    let _ = writeln!(std::io::stdout().lock(), "{json}");
    0
}

/// `self backup-local`: in a runner pod, `flow::backup` of the install there.
pub fn pod_backup(path: &Path) -> i32 {
    let made = read_request(path).and_then(|(request, mut db)| {
        let spec = spec_from_json(&request["spec"])?;
        let manifest = Manifest::from_json(&request["manifest"])?;
        let root = PathBuf::from(request["root"].as_str().unwrap_or("/"));
        let to = PathBuf::from(
            request["to"]
                .as_str()
                .ok_or("the backup request has no to")?,
        );
        backup(&root, &spec, &mut db, manifest, &to)
    });
    match made {
        Ok(warnings) => reply(json!({"ok": true, "warnings": warnings})),
        Err(error) => reply(json!({"ok": false, "error": error})),
    }
}

/// `self restore-probe`: in a runner pod, whether the install there has content a restore would replace, and its URL.
pub fn pod_restore_probe(path: &Path) -> i32 {
    let found = read_request(path).and_then(|(request, mut db)| {
        let spec = spec_from_json(&request["spec"])?;
        let root = PathBuf::from(request["root"].as_str().unwrap_or("/"));
        let site_url = match &spec.site_url {
            Some(at) => db.site_url(at).ok().flatten(),
            None => None,
        };
        let has_content = db.has_prefixed_tables()? || has_files(&root, &spec);
        Ok(json!({"ok": true, "hasContent": has_content, "siteUrl": site_url}))
    });
    reply(found.unwrap_or_else(|error| json!({"ok": false, "error": error})))
}

/// `self restore-local`: in a runner pod, `flow::restore` of an archive into the install there, the site in
/// maintenance while it runs.
pub fn pod_restore(path: &Path) -> i32 {
    let (request, mut db) = match read_request(path) {
        Ok(read) => read,
        Err(error) => return reply(json!({"ok": false, "error": error, "databaseChanged": false})),
    };
    let parsed = spec_from_json(&request["spec"])
        .and_then(|spec| Ok((spec, move_from_json(&request["move"])?)));
    let (spec, plan) = match parsed {
        Ok(parsed) => parsed,
        Err(error) => return reply(json!({"ok": false, "error": error, "databaseChanged": false})),
    };
    let root = PathBuf::from(request["root"].as_str().unwrap_or("/"));
    let archive = PathBuf::from(request["archive"].as_str().unwrap_or_default());
    let maintenance = request["maintenance"]["path"]
        .as_str()
        .map(|file| root.join(file));
    if let Some(file) = &maintenance {
        let _ = std::fs::write(
            file,
            request["maintenance"]["contents"]
                .as_str()
                .unwrap_or_default(),
        );
    }
    let mut warnings = Vec::new();
    let result = restore(&archive, &root, &spec, &mut db, &plan, &mut warnings);
    if let Some(file) = &maintenance {
        let _ = std::fs::remove_file(file);
    }
    match result {
        Ok(()) => reply(json!({"ok": true, "warnings": warnings})),
        Err(error) => reply(json!({
            "ok": false, "error": error.message, "databaseChanged": error.database_changed, "warnings": warnings,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use speedyspec_driver::PrefixedColumn;

    #[test]
    fn a_spec_and_a_move_go_to_the_runner_pod_unchanged() {
        let spec = BackupSpec {
            include: vec![".".into()],
            exclude: vec!["cache".into()],
            preserve: vec!["wp-config.php".into()],
            site_url: Some(SiteUrl {
                table: "options".into(),
                key_column: "option_name".into(),
                key: "siteurl".into(),
                value_column: "option_value".into(),
            }),
            prefixed_columns: vec![
                PrefixedColumn {
                    table: "options".into(),
                    column: "option_name".into(),
                    keys: Some(vec!["user_roles".into()]),
                },
                PrefixedColumn {
                    table: "usermeta".into(),
                    column: "meta_key".into(),
                    keys: None,
                },
            ],
            app_version: Some("7.1".into()),
            keep_urls: vec![("posts".into(), "guid".into())],
        };
        assert_eq!(spec_from_json(&spec_to_json(&spec)).unwrap(), spec);
        let none = BackupSpec {
            site_url: None,
            app_version: None,
            ..spec.clone()
        };
        assert_eq!(spec_from_json(&spec_to_json(&none)).unwrap(), none);
        let plan = Move {
            from_prefix: "wp_".into(),
            to_prefix: "site_".into(),
            urls: Some(("https://old.example".into(), "https://new.example".into())),
            prefixed_columns: vec![(
                "options".into(),
                "option_name".into(),
                Some(vec!["user_roles".into()]),
            )],
            keep_urls: vec![("posts".into(), "guid".into())],
        };
        let back = move_from_json(&move_to_json(&plan)).unwrap();
        assert_eq!(
            (back.from_prefix, back.to_prefix, back.urls),
            (
                plan.from_prefix.clone(),
                plan.to_prefix.clone(),
                plan.urls.clone()
            )
        );
        assert_eq!(
            (back.prefixed_columns, back.keep_urls),
            (plan.prefixed_columns.clone(), plan.keep_urls.clone())
        );
    }
}
