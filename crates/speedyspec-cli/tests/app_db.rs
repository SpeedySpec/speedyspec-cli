//! `speedyspec app db …` against the throwaway MySQL and MariaDB from `scripts/mysql.sh up`,
//! never anything else:
//!
//!   eval "$(scripts/mysql.sh env)"
//!   cargo test -p speedyspec-cli --test app_db -- --ignored --test-threads=1
//!
//! Each test zip-installs the `db-app` fixture, points its db.json at a fresh database, and drops
//! the database afterwards.

mod common;

use std::fs;
use std::path::PathBuf;

use common::{Run, Speedyspec};
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

/// A database and a user with a distinctive password for it, both dropped afterwards.
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
        let name = format!("speedyspec_cli_{}_{tag}_{n}", std::process::id());
        let user = format!("u{}_{n}", std::process::id());
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

    fn checksum(&mut self, table: &str) -> Option<u64> {
        let row: Option<(String, Option<u64>)> = self
            .admin
            .query_first(format!("CHECKSUM TABLE `{}`.`{table}`", self.name))
            .unwrap();
        row.and_then(|r| r.1)
    }

    fn db_json(&self) -> String {
        serde_json::json!({
            "host": self.host, "port": self.port, "database": self.name,
            "user": self.user, "password": self.password,
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

/// A zip install of `db-app` named `name` whose db.json points at `database`.
struct Site {
    home: TempDir,
    base: TempDir,
    _server: Server,
    name: String,
}

impl Site {
    fn new(name: &str, database: &Database) -> Site {
        let server = Server::start(vec![(
            "/app.zip",
            Route::ok(build_zip(&[("index.php", Entry::file("<?php"))])),
        )]);
        let (home, base) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let out = Speedyspec::with_home(&["db-app"], home.path())
            .env("DB_APP_ARCHIVE_URL", format!("{}/app.zip", server.url()))
            .run(&[
                "--no-interaction",
                "app",
                "install",
                "db-app",
                "--driver",
                "zip",
                "--base",
                base.path().to_str().unwrap(),
                "--name",
                name,
            ]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        fs::write(base.path().join("db.json"), database.db_json()).unwrap();
        Site {
            home,
            base,
            _server: server,
            name: name.to_owned(),
        }
    }

    fn db(&self, args: &[&str]) -> Run {
        let mut all = vec!["--no-interaction", "app", "db"];
        all.extend_from_slice(args);
        all.extend_from_slice(&["--install", &self.name]);
        Speedyspec::with_home(&["db-app"], self.home.path()).run(&all)
    }

    fn record(&self) -> String {
        fs::read_to_string(
            self.home
                .path()
                .join("installs")
                .join(format!("{}.json", self.name)),
        )
        .unwrap()
    }

    fn base(&self) -> PathBuf {
        self.base.path().to_path_buf()
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn export_then_import_round_trips() {
    for (label, url) in servers() {
        let mut source = Database::new(&url, "src");
        source.exec("CREATE TABLE posts (id int PRIMARY KEY, body longtext, raw longblob)");
        source.exec("INSERT INTO posts VALUES (1, 'it''s 🙂', 0x00ff), (2, NULL, NULL)");
        let mut target = Database::new(&url, "dst");
        let (from, to) = (Site::new("from", &source), Site::new("to", &target));
        let dump = from.base().join("dump.sql");
        let out = from.db(&["export", "--to", dump.to_str().unwrap()]);
        assert_eq!(out.code, 0, "{label}: {}", out.stderr);
        let out = to.db(&["import", dump.to_str().unwrap(), "--yes"]);
        assert_eq!(out.code, 0, "{label}: {}", out.stderr);
        assert!(out.stderr.contains("statement"), "{label}: {}", out.stderr);
        assert_eq!(
            source.checksum("posts"),
            target.checksum("posts"),
            "{label}"
        );
        assert!(target.checksum("posts").is_some(), "{label}");
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn export_to_stdout_and_mode_of_the_file() {
    for (label, url) in servers() {
        let mut database = Database::new(&url, "out");
        database.exec("CREATE TABLE t (id int)");
        let site = Site::new("out", &database);
        let out = site.db(&["export"]);
        assert_eq!(out.code, 0, "{label}: {}", out.stderr);
        assert!(
            out.stdout.contains("CREATE TABLE `t`"),
            "{label}: {}",
            out.stdout
        );
        let file = site.base().join("dump.sql");
        assert_eq!(
            site.db(&["export", "--to", file.to_str().unwrap()]).code,
            0,
            "{label}"
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600,
            "{label}"
        );
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn export_refuses_an_existing_file() {
    for (label, url) in servers() {
        let database = Database::new(&url, "ex");
        let site = Site::new("ex", &database);
        let file = site.base().join("dump.sql");
        fs::write(&file, "keep me").unwrap();
        let out = site.db(&["export", "--to", file.to_str().unwrap()]);
        assert_eq!(out.code, 1, "{label}: {}", out.stderr);
        assert!(
            out.stderr.contains("already exists"),
            "{label}: {}",
            out.stderr
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), "keep me", "{label}");
        assert_eq!(
            site.db(&["export", "--to", file.to_str().unwrap(), "--force"])
                .code,
            0,
            "{label}"
        );
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn import_needs_confirmation() {
    for (label, url) in servers() {
        let database = Database::new(&url, "conf");
        let site = Site::new("conf", &database);
        let file = site.base().join("dump.sql");
        fs::write(&file, "CREATE TABLE never (id int);").unwrap();
        let out = site.db(&["import", file.to_str().unwrap()]);
        assert_eq!(out.code, 2, "{label}: {}", out.stderr);
        assert!(
            out.stderr.contains("confirm with --yes"),
            "{label}: {}",
            out.stderr
        );
        let mut database = database;
        assert_eq!(
            database.checksum("never"),
            None,
            "{label}: nothing was imported"
        );
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn query_runs() {
    for (label, url) in servers() {
        let database = Database::new(&url, "q");
        let site = Site::new("q", &database);
        let out = site.db(&["query", "SELECT 1 + 1 AS two"]);
        assert_eq!(out.code, 0, "{label}: {}", out.stderr);
        assert_eq!(out.stdout, "two\n2\n", "{label}");
        let out = site.db(&["query", "CREATE TABLE made (id int)"]);
        assert_eq!(out.stdout, "0 rows affected\n", "{label}");
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn tables_size_and_check_print_lines() {
    for (label, url) in servers() {
        let mut database = Database::new(&url, "tsc");
        database.exec("CREATE TABLE alpha (id int)");
        database.exec("CREATE TABLE beta (id int)");
        let site = Site::new("tsc", &database);
        let tables = site.db(&["tables"]);
        assert_eq!(tables.code, 0, "{label}: {}", tables.stderr);
        let names: Vec<&str> = tables
            .stdout
            .lines()
            .map(|l| l.split('\t').next().unwrap())
            .collect();
        assert_eq!(names, vec!["alpha", "beta"], "{label}");
        let size = site.db(&["size"]);
        assert!(
            size.stdout.starts_with("data\t") && size.stdout.contains("\nindex\t"),
            "{label}: {}",
            size.stdout
        );
        let check = site.db(&["check"]);
        assert_eq!(check.stdout, "alpha\tOK\t\nbeta\tOK\t\n", "{label}");
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn the_password_never_appears() {
    for (label, url) in servers() {
        let mut database = Database::new(&url, "pw");
        database.exec("CREATE TABLE t (id int)");
        let site = Site::new("pw", &database);
        let file = site.base().join("d.sql");
        let mut runs = vec![
            site.db(&["export", "--to", file.to_str().unwrap()]),
            site.db(&["tables"]),
            site.db(&["size"]),
            site.db(&["check"]),
            site.db(&["query", "SELECT nope FROM nowhere"]),
            site.db(&["import", file.to_str().unwrap(), "--yes"]),
        ];
        fs::write(
            site.base().join("db.json"),
            database.db_json().replace(SECRET_MARK, "wrong-mark"),
        )
        .unwrap();
        runs.push(site.db(&["tables"]));
        runs.push(Speedyspec::new(&[]).run(&["app", "db", "--help"]));
        for run in &runs {
            assert!(
                !run.stdout.contains(SECRET_MARK) && !run.stderr.contains(SECRET_MARK),
                "{label}: {} {}",
                run.stdout,
                run.stderr
            );
        }
        assert!(
            runs[6].stderr.contains("access denied"),
            "{label}: {}",
            runs[6].stderr
        );
        assert!(!site.record().contains(SECRET_MARK), "{label}");
    }
}
