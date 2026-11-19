//! `speedyspec app backup` and `app restore`. The tests that need a database use only the throwaway MySQL and
//! MariaDB from `scripts/mysql.sh up`:
//!
//!   eval "$(scripts/mysql.sh env)"
//!   cargo test -p speedyspec-cli --test backup -- --ignored --test-threads=1

mod common;

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use common::{Run, Speedyspec};
use flate2::read::GzDecoder;
use mysql::prelude::Queryable;
use speedyspec_testkit::{Entry, Route, Server, build_zip};
use tempfile::TempDir;

/// A distinctive password, so a leak is easy to spot.
const SECRET_MARK: &str = "never-print-me";

fn servers() -> Vec<(&'static str, String)> {
    let found: Vec<(&str, String)> = [
        ("mysql", "SPEEDYSPEC_E2E_MYSQL"),
        ("mariadb", "SPEEDYSPEC_E2E_MARIADB"),
    ]
    .into_iter()
    .filter_map(|(label, var)| std::env::var(var).ok().map(|url| (label, url)))
    .collect();
    assert!(
        !found.is_empty(),
        "set SPEEDYSPEC_E2E_MYSQL / SPEEDYSPEC_E2E_MARIADB with eval \"$(scripts/mysql.sh env)\"; \
         these tests never use any other database"
    );
    found
}

/// A database and its own user, dropped afterwards.
struct Database {
    admin: mysql::Conn,
    name: String,
    user: String,
    password: String,
    host: String,
    port: u16,
}

impl Database {
    fn new(url: &str, tag: &str) -> Database {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let opts = mysql::Opts::from_url(url).unwrap();
        let mut admin = mysql::Conn::new(opts.clone()).unwrap();
        let name = format!("speedyspec_bk_{}_{tag}_{n}", std::process::id());
        let user = format!("b{}_{n}", std::process::id());
        let password = format!("{SECRET_MARK}-{n}");
        admin
            .query_drop(format!("CREATE DATABASE `{name}`"))
            .unwrap();
        admin
            .query_drop(format!(
                "CREATE USER '{user}'@'%' IDENTIFIED BY '{password}'"
            ))
            .unwrap();
        admin
            .query_drop(format!("GRANT ALL ON `{name}`.* TO '{user}'@'%'"))
            .unwrap();
        Database {
            admin,
            name,
            user,
            password,
            host: opts.get_ip_or_hostname().to_string(),
            port: opts.get_tcp_port(),
        }
    }

    fn exec(&mut self, sql: &str) {
        self.admin
            .query_drop(format!("USE `{}`", self.name))
            .unwrap();
        self.admin.query_drop(sql).unwrap();
    }

    fn value(&mut self, sql: &str) -> Option<String> {
        self.admin
            .query_drop(format!("USE `{}`", self.name))
            .unwrap();
        self.admin.query_first(sql).unwrap()
    }

    fn tables(&mut self) -> Vec<String> {
        self.admin
            .query_drop(format!("USE `{}`", self.name))
            .unwrap();
        self.admin.query("SHOW TABLES").unwrap()
    }

    fn json(&self, prefix: &str) -> String {
        serde_json::json!({
            "host": self.host, "port": self.port, "database": self.name,
            "user": self.user, "password": self.password, "tablePrefix": prefix,
        })
        .to_string()
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let _ = self
            .admin
            .query_drop(format!("DROP DATABASE IF EXISTS `{}`", self.name));
        let _ = self
            .admin
            .query_drop(format!("DROP USER IF EXISTS '{}'@'%'", self.user));
    }
}

fn app_zip() -> Vec<u8> {
    build_zip(&[
        ("index.php", Entry::file("<?php // the site")),
        ("cache/", Entry::Dir),
        ("cache/page.html", Entry::file("cached")),
        ("content/", Entry::Dir),
        ("content/upload.txt", Entry::file("an upload")),
    ])
}

/// A `backup-app` install named `name` whose db.json is `db_json`.
struct Site {
    home: PathBuf,
    base: TempDir,
    name: String,
}

fn install(home: &Path, server: &Server, name: &str, db_json: &str) -> Site {
    let base = TempDir::new().unwrap();
    let out = Speedyspec::with_home(&["backup-app"], home)
        .env(
            "BACKUP_APP_ARCHIVE_URL",
            format!("{}/app.zip", server.url()),
        )
        .run(&[
            "--no-interaction",
            "app",
            "install",
            "backup-app",
            "--name",
            name,
            "--base",
            base.path().to_str().unwrap(),
        ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    fs::write(base.path().join("db.json"), db_json).unwrap();
    Site {
        home: home.to_path_buf(),
        base,
        name: name.to_owned(),
    }
}

impl Site {
    fn run(&self, args: &[&str]) -> Run {
        let mut all = vec!["--no-interaction"];
        all.extend_from_slice(args);
        Speedyspec::with_home(&["backup-app"], &self.home).run(&all)
    }

    fn backup(&self, to: &Path) -> Run {
        self.run(&[
            "app",
            "backup",
            "--install",
            &self.name,
            "--to",
            to.to_str().unwrap(),
        ])
    }
}

fn entries(archive: &Path) -> Vec<String> {
    let mut tar = tar::Archive::new(GzDecoder::new(File::open(archive).unwrap()));
    tar.entries()
        .unwrap()
        .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
        .collect()
}

fn read_entry(archive: &Path, name: &str) -> String {
    let mut tar = tar::Archive::new(GzDecoder::new(File::open(archive).unwrap()));
    for entry in tar.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path().unwrap().to_string_lossy() == name {
            let mut text = String::new();
            entry.read_to_string(&mut text).unwrap();
            return text;
        }
    }
    panic!("{name} is not in the archive");
}

fn unreachable_db() -> String {
    serde_json::json!({"host": "127.0.0.1", "port": 1, "database": "nope", "user": "nope", "password": format!("{SECRET_MARK}-x")}).to_string()
}

// ----- app backup

#[test]
fn an_unreachable_database_fails_before_any_file_is_left() {
    let (home, server) = (
        TempDir::new().unwrap(),
        Server::start(vec![("/app.zip", Route::ok(app_zip()))]),
    );
    let site = install(home.path(), &server, "blog", &unreachable_db());
    let out_dir = TempDir::new().unwrap();
    let out = site.backup(&out_dir.path().join("b.tar.gz"));
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(!out.stderr.contains(SECRET_MARK), "{}", out.stderr);
    assert_eq!(fs::read_dir(out_dir.path()).unwrap().count(), 0);
}

#[test]
fn to_refuses_an_existing_file() {
    let (home, server) = (
        TempDir::new().unwrap(),
        Server::start(vec![("/app.zip", Route::ok(app_zip()))]),
    );
    let site = install(home.path(), &server, "blog", &unreachable_db());
    let out_dir = TempDir::new().unwrap();
    let to = out_dir.path().join("b.tar.gz");
    fs::write(&to, "keep me").unwrap();
    let out = site.backup(&to);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("already exists; use --force"),
        "{}",
        out.stderr
    );
    assert_eq!(fs::read_to_string(&to).unwrap(), "keep me");
}

#[test]
fn a_backup_needs_a_plugin_that_describes_it() {
    let (home, server) = (
        TempDir::new().unwrap(),
        Server::start(vec![("/app.zip", Route::ok(app_zip()))]),
    );
    let site = install(home.path(), &server, "blog", &unreachable_db());
    let out = Speedyspec::with_home(&[], &site.home).run(&["app", "backup", "--install", "blog"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("no plugin says what a backup of backup-app holds"),
        "{}",
        out.stderr
    );
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn backup_writes_the_documented_archive() {
    for (label, url) in servers() {
        let mut db = Database::new(&url, "doc");
        db.exec("CREATE TABLE `wp_options` (`option_name` varchar(191) PRIMARY KEY, `option_value` longtext)");
        db.exec("INSERT INTO `wp_options` VALUES ('siteurl','https://old.example')");
        let (home, server) = (
            TempDir::new().unwrap(),
            Server::start(vec![("/app.zip", Route::ok(app_zip()))]),
        );
        let site = install(home.path(), &server, "blog", &db.json("wp_"));
        let out = site.run(&["app", "backup", "--install", "blog"]);
        assert_eq!(out.code, 0, "{label}: {}", out.stderr);
        let path = PathBuf::from(out.stdout.trim());
        assert!(
            path.starts_with(home.path().join("backups")),
            "{label}: {path:?}"
        );
        assert!(
            out.stderr.contains("holds the site's database and secrets"),
            "{label}: {}",
            out.stderr
        );
        let names = entries(&path);
        assert_eq!(
            names.first().map(String::as_str),
            Some("manifest.json"),
            "{label}"
        );
        assert_eq!(
            names.last().map(String::as_str),
            Some("database.sql"),
            "{label}"
        );
        assert!(
            names.contains(&"files/index.php".to_owned()),
            "{label}: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.starts_with("files/cache")),
            "{label}: {names:?}"
        );
        let manifest: serde_json::Value =
            serde_json::from_str(&read_entry(&path, "manifest.json")).unwrap();
        assert_eq!(manifest["app"], "backup-app", "{label}");
        assert_eq!(manifest["site_url"], "https://old.example", "{label}");
        assert_eq!(manifest["database"]["table_prefix"], "wp_", "{label}");
        assert_eq!(manifest["source"]["install"], "blog", "{label}");
        assert!(
            read_entry(&path, "database.sql").contains("CREATE TABLE `wp_options`"),
            "{label}"
        );
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn the_password_is_nowhere_in_the_manifest_or_output() {
    for (label, url) in servers() {
        let mut db = Database::new(&url, "secret");
        db.exec("CREATE TABLE `wp_options` (`option_name` varchar(191) PRIMARY KEY, `option_value` longtext)");
        let (home, server) = (
            TempDir::new().unwrap(),
            Server::start(vec![("/app.zip", Route::ok(app_zip()))]),
        );
        let site = install(home.path(), &server, "blog", &db.json("wp_"));
        let to = home.path().join("b.tar.gz");
        let out = site.backup(&to);
        assert_eq!(out.code, 0, "{label}: {}", out.stderr);
        assert!(
            !out.stdout.contains(SECRET_MARK) && !out.stderr.contains(SECRET_MARK),
            "{label}"
        );
        assert!(
            !read_entry(&to, "manifest.json").contains(SECRET_MARK),
            "{label}"
        );
    }
}

// ----- app restore

/// A source database with the options a WordPress-like app has: the site URL, a serialized value holding it, and a
/// key that carries the prefix.
fn fill_source(db: &mut Database, prefix: &str) {
    db.exec(&format!(
        "CREATE TABLE `{prefix}options` (`option_name` varchar(191) PRIMARY KEY, `option_value` longtext)"
    ));
    db.exec(&format!(
        "INSERT INTO `{prefix}options` VALUES ('siteurl','https://old.example'), \
         ('widget','a:1:{{s:3:\"url\";s:21:\"https://old.example/x\";}}'), \
         ('{prefix}user_roles','a:0:{{}}'), ('{prefix}page_for_privacy_policy','3')"
    ));
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn restore_over_content_needs_yes_when_not_interactive() {
    for (label, url) in servers() {
        let mut db = Database::new(&url, "yes");
        fill_source(&mut db, "wp_");
        let (home, server) = (
            TempDir::new().unwrap(),
            Server::start(vec![("/app.zip", Route::ok(app_zip()))]),
        );
        let site = install(home.path(), &server, "blog", &db.json("wp_"));
        let archive = home.path().join("b.tar.gz");
        assert_eq!(site.backup(&archive).code, 0, "{label}");
        fs::write(site.base.path().join("index.php"), "changed").unwrap();
        let out = site.run(&[
            "app",
            "restore",
            archive.to_str().unwrap(),
            "--install",
            "blog",
        ]);
        assert_eq!(out.code, 2, "{label}: {}", out.stderr);
        assert_eq!(
            fs::read_to_string(site.base.path().join("index.php")).unwrap(),
            "changed",
            "{label}"
        );
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn same_site_restore_undoes_a_change() {
    for (label, url) in servers() {
        let mut db = Database::new(&url, "same");
        fill_source(&mut db, "wp_");
        let (home, server) = (
            TempDir::new().unwrap(),
            Server::start(vec![("/app.zip", Route::ok(app_zip()))]),
        );
        let site = install(home.path(), &server, "blog", &db.json("wp_"));
        let archive = home.path().join("b.tar.gz");
        assert_eq!(site.backup(&archive).code, 0, "{label}");
        db.exec("DELETE FROM `wp_options` WHERE option_name = 'widget'");
        fs::write(site.base.path().join("index.php"), "changed").unwrap();
        let out = site.run(&[
            "app",
            "restore",
            archive.to_str().unwrap(),
            "--install",
            "blog",
            "--yes",
        ]);
        assert_eq!(out.code, 0, "{label}: {}", out.stderr);
        assert!(
            db.value("SELECT option_value FROM wp_options WHERE option_name = 'widget'")
                .is_some(),
            "{label}"
        );
        assert_eq!(
            fs::read_to_string(site.base.path().join("index.php")).unwrap(),
            "<?php // the site",
            "{label}"
        );
        assert!(
            !out.stderr.contains(SECRET_MARK) && !out.stdout.contains(SECRET_MARK),
            "{label}"
        );
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn restore_to_another_url_and_prefix() {
    for (label, url) in servers() {
        let mut source_db = Database::new(&url, "src");
        fill_source(&mut source_db, "wp_");
        let mut target_db = Database::new(&url, "dst");
        let (home, server) = (
            TempDir::new().unwrap(),
            Server::start(vec![("/app.zip", Route::ok(app_zip()))]),
        );
        let source = install(home.path(), &server, "source", &source_db.json("wp_"));
        let target = install(home.path(), &server, "target", &target_db.json("site_"));
        let target_config = fs::read_to_string(target.base.path().join("db.json")).unwrap();
        let archive = home.path().join("b.tar.gz");
        assert_eq!(source.backup(&archive).code, 0, "{label}");
        let out = target.run(&[
            "app",
            "restore",
            archive.to_str().unwrap(),
            "--install",
            "target",
            "--url",
            "https://new.example.org",
            "--yes",
        ]);
        assert_eq!(out.code, 0, "{label}: {}", out.stderr);
        assert_eq!(target_db.tables(), ["site_options"], "{label}");
        assert_eq!(
            target_db
                .value("SELECT option_value FROM site_options WHERE option_name = 'siteurl'")
                .as_deref(),
            Some("https://new.example.org"),
            "{label}"
        );
        assert_eq!(
            target_db
                .value("SELECT option_value FROM site_options WHERE option_name = 'widget'")
                .as_deref(),
            Some("a:1:{s:3:\"url\";s:25:\"https://new.example.org/x\";}"),
            "{label}"
        );
        assert!(
            target_db
                .value(
                    "SELECT option_value FROM site_options WHERE option_name = 'site_user_roles'"
                )
                .is_some(),
            "{label}"
        );
        assert!(
            target_db
                .value("SELECT option_value FROM site_options WHERE option_name = 'wp_page_for_privacy_policy'")
                .is_some(),
            "{label}: an option that only starts with the prefix was renamed"
        );
        assert_eq!(
            fs::read_to_string(target.base.path().join("db.json")).unwrap(),
            target_config,
            "{label}"
        );
        // Its database was empty, but its files are replaced, so they were backed up first.
        assert!(
            out.stderr.contains("what was there is backed up to"),
            "{label}: {}",
            out.stderr
        );
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn a_restore_takes_a_backup_first() {
    for (label, url) in servers() {
        let mut db = Database::new(&url, "first");
        fill_source(&mut db, "wp_");
        let (home, server) = (
            TempDir::new().unwrap(),
            Server::start(vec![("/app.zip", Route::ok(app_zip()))]),
        );
        let site = install(home.path(), &server, "blog", &db.json("wp_"));
        let archive = home.path().join("b.tar.gz");
        assert_eq!(site.backup(&archive).code, 0, "{label}");
        let out = site.run(&[
            "app",
            "restore",
            archive.to_str().unwrap(),
            "--install",
            "blog",
            "--yes",
        ]);
        assert_eq!(out.code, 0, "{label}: {}", out.stderr);
        assert!(
            out.stderr.contains("what was there is backed up to"),
            "{label}: {}",
            out.stderr
        );
        let backups: Vec<_> = fs::read_dir(home.path().join("backups"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            backups
                .iter()
                .any(|n| n.starts_with("blog-before-restore-")),
            "{label}: {backups:?}"
        );
    }
}

#[test]
fn a_backup_of_another_app_is_refused() {
    let (home, server) = (
        TempDir::new().unwrap(),
        Server::start(vec![("/app.zip", Route::ok(app_zip()))]),
    );
    let site = install(home.path(), &server, "blog", &unreachable_db());
    let archive = home.path().join("other.tar.gz");
    let manifest = serde_json::json!({
        "format": 1, "app": "something-else", "parts": [], "cli_version": "1", "app_version": null,
        "extensions": [], "database": {"engine": "mysql", "table_prefix": "wp_"}, "site_url": null,
        "source": {"install": "x", "driver": "zip"}, "created_at": "2026-11-19T10:00:00Z",
    });
    let bytes = serde_json::to_vec(&manifest).unwrap();
    let gz = flate2::write::GzEncoder::new(
        File::create(&archive).unwrap(),
        flate2::Compression::default(),
    );
    let mut tar = tar::Builder::new(gz);
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_cksum();
    tar.append_data(&mut header, "manifest.json", &bytes[..])
        .unwrap();
    tar.into_inner().unwrap().finish().unwrap();
    let out = site.run(&[
        "app",
        "restore",
        archive.to_str().unwrap(),
        "--install",
        "blog",
        "--yes",
    ]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("is of a something-else install"),
        "{}",
        out.stderr
    );
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn a_large_table_round_trips() {
    for (label, url) in servers() {
        let mut db = Database::new(&url, "big");
        fill_source(&mut db, "wp_");
        db.exec("CREATE TABLE `wp_rows` (`id` int PRIMARY KEY, `text` varchar(255))");
        for chunk in 0..50 {
            let rows: Vec<String> = (0..1000)
                .map(|i| {
                    let id = chunk * 1000 + i;
                    format!("({id},'row {id} https://old.example/{id}')")
                })
                .collect();
            db.exec(&format!("INSERT INTO `wp_rows` VALUES {}", rows.join(",")));
        }
        let checksum = |db: &mut Database| {
            db.value("SELECT CONCAT(COUNT(*), ':', SUM(CRC32(CONCAT(id, '|', text)))) FROM wp_rows")
        };
        let before = checksum(&mut db);
        let (home, server) = (
            TempDir::new().unwrap(),
            Server::start(vec![("/app.zip", Route::ok(app_zip()))]),
        );
        let site = install(home.path(), &server, "blog", &db.json("wp_"));
        let archive = home.path().join("b.tar.gz");
        assert_eq!(site.backup(&archive).code, 0, "{label}");
        db.exec("DELETE FROM `wp_rows` WHERE id % 2 = 0");
        let out = site.run(&[
            "app",
            "restore",
            archive.to_str().unwrap(),
            "--install",
            "blog",
            "--yes",
        ]);
        assert_eq!(out.code, 0, "{label}: {}", out.stderr);
        assert_eq!(checksum(&mut db), before, "{label}");
        assert!(
            before.as_deref().is_some_and(|c| c.starts_with("50000:")),
            "{label}: {before:?}"
        );
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn a_site_without_tables_is_still_asked_about_for_its_files() {
    for (label, url) in servers() {
        let mut source_db = Database::new(&url, "nf_src");
        fill_source(&mut source_db, "wp_");
        let target_db = Database::new(&url, "nf_dst");
        let (home, server) = (
            TempDir::new().unwrap(),
            Server::start(vec![("/app.zip", Route::ok(app_zip()))]),
        );
        let source = install(home.path(), &server, "source", &source_db.json("wp_"));
        let target = install(home.path(), &server, "target", &target_db.json("wp_"));
        fs::write(
            target.base.path().join("content/since-the-backup.txt"),
            "a new upload",
        )
        .unwrap();
        let archive = home.path().join("b.tar.gz");
        assert_eq!(source.backup(&archive).code, 0, "{label}");
        let out = target.run(&[
            "app",
            "restore",
            archive.to_str().unwrap(),
            "--install",
            "target",
        ]);
        assert_eq!(out.code, 2, "{label}: {}", out.stderr);
        assert!(
            target
                .base
                .path()
                .join("content/since-the-backup.txt")
                .exists(),
            "{label}"
        );
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn another_prefix_is_asked_about_and_backed_up() {
    for (label, url) in servers() {
        let mut source_db = Database::new(&url, "op_src");
        fill_source(&mut source_db, "wp_");
        let mut target_db = Database::new(&url, "op_dst");
        target_db.exec("CREATE TABLE `other_options` (`option_name` varchar(191) PRIMARY KEY, `option_value` longtext)");
        target_db.exec("INSERT INTO `other_options` VALUES ('keep','the other install''s')");
        let (home, server) = (
            TempDir::new().unwrap(),
            Server::start(vec![("/app.zip", Route::ok(app_zip()))]),
        );
        let source = install(home.path(), &server, "source", &source_db.json("wp_"));
        let target = install(home.path(), &server, "target", &target_db.json("site_"));
        let archive = home.path().join("b.tar.gz");
        assert_eq!(source.backup(&archive).code, 0, "{label}");
        let out = target.run(&[
            "app",
            "restore",
            archive.to_str().unwrap(),
            "--install",
            "target",
            "--table-prefix",
            "other_",
            "--yes",
        ]);
        assert_eq!(out.code, 0, "{label}: {}", out.stderr);
        let before = fs::read_dir(home.path().join("backups"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("target-before-restore-")
            })
            .unwrap_or_else(|| panic!("{label}: no backup was taken first"));
        assert!(
            read_entry(&before, "database.sql").contains("the other install\\'s"),
            "{label}: the tables about to be overwritten were not backed up"
        );
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn restoring_onto_another_url_without_url_is_warned_about() {
    for (label, url) in servers() {
        let mut source_db = Database::new(&url, "nu_src");
        fill_source(&mut source_db, "wp_");
        let mut target_db = Database::new(&url, "nu_dst");
        target_db.exec("CREATE TABLE `wp_options` (`option_name` varchar(191) PRIMARY KEY, `option_value` longtext)");
        target_db.exec("INSERT INTO `wp_options` VALUES ('siteurl','https://target.example')");
        let (home, server) = (
            TempDir::new().unwrap(),
            Server::start(vec![("/app.zip", Route::ok(app_zip()))]),
        );
        let source = install(home.path(), &server, "source", &source_db.json("wp_"));
        let target = install(home.path(), &server, "target", &target_db.json("wp_"));
        let archive = home.path().join("b.tar.gz");
        assert_eq!(source.backup(&archive).code, 0, "{label}");
        let out = target.run(&[
            "app",
            "restore",
            archive.to_str().unwrap(),
            "--install",
            "target",
            "--yes",
        ]);
        assert_eq!(out.code, 0, "{label}: {}", out.stderr);
        assert!(
            out.stderr.contains("https://old.example")
                && out.stderr.contains("--url https://target.example"),
            "{label}: {}",
            out.stderr
        );
    }
}
