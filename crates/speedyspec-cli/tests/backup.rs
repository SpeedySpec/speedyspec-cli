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
    _base: TempDir,
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
        _base: base,
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
