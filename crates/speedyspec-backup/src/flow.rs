//! Backing up an install and restoring it, over a [`SiteDatabase`] so the flows are the same for a real MySQL and
//! for the tests.
//!
//! A backup is one gzipped tar file, created readable only by its owner: `manifest.json` first, then the install's
//! files under `files/` (as the app's `BackupSpec` includes and excludes them; symbolic links are kept as links),
//! then `database.sql`, a dump in `speedyspec_db`'s own format. Nothing in it depends on the driver that made it.

use std::fs::{self, File};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;
use speedyspec_driver::{BackupSpec, SiteUrl};

use crate::manifest::Manifest;
use crate::rewrite::Move;

/// The database of an install, as backups and restores need it.
pub trait SiteDatabase {
    /// Writes a dump of the install's tables.
    fn export(&mut self, out: &mut dyn Write) -> Result<(), String>;
    /// Runs a dump, each statement rewritten by `plan`; the rewrite's warnings are appended to `warnings`.
    fn import(
        &mut self,
        input: &mut dyn BufRead,
        plan: &Move,
        warnings: &mut Vec<String>,
    ) -> Result<u64, String>;
    /// Whether the install's database already has tables with its prefix.
    fn has_prefixed_tables(&mut self) -> Result<bool, String>;
    /// The value at `at`, in the install's prefixed table.
    fn site_url(&mut self, at: &SiteUrl) -> Result<Option<String>, String>;
    /// The install's table prefix.
    fn prefix(&self) -> String;
}

/// Under the install root: entries speedyspec makes while it works, never part of a backup.
pub const WORK_PREFIX: &str = ".speedyspec-";

fn io_error(what: &str, path: &Path, e: io::Error) -> String {
    format!("cannot {what} {}: {e}", path.display())
}

fn excluded(relative: &Path, spec: &BackupSpec) -> bool {
    spec.exclude
        .iter()
        .any(|ex| relative == Path::new(ex) || relative.starts_with(ex))
}

/// Every entry under `path` (itself included), depth first, sorted; symbolic links are not followed.
fn walk(
    root: &Path,
    relative: &Path,
    spec: &BackupSpec,
    skip: &[PathBuf],
    out: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let full = if relative == Path::new(".") {
        root.to_path_buf()
    } else {
        root.join(relative)
    };
    if skip.iter().any(|s| s == &full) || excluded(relative, spec) {
        return Ok(());
    }
    let meta = match fs::symlink_metadata(&full) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_error("read", &full, e)),
    };
    if relative != Path::new(".") {
        out.push(relative.to_path_buf());
    }
    if meta.is_dir() {
        let mut names: Vec<_> = fs::read_dir(&full)
            .map_err(|e| io_error("read", &full, e))?
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect();
        names.sort();
        for name in names {
            if name.to_string_lossy().starts_with(WORK_PREFIX) {
                continue;
            }
            let child = if relative == Path::new(".") {
                PathBuf::from(&name)
            } else {
                relative.join(&name)
            };
            walk(root, &child, spec, skip, out)?;
        }
    }
    Ok(())
}

/// Writes a backup of the install at `root` to `to`. The manifest's `table_prefix` and `site_url` are filled in from
/// the database. `to` is created readable only by its owner, through a temporary file beside it.
pub fn backup(
    root: &Path,
    spec: &BackupSpec,
    db: &mut dyn SiteDatabase,
    mut manifest: Manifest,
    to: &Path,
) -> Result<(), String> {
    let dir = to
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    manifest.table_prefix = db.prefix();
    manifest.site_url = match &spec.site_url {
        Some(at) => db.site_url(at)?,
        None => None,
    };

    // The dump first: a tar entry needs its size before its contents.
    let mut dump = tempfile::Builder::new()
        .prefix(".speedyspec-dump-")
        .tempfile_in(dir)
        .map_err(|e| io_error("write in", dir, e))?;
    {
        let dump_path = dump.path().to_path_buf();
        let mut writer = io::BufWriter::new(dump.as_file_mut());
        db.export(&mut writer)?;
        writer
            .flush()
            .map_err(|e| io_error("write", &dump_path, e))?;
    }

    let archive = tempfile::Builder::new()
        .prefix(".speedyspec-backup-")
        .tempfile_in(dir)
        .map_err(|e| io_error("write in", dir, e))?;
    let skip = [
        archive.path().to_path_buf(),
        dump.path().to_path_buf(),
        to.to_path_buf(),
    ];
    let mut entries = Vec::new();
    for include in &spec.include {
        walk(root, Path::new(include), spec, &skip, &mut entries)?;
    }
    entries.dedup();

    let gz = GzEncoder::new(archive.as_file(), Compression::default());
    let mut tar = tar::Builder::new(gz);
    tar.follow_symlinks(false);
    let manifest_bytes =
        serde_json::to_vec_pretty(&manifest.to_json()).expect("a manifest is JSON");
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest_bytes.len() as u64);
    header.set_mode(0o600);
    header.set_cksum();
    tar.append_data(&mut header, "manifest.json", &manifest_bytes[..])
        .map_err(|e| io_error("write", archive.path(), e))?;
    for relative in &entries {
        let name = Path::new("files").join(relative);
        tar.append_path_with_name(root.join(relative), &name)
            .map_err(|e| io_error("archive", &root.join(relative), e))?;
    }
    let mut dump_file = File::open(dump.path()).map_err(|e| io_error("read", dump.path(), e))?;
    tar.append_file("database.sql", &mut dump_file)
        .map_err(|e| io_error("write", archive.path(), e))?;
    let gz = tar
        .into_inner()
        .map_err(|e| io_error("write", archive.path(), e))?;
    gz.finish()
        .map_err(|e| io_error("write", archive.path(), e))?;
    archive
        .persist(to)
        .map_err(|e| io_error("create", to, e.error))?;
    Ok(())
}

/// Reads a backup's manifest, which must be its first entry. Refuses what is not a backup speedyspec can read.
pub fn read_manifest(archive: &Path) -> Result<Manifest, String> {
    let file = File::open(archive).map_err(|e| io_error("open", archive, e))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let not_backup = || format!("{} is not a speedyspec backup", archive.display());
    let mut entries = tar.entries().map_err(|_| not_backup())?;
    let mut first = entries
        .next()
        .ok_or_else(not_backup)?
        .map_err(|_| not_backup())?;
    if first.path().map_err(|_| not_backup())?.as_ref() != Path::new("manifest.json") {
        return Err(format!(
            "{}: the first entry is not manifest.json",
            not_backup()
        ));
    }
    let mut text = String::new();
    io::Read::read_to_string(&mut first, &mut text).map_err(|_| not_backup())?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|_| not_backup())?;
    Manifest::from_json(&json).map_err(|e| format!("{}: {e}", archive.display()))
}

/// The move a restore makes: from the backup's prefix to `to_prefix`, and from its site URL to `url` when given.
pub fn plan_move(
    manifest: &Manifest,
    to_prefix: &str,
    url: Option<&str>,
    spec: &BackupSpec,
) -> Result<Move, String> {
    let urls = match url {
        None => None,
        Some(new) => {
            let new = new.trim_end_matches('/');
            let host = new
                .strip_prefix("https://")
                .or_else(|| new.strip_prefix("http://"))
                .unwrap_or("");
            if host.is_empty() || host.starts_with('/') {
                return Err(format!(
                    "--url must be the site's whole URL, such as https://example.com; got {new:?}"
                ));
            }
            let old = manifest.site_url.clone().ok_or(
                "the backup does not say what its site URL was, so --url cannot be used; restore without it",
            )?;
            // Both without a trailing slash, so `https://old/about` becomes `https://new/about`.
            Some((old.trim_end_matches('/').to_owned(), new.to_owned()))
        }
    };
    Ok(Move {
        from_prefix: manifest.table_prefix.clone(),
        to_prefix: to_prefix.to_owned(),
        urls,
        prefixed_columns: spec
            .prefixed_columns
            .iter()
            .map(|c| (c.table.clone(), c.column.clone()))
            .collect(),
    })
}

/// Where a restore stopped, and what it had changed by then.
#[derive(Debug)]
pub struct RestoreError {
    pub message: String,
    /// Whether the database had been changed (some statements ran).
    pub database_changed: bool,
}

/// A path under `files/`, checked: relative, only plain components.
fn entry_path(path: &Path) -> Result<PathBuf, String> {
    let rest = path
        .strip_prefix("files")
        .map_err(|_| format!("archive entry {} is not under files/", path.display()))?;
    let mut clean = PathBuf::new();
    for component in rest.components() {
        match component {
            std::path::Component::Normal(part) => clean.push(part),
            std::path::Component::CurDir => {}
            _ => {
                return Err(format!(
                    "archive entry {} was refused: it leaves the install",
                    path.display()
                ));
            }
        }
    }
    Ok(clean)
}

/// Whether writing `relative` under `staged` would go through, or replace, a symbolic link an earlier entry made. Each
/// link is checked on its own when it is made, so a chain of links (`a -> .`, `b -> a/..`) could otherwise reach
/// outside the install.
fn through_link(staged: &Path, relative: &Path) -> bool {
    let mut at = staged.to_path_buf();
    for component in relative.components() {
        at.push(component);
        if fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink()) {
            return true;
        }
    }
    false
}

/// Whether a link at `relative` pointing to `target` passes through a link an earlier entry made (`b -> a/..` with
/// `a -> .`), which `link_stays_inside` cannot see.
fn target_through_link(staged: &Path, relative: &Path, target: &Path) -> bool {
    let mut at: Vec<std::ffi::OsString> = relative
        .parent()
        .map(|p| p.components().map(|c| c.as_os_str().to_owned()).collect())
        .unwrap_or_default();
    for component in target.components() {
        match component {
            std::path::Component::ParentDir => {
                at.pop();
            }
            std::path::Component::Normal(part) => {
                at.push(part.to_owned());
                let path: PathBuf = at.iter().collect();
                if fs::symlink_metadata(staged.join(&path))
                    .is_ok_and(|m| m.file_type().is_symlink())
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Whether a link at `relative` pointing to `target` stays inside the install.
fn link_stays_inside(relative: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return false;
    }
    let mut depth: i64 = relative.components().count() as i64 - 1;
    for component in target.components() {
        match component {
            std::path::Component::ParentDir => depth -= 1,
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::CurDir => {}
            _ => return false,
        }
        if depth < 0 {
            return false;
        }
    }
    true
}

fn random_name(prefix: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{WORK_PREFIX}{prefix}-{}-{nanos}", std::process::id())
}

/// The top-level entries a restore replaces: everything under the included paths (for `.`, every entry of the root
/// and of the unpacked backup, except speedyspec's own work entries).
fn scope(root: &Path, staged: &Path, spec: &BackupSpec) -> Result<Vec<PathBuf>, String> {
    let mut found = Vec::new();
    for include in &spec.include {
        if include == "." {
            for dir in [root, staged] {
                for entry in fs::read_dir(dir)
                    .map_err(|e| io_error("read", dir, e))?
                    .filter_map(Result::ok)
                {
                    let name = entry.file_name();
                    if !name.to_string_lossy().starts_with(WORK_PREFIX) {
                        found.push(PathBuf::from(name));
                    }
                }
            }
        } else {
            found.push(PathBuf::from(include));
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}

/// Moves `from` to `to`, making `to`'s parents.
fn move_to(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(from, to)
}

/// Puts the unpacked files in place of the install's: each entry in scope is moved aside and its new copy moved in,
/// then the target's own preserved and excluded paths are moved back into the new tree. On failure, what was moved
/// is put back.
fn swap(root: &Path, staged: &Path, spec: &BackupSpec) -> Result<(), String> {
    let aside = root.join(random_name("restore-old"));
    fs::create_dir(&aside).map_err(|e| io_error("create", &aside, e))?;
    let mut moved_aside: Vec<PathBuf> = Vec::new();
    let mut moved_in: Vec<PathBuf> = Vec::new();
    let result = (|| -> Result<(), String> {
        let keep: Vec<&String> = spec.preserve.iter().chain(spec.exclude.iter()).collect();
        for entry in scope(root, staged, spec)? {
            // A kept path at the top level stays where it is.
            if keep.iter().any(|k| Path::new(k.as_str()) == entry) {
                continue;
            }
            if fs::symlink_metadata(root.join(&entry)).is_ok() {
                move_to(&root.join(&entry), &aside.join(&entry))
                    .map_err(|e| io_error("move aside", &root.join(&entry), e))?;
                moved_aside.push(entry.clone());
            }
            if fs::symlink_metadata(staged.join(&entry)).is_ok() {
                move_to(&staged.join(&entry), &root.join(&entry))
                    .map_err(|e| io_error("restore", &root.join(&entry), e))?;
                moved_in.push(entry);
            }
        }
        for kept in keep {
            let old = aside.join(kept);
            if fs::symlink_metadata(&old).is_err() {
                continue;
            }
            let live = root.join(kept);
            if let Ok(meta) = fs::symlink_metadata(&live) {
                let removed = if meta.is_dir() {
                    fs::remove_dir_all(&live)
                } else {
                    fs::remove_file(&live)
                };
                removed.map_err(|e| io_error("replace", &live, e))?;
            }
            move_to(&old, &live).map_err(|e| io_error("keep", &live, e))?;
        }
        Ok(())
    })();
    if result.is_err() {
        for entry in &moved_in {
            let _ = move_to(&root.join(entry), &staged.join(entry));
        }
        for entry in &moved_aside {
            let _ = move_to(&aside.join(entry), &root.join(entry));
        }
    }
    let _ = fs::remove_dir_all(&aside);
    result
}

/// Restores `archive` into the install at `root` and its database: the files are unpacked beside the install, the
/// dump is imported straight from the archive with `plan`, and only then are the files swapped in. The import's
/// warnings are appended to `warnings`.
pub fn restore(
    archive: &Path,
    root: &Path,
    spec: &BackupSpec,
    db: &mut dyn SiteDatabase,
    plan: &Move,
    warnings: &mut Vec<String>,
) -> Result<(), RestoreError> {
    let plain = |message: String| RestoreError {
        message,
        database_changed: false,
    };
    let staged = tempfile::Builder::new()
        .prefix(&format!("{WORK_PREFIX}restore-"))
        .tempdir_in(root)
        .map_err(|e| plain(io_error("write in", root, e)))?;
    let file = File::open(archive).map_err(|e| plain(io_error("open", archive, e)))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let not_backup = || plain(format!("{} is not a speedyspec backup", archive.display()));
    let mut imported = false;
    for entry in tar.entries().map_err(|_| not_backup())? {
        let mut entry = entry.map_err(|_| not_backup())?;
        let path = entry.path().map_err(|_| not_backup())?.into_owned();
        if path == Path::new("manifest.json") {
            continue;
        }
        if path == Path::new("database.sql") {
            let mut reader = io::BufReader::new(&mut entry);
            db.import(&mut reader, plan, warnings)
                .map_err(|message| RestoreError {
                    message: format!(
                        "the database import stopped: {message}; the files were not replaced"
                    ),
                    database_changed: true,
                })?;
            imported = true;
            continue;
        }
        if imported {
            return Err(RestoreError {
                message: format!("{}: files after database.sql", archive.display()),
                database_changed: true,
            });
        }
        let relative = entry_path(&path).map_err(plain)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let dest = staged.path().join(&relative);
        if through_link(staged.path(), &relative) {
            return Err(plain(format!(
                "archive entry {} was refused: it goes through a link an earlier entry made",
                path.display()
            )));
        }
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                fs::create_dir_all(&dest).map_err(|e| plain(io_error("create", &dest, e)))?;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).map_err(|e| plain(io_error("create", parent, e)))?;
                }
                entry
                    .unpack(&dest)
                    .map_err(|e| plain(io_error("unpack", &dest, e)))?;
            }
            tar::EntryType::Symlink => {
                let target = entry
                    .link_name()
                    .map_err(|_| not_backup())?
                    .ok_or_else(not_backup)?
                    .into_owned();
                if !link_stays_inside(&relative, &target)
                    || target_through_link(staged.path(), &relative, &target)
                {
                    return Err(plain(format!(
                        "archive entry {} was refused: its link points outside the install",
                        path.display()
                    )));
                }
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).map_err(|e| plain(io_error("create", parent, e)))?;
                }
                std::os::unix::fs::symlink(&target, &dest)
                    .map_err(|e| plain(io_error("create", &dest, e)))?;
            }
            other => {
                return Err(plain(format!(
                    "archive entry {} was refused: entries of type {other:?} are not restored",
                    path.display()
                )));
            }
        }
    }
    if !imported {
        return Err(plain(format!("{} has no database.sql", archive.display())));
    }
    swap(root, staged.path(), spec).map_err(|message| RestoreError {
        message: format!(
            "the database was restored, but the files could not be swapped in: {message}"
        ),
        database_changed: true,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;

    use flate2::read::GzDecoder;

    /// A database that keeps its "tables" as one dump.
    pub(crate) struct FakeDatabase {
        pub dump: Vec<u8>,
        pub prefix: String,
        pub site_url: Option<String>,
        pub tables: bool,
        pub imported: Vec<u8>,
        pub fail_import: bool,
    }

    impl FakeDatabase {
        pub fn new() -> FakeDatabase {
            FakeDatabase {
                dump: b"DROP TABLE IF EXISTS `wp_options`;\nCREATE TABLE `wp_options` (`a` int);\n"
                    .to_vec(),
                prefix: "wp_".into(),
                site_url: Some("https://old.example".into()),
                tables: true,
                imported: Vec::new(),
                fail_import: false,
            }
        }
    }

    impl SiteDatabase for FakeDatabase {
        fn export(&mut self, out: &mut dyn Write) -> Result<(), String> {
            out.write_all(&self.dump).map_err(|e| e.to_string())
        }

        fn import(
            &mut self,
            input: &mut dyn BufRead,
            plan: &Move,
            warnings: &mut Vec<String>,
        ) -> Result<u64, String> {
            let mut all = Vec::new();
            input.read_to_end(&mut all).map_err(|e| e.to_string())?;
            if self.fail_import {
                return Err("statement 2 (line 3) failed: Table exists".into());
            }
            let mut count = 0;
            for statement in all.split(|b| *b == b'\n').filter(|s| !s.is_empty()) {
                let s = statement.strip_suffix(b";").unwrap_or(statement);
                self.imported
                    .extend_from_slice(&crate::rewrite::rewrite(s, plan, warnings));
                self.imported.extend_from_slice(b";\n");
                count += 1;
            }
            Ok(count)
        }

        fn has_prefixed_tables(&mut self) -> Result<bool, String> {
            Ok(self.tables)
        }

        fn site_url(&mut self, _: &SiteUrl) -> Result<Option<String>, String> {
            Ok(self.site_url.clone())
        }

        fn prefix(&self) -> String {
            self.prefix.clone()
        }
    }

    pub(crate) fn spec() -> BackupSpec {
        BackupSpec {
            include: vec![".".into()],
            exclude: vec!["cache".into()],
            preserve: vec!["config.php".into()],
            site_url: Some(SiteUrl {
                table: "options".into(),
                key_column: "option_name".into(),
                key: "siteurl".into(),
                value_column: "option_value".into(),
            }),
            prefixed_columns: Vec::new(),
            app_version: Some("1.0".into()),
        }
    }

    pub(crate) fn manifest() -> Manifest {
        Manifest {
            app: "site".into(),
            parts: vec![],
            cli_version: "1.0.1".into(),
            app_version: Some("1.0".into()),
            extensions: vec![],
            engine: "mysql".into(),
            table_prefix: String::new(),
            site_url: None,
            source_install: "blog".into(),
            source_driver: "zip".into(),
            created_at: "2026-11-19T10:00:00Z".into(),
        }
    }

    pub(crate) fn site() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("cache")).unwrap();
        fs::write(root.path().join("cache/page.html"), "cached").unwrap();
        fs::write(root.path().join("index.php"), "<?php // site").unwrap();
        fs::write(root.path().join("config.php"), "<?php // source config").unwrap();
        fs::create_dir_all(root.path().join("content/uploads")).unwrap();
        fs::write(root.path().join("content/uploads/a.txt"), "upload").unwrap();
        std::os::unix::fs::symlink("index.php", root.path().join("link.php")).unwrap();
        root
    }

    fn entries(archive: &Path) -> Vec<(String, tar::EntryType)> {
        let mut tar = tar::Archive::new(GzDecoder::new(File::open(archive).unwrap()));
        tar.entries()
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (
                    e.path().unwrap().to_string_lossy().into_owned(),
                    e.header().entry_type(),
                )
            })
            .collect()
    }

    #[test]
    fn writes_the_documented_archive() {
        let root = site();
        let out = tempfile::tempdir().unwrap();
        let to = out.path().join("b.tar.gz");
        backup(
            root.path(),
            &spec(),
            &mut FakeDatabase::new(),
            manifest(),
            &to,
        )
        .unwrap();
        let names: Vec<String> = entries(&to).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names.first().map(String::as_str), Some("manifest.json"));
        assert_eq!(names.last().map(String::as_str), Some("database.sql"));
        assert!(names.contains(&"files/index.php".to_owned()), "{names:?}");
        assert!(
            names.contains(&"files/content/uploads/a.txt".to_owned()),
            "{names:?}"
        );
        assert!(
            !names.iter().any(|n| n.starts_with("files/cache")),
            "excluded: {names:?}"
        );
        let link = entries(&to)
            .into_iter()
            .find(|(n, _)| n == "files/link.php")
            .unwrap();
        assert_eq!(link.1, tar::EntryType::Symlink);
        assert_eq!(
            fs::metadata(&to).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Only the archive is left beside it.
        assert_eq!(fs::read_dir(out.path()).unwrap().count(), 1);
    }

    #[test]
    fn the_manifest_holds_the_prefix_and_site_url() {
        let root = site();
        let out = tempfile::tempdir().unwrap();
        let to = out.path().join("b.tar.gz");
        backup(
            root.path(),
            &spec(),
            &mut FakeDatabase::new(),
            manifest(),
            &to,
        )
        .unwrap();
        let mut tar = tar::Archive::new(GzDecoder::new(File::open(&to).unwrap()));
        let mut first = tar.entries().unwrap().next().unwrap().unwrap();
        let mut text = String::new();
        first.read_to_string(&mut text).unwrap();
        let m = Manifest::from_json(&serde_json::from_str(&text).unwrap()).unwrap();
        assert_eq!(m.table_prefix, "wp_");
        assert_eq!(m.site_url.as_deref(), Some("https://old.example"));
    }

    #[test]
    fn a_failing_export_leaves_nothing() {
        struct Broken;
        impl SiteDatabase for Broken {
            fn export(&mut self, _: &mut dyn Write) -> Result<(), String> {
                Err("could not connect".into())
            }
            fn import(
                &mut self,
                _: &mut dyn BufRead,
                _: &Move,
                _: &mut Vec<String>,
            ) -> Result<u64, String> {
                unreachable!()
            }
            fn has_prefixed_tables(&mut self) -> Result<bool, String> {
                Ok(false)
            }
            fn site_url(&mut self, _: &SiteUrl) -> Result<Option<String>, String> {
                Ok(None)
            }
            fn prefix(&self) -> String {
                "wp_".into()
            }
        }
        let root = site();
        let out = tempfile::tempdir().unwrap();
        let error = backup(
            root.path(),
            &spec(),
            &mut Broken,
            manifest(),
            &out.path().join("b.tar.gz"),
        )
        .unwrap_err();
        assert!(error.contains("could not connect"), "{error}");
        assert_eq!(fs::read_dir(out.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_backup_inside_the_root_does_not_archive_itself() {
        let root = site();
        let to = root.path().join("b.tar.gz");
        backup(
            root.path(),
            &spec(),
            &mut FakeDatabase::new(),
            manifest(),
            &to,
        )
        .unwrap();
        let names: Vec<String> = entries(&to).into_iter().map(|(n, _)| n).collect();
        assert!(
            !names
                .iter()
                .any(|n| n.contains("b.tar.gz") || n.contains(".speedyspec-")),
            "{names:?}"
        );
    }

    // ----- restore

    fn backed_up(root: &Path) -> (tempfile::TempDir, PathBuf) {
        let out = tempfile::tempdir().unwrap();
        let to = out.path().join("b.tar.gz");
        backup(root, &spec(), &mut FakeDatabase::new(), manifest(), &to).unwrap();
        (out, to)
    }

    fn same_site(m: &Manifest) -> Move {
        plan_move(m, "wp_", None, &spec()).unwrap()
    }

    #[test]
    fn restores_files_and_database_and_keeps_the_targets_own() {
        let root = site();
        let (_out, archive) = backed_up(root.path());
        // Changes after the backup: a file deleted, one added, the config and the cache rewritten.
        fs::remove_file(root.path().join("content/uploads/a.txt")).unwrap();
        fs::write(root.path().join("new.php"), "added later").unwrap();
        fs::write(
            root.path().join("config.php"),
            "<?php // the target's own config",
        )
        .unwrap();
        fs::write(root.path().join("cache/page.html"), "fresh cache").unwrap();
        let m = read_manifest(&archive).unwrap();
        let mut db = FakeDatabase::new();
        let mut warnings = Vec::new();
        restore(
            &archive,
            root.path(),
            &spec(),
            &mut db,
            &same_site(&m),
            &mut warnings,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("content/uploads/a.txt")).unwrap(),
            "upload"
        );
        assert!(
            !root.path().join("new.php").exists(),
            "a file the backup did not have is gone"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("config.php")).unwrap(),
            "<?php // the target's own config"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("cache/page.html")).unwrap(),
            "fresh cache"
        );
        assert!(
            fs::symlink_metadata(root.path().join("link.php"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(String::from_utf8_lossy(&db.imported).contains("CREATE TABLE `wp_options`"));
        let leftovers: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(WORK_PREFIX))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn a_move_renames_the_prefix_in_the_import() {
        let root = site();
        let (_out, archive) = backed_up(root.path());
        let m = read_manifest(&archive).unwrap();
        let plan = plan_move(&m, "site_", Some("https://new.example.org/"), &spec()).unwrap();
        assert_eq!(
            plan.urls,
            Some((
                "https://old.example".into(),
                "https://new.example.org".into()
            ))
        );
        let mut db = FakeDatabase::new();
        restore(
            &archive,
            root.path(),
            &spec(),
            &mut db,
            &plan,
            &mut Vec::new(),
        )
        .unwrap();
        assert!(String::from_utf8_lossy(&db.imported).contains("CREATE TABLE `site_options`"));
    }

    #[test]
    fn urls_are_checked_and_trimmed_alike() {
        let mut m = manifest();
        m.site_url = Some("https://old.example/".into());
        let plan = plan_move(&m, "wp_", Some("https://new.example.org/"), &spec()).unwrap();
        assert_eq!(
            plan.urls,
            Some((
                "https://old.example".into(),
                "https://new.example.org".into()
            ))
        );
        for bad in [
            "",
            "/",
            "new.example.org",
            "ftp://new.example.org",
            "https://",
        ] {
            let error = plan_move(&m, "wp_", Some(bad), &spec()).unwrap_err();
            assert!(error.contains("--url"), "{bad:?}: {error}");
        }
    }

    #[test]
    fn url_without_a_site_url_in_the_backup_is_refused() {
        let mut m = manifest();
        m.site_url = None;
        let error = plan_move(&m, "wp_", Some("https://new.example.org"), &spec()).unwrap_err();
        assert!(error.contains("--url cannot be used"), "{error}");
    }

    #[test]
    fn a_failed_import_leaves_the_files_alone() {
        let root = site();
        let (_out, archive) = backed_up(root.path());
        fs::write(root.path().join("index.php"), "<?php // changed").unwrap();
        let m = read_manifest(&archive).unwrap();
        let mut db = FakeDatabase::new();
        db.fail_import = true;
        let error = restore(
            &archive,
            root.path(),
            &spec(),
            &mut db,
            &same_site(&m),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(error.database_changed);
        assert!(
            error.message.contains("statement 2")
                && error.message.contains("files were not replaced"),
            "{}",
            error.message
        );
        assert_eq!(
            fs::read_to_string(root.path().join("index.php")).unwrap(),
            "<?php // changed"
        );
    }

    /// A gzipped tar with a manifest and one raw entry whose name and type are written by hand (the tar crate refuses
    /// to build unsafe ones).
    fn crafted(
        name: &str,
        kind: tar::EntryType,
        link: Option<&str>,
    ) -> (tempfile::TempDir, PathBuf) {
        let out = tempfile::tempdir().unwrap();
        let path = out.path().join("bad.tar.gz");
        let gz = GzEncoder::new(File::create(&path).unwrap(), Compression::default());
        let mut tar = tar::Builder::new(gz);
        let manifest_bytes = serde_json::to_vec(&manifest().to_json()).unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_size(manifest_bytes.len() as u64);
        header.set_cksum();
        tar.append_data(&mut header, "manifest.json", &manifest_bytes[..])
            .unwrap();
        let mut header = tar::Header::new_old();
        header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        header.set_entry_type(kind);
        header.set_size(0);
        header.set_mode(0o644);
        if let Some(link) = link {
            header.as_old_mut().linkname[..link.len()].copy_from_slice(link.as_bytes());
        }
        header.set_cksum();
        tar.append(&header, &[][..]).unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        (out, path)
    }

    #[test]
    fn bad_archives_are_refused_before_anything_changes() {
        let root = site();
        let before = fs::read_to_string(root.path().join("index.php")).unwrap();
        let junk = tempfile::NamedTempFile::new().unwrap();
        fs::write(junk.path(), "not a gzip file").unwrap();
        assert!(
            read_manifest(junk.path())
                .unwrap_err()
                .contains("is not a speedyspec backup")
        );
        let (_d, escape) = crafted("files/../../evil.php", tar::EntryType::Regular, None);
        let (_e, link) = crafted(
            "files/link",
            tar::EntryType::Symlink,
            Some("../../etc/passwd"),
        );
        let (_f, device) = crafted("files/dev", tar::EntryType::Char, None);
        for (archive, expected) in [
            (&escape, "leaves the install"),
            (&link, "points outside"),
            (&device, "are not restored"),
        ] {
            let m = read_manifest(archive).unwrap();
            let error = restore(
                archive,
                root.path(),
                &spec(),
                &mut FakeDatabase::new(),
                &same_site(&m),
                &mut Vec::new(),
            )
            .unwrap_err();
            assert!(error.message.contains(expected), "{}", error.message);
            assert!(!error.database_changed);
        }
        assert_eq!(
            fs::read_to_string(root.path().join("index.php")).unwrap(),
            before
        );
        assert!(!root.path().parent().unwrap().join("evil.php").exists());
        // Only a manifest: no database.
        let (_g, empty) = crafted("files/ok.txt", tar::EntryType::Regular, None);
        let m = read_manifest(&empty).unwrap();
        let error = restore(
            &empty,
            root.path(),
            &spec(),
            &mut FakeDatabase::new(),
            &same_site(&m),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(
            error.message.contains("has no database.sql"),
            "{}",
            error.message
        );
        assert_eq!(
            fs::read_to_string(root.path().join("index.php")).unwrap(),
            before
        );
    }

    /// A gzipped tar with a manifest and raw entries `(name, type, link)`, written by hand.
    fn crafted_many(
        items: &[(&str, tar::EntryType, Option<&str>)],
    ) -> (tempfile::TempDir, PathBuf) {
        let out = tempfile::tempdir().unwrap();
        let path = out.path().join("bad.tar.gz");
        let gz = GzEncoder::new(File::create(&path).unwrap(), Compression::default());
        let mut tar = tar::Builder::new(gz);
        let manifest_bytes = serde_json::to_vec(&manifest().to_json()).unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_size(manifest_bytes.len() as u64);
        header.set_cksum();
        tar.append_data(&mut header, "manifest.json", &manifest_bytes[..])
            .unwrap();
        for (name, kind, link) in items {
            let mut header = tar::Header::new_old();
            header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
            header.set_entry_type(*kind);
            header.set_size(0);
            header.set_mode(0o644);
            if let Some(link) = link {
                header.as_old_mut().linkname[..link.len()].copy_from_slice(link.as_bytes());
            }
            header.set_cksum();
            tar.append(&header, &[][..]).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
        (out, path)
    }

    #[test]
    fn chained_links_cannot_reach_outside() {
        let root = site();
        let outside = root.path().parent().unwrap().join("evil.txt");
        let _ = fs::remove_file(&outside);
        let (_d, archive) = crafted_many(&[
            ("files/a", tar::EntryType::Symlink, Some(".")),
            ("files/b", tar::EntryType::Symlink, Some("a/..")),
            ("files/c", tar::EntryType::Symlink, Some("b/..")),
            ("files/c/evil.txt", tar::EntryType::Regular, None),
        ]);
        let m = read_manifest(&archive).unwrap();
        let result = restore(
            &archive,
            root.path(),
            &spec(),
            &mut FakeDatabase::new(),
            &same_site(&m),
            &mut Vec::new(),
        );
        assert!(!outside.exists(), "a file was written outside the install");
        let error = result.unwrap_err();
        assert!(!error.database_changed, "{}", error.message);
        assert!(error.message.contains("refused"), "{}", error.message);
    }

    #[test]
    fn a_link_through_an_earlier_link_is_refused() {
        let root = site();
        let (_d, archive) = crafted_many(&[
            ("files/a", tar::EntryType::Symlink, Some(".")),
            ("files/b", tar::EntryType::Symlink, Some("a/..")),
        ]);
        let m = read_manifest(&archive).unwrap();
        let error = restore(
            &archive,
            root.path(),
            &spec(),
            &mut FakeDatabase::new(),
            &same_site(&m),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(
            error.message.contains("points outside"),
            "{}",
            error.message
        );
        assert!(!root.path().join("b").exists());
    }

    #[test]
    fn links_inside_the_install_are_fine() {
        assert!(link_stays_inside(Path::new("a/b/link"), Path::new("../c")));
        assert!(!link_stays_inside(Path::new("link"), Path::new("../x")));
        assert!(!link_stays_inside(
            Path::new("a/link"),
            Path::new("/etc/passwd")
        ));
    }
}
