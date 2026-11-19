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
}
