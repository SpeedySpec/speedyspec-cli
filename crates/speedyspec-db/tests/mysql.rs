//! Against the throwaway MySQL and MariaDB from `scripts/mysql.sh up`, never anything else:
//!
//!   eval "$(scripts/mysql.sh env)"
//!   cargo test -p speedyspec-db --test mysql -- --ignored --test-threads=1
//!
//! Each test makes its own database and drops it afterwards.

use std::io::Write;

use mysql::prelude::Queryable;
use speedyspec_db::{Connection, ExportOptions, QueryOutcome};
use speedyspec_driver::DatabaseConnection;

/// The servers to test against, from the variables `scripts/mysql.sh env` prints.
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

/// A fresh database on `url`, dropped when the guard goes.
struct Scratch {
    admin: mysql::Conn,
    name: String,
    connection: DatabaseConnection,
}

impl Scratch {
    fn new(url: &str, prefix: &str) -> Scratch {
        let opts = mysql::Opts::from_url(url).unwrap();
        let mut admin = mysql::Conn::new(opts.clone()).unwrap();
        let name = format!(
            "speedyspec_t_{}",
            std::process::id() as u64 * 1000 + rand_suffix()
        );
        admin
            .query_drop(format!("CREATE DATABASE `{name}` CHARACTER SET utf8mb4"))
            .unwrap();
        let connection = DatabaseConnection {
            host: opts.get_ip_or_hostname().to_string(),
            database: name.clone(),
            user: opts.get_user().unwrap().to_owned(),
            password: opts.get_pass().unwrap().to_owned(),
            port: opts.get_tcp_port(),
            socket: None,
            table_prefix: prefix.to_owned(),
            charset: "utf8mb4".to_owned(),
            engine: "mysql".to_owned(),
        };
        Scratch {
            admin,
            name,
            connection,
        }
    }

    fn exec(&mut self, sql: &str) {
        self.admin
            .query_drop(format!("USE `{}`", self.name))
            .unwrap();
        self.admin.query_drop(sql).unwrap();
    }

    fn checksums(&mut self) -> Vec<(String, Option<u64>)> {
        self.admin
            .query_drop(format!("USE `{}`", self.name))
            .unwrap();
        let tables: Vec<String> = self.admin.query("SHOW TABLES").unwrap();
        tables
            .into_iter()
            .map(|table| {
                let row: (String, Option<u64>) = self
                    .admin
                    .query_first(format!("CHECKSUM TABLE `{}`", table.replace('`', "``")))
                    .unwrap()
                    .unwrap();
                (table, row.1)
            })
            .collect()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = self
            .admin
            .query_drop(format!("DROP DATABASE IF EXISTS `{}`", self.name));
    }
}

fn rand_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

const TRICKY: &str = "
CREATE TABLE `t_values` (
  id int NOT NULL PRIMARY KEY,
  bin longblob NULL,
  txt longtext NULL,
  d date NULL,
  n decimal(10,2) NULL,
  f double NULL,
  b bit(8) NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
CREATE TABLE `t_empty` (id int NOT NULL PRIMARY KEY) ENGINE=InnoDB;
CREATE TABLE `we;ird` (`col``q` varchar(10) NULL) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
";

fn fill(scratch: &mut Scratch) {
    scratch.exec("SET SESSION sql_mode = ''");
    for statement in TRICKY.split(";\n").filter(|s| !s.trim().is_empty()) {
        scratch.exec(statement);
    }
    scratch.exec("SET SESSION sql_mode = ''");
    scratch
        .admin
        .query_drop("SET SESSION sql_mode = ''")
        .unwrap();
    scratch.exec(
        "INSERT INTO t_values VALUES
         (1, 0x00FF0A0D27225C1A, 'NULL', '0000-00-00', 12.50, 1.5e300, b'10100101'),
         (2, NULL, NULL, NULL, NULL, NULL, NULL),
         (3, '', '', '2026-11-18', -0.01, -0.0, b'0'),
         (4, NULL, 'quote '' double \" back \\\\ nul \\0 tab \\t 🙂 ünïcødé', '1970-01-01', 0, 0, b'11111111')",
    );
    let big = "x".repeat(4 * 1024 * 1024);
    scratch.exec(&format!(
        "INSERT INTO t_values (id, txt) VALUES (5, '{big}')"
    ));
    scratch.exec("INSERT INTO `we;ird` VALUES ('a;b'), (NULL)");
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn round_trip_keeps_every_value() {
    for (label, url) in servers() {
        let mut source = Scratch::new(&url, "");
        fill(&mut source);
        let mut dump = Vec::new();
        Connection::open(&source.connection)
            .unwrap()
            .export(&mut dump, &ExportOptions::default())
            .unwrap();
        let mut target = Scratch::new(&url, "");
        let applied = Connection::open(&target.connection)
            .unwrap()
            .import(&dump[..])
            .unwrap_or_else(|e| panic!("{label}: {e}"));
        assert!(applied > 0);
        assert_eq!(source.checksums(), target.checksums(), "{label}");
        assert_eq!(target.checksums().len(), 3, "{label}");
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn imports_a_mysqldump_file() {
    let dump = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/mysqldump-8.4.sql"
    ))
    .unwrap();
    for (label, url) in servers() {
        let mut target = Scratch::new(&url, "");
        Connection::open(&target.connection)
            .unwrap()
            .import(&dump[..])
            .unwrap_or_else(|e| panic!("{label}: {e}"));
        let tables: Vec<String> = target.checksums().into_iter().map(|(t, _)| t).collect();
        assert_eq!(tables, vec!["wp_blobs", "wp_options"], "{label}");
        let count: Option<u64> = target
            .admin
            .query_first(format!("SELECT COUNT(*) FROM `{}`.wp_options", target.name))
            .unwrap();
        assert_eq!(count, Some(5), "{label}");
        let blob: Option<Vec<u8>> = target
            .admin
            .query_first(format!(
                "SELECT data FROM `{}`.wp_blobs WHERE id = 1",
                target.name
            ))
            .unwrap();
        assert_eq!(
            blob,
            Some(vec![0x00, 0xFF, 0x0A, 0x0D, 0x27, 0x22, 0x5C]),
            "{label}"
        );
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn a_dump_that_switches_database_is_refused() {
    let dump = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/mysqldump-8.4-databases.sql"
    ))
    .unwrap();
    for (label, url) in servers() {
        let mut target = Scratch::new(&url, "");
        let error = Connection::open(&target.connection)
            .unwrap()
            .import(&dump[..])
            .unwrap_err();
        assert!(
            error.to_string().contains("switches to another database"),
            "{label}: {error}"
        );
        assert_eq!(error.applied, error.statement - 1, "{label}");
        assert!(
            target.checksums().is_empty(),
            "{label}: nothing was created"
        );
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn broken_dump_names_the_line() {
    for (label, url) in servers() {
        let target = Scratch::new(&url, "");
        let dump = b"CREATE TABLE a (id int);\nINSERT INTO a VALUES (1);\n\nINSERT INTO nope VALUES (2);\n";
        let error = Connection::open(&target.connection)
            .unwrap()
            .import(&dump[..])
            .unwrap_err();
        assert_eq!(
            (error.line, error.statement, error.applied),
            (4, 3, 2),
            "{label}: {error}"
        );
        assert!(error.to_string().contains("line 4"), "{label}: {error}");
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn wrong_password_is_reported_without_it() {
    for (label, url) in servers() {
        let target = Scratch::new(&url, "");
        let mut wrong = target.connection.clone();
        wrong.password = "not-the-password-4711".to_owned();
        let error = Connection::open(&wrong).err().unwrap().to_string();
        assert!(
            error.contains("access denied for user root@"),
            "{label}: {error}"
        );
        assert!(!error.contains("4711"), "{label}: {error}");
        assert!(
            !error.contains(&target.connection.password),
            "{label}: {error}"
        );
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn query_prints_rows_and_affected_counts() {
    for (label, url) in servers() {
        let mut target = Scratch::new(&url, "");
        target.exec("CREATE TABLE q (id int, name varchar(10), raw varbinary(4))");
        let mut connection = Connection::open(&target.connection).unwrap();
        assert_eq!(
            connection
                .query(
                    "INSERT INTO q VALUES (1, 'a', 0x00ff), (2, NULL, NULL)",
                    &mut Vec::new()
                )
                .unwrap(),
            QueryOutcome::Affected(2),
            "{label}"
        );
        let mut out = Vec::new();
        assert_eq!(
            connection
                .query("SELECT * FROM q ORDER BY id", &mut out)
                .unwrap(),
            QueryOutcome::Rows(2),
            "{label}"
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "id\tname\traw\n1\ta\t0x00ff\n2\tNULL\tNULL\n",
            "{label}"
        );
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn tables_size_and_check_respect_the_prefix() {
    for (label, url) in servers() {
        let mut target = Scratch::new(&url, "wp_");
        target.exec("CREATE TABLE wp_posts (id int) ENGINE=InnoDB");
        target.exec("CREATE TABLE wpx_other (id int) ENGINE=InnoDB");
        target.exec("CREATE TABLE other (id int) ENGINE=InnoDB");
        let mut connection = Connection::open(&target.connection).unwrap();
        let names: Vec<String> = connection
            .tables()
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(
            names,
            vec!["wp_posts"],
            "{label}: the prefix is literal, _ included"
        );
        let (data, _index) = connection.size().unwrap();
        assert!(data > 0, "{label}");
        let checked = connection.check().unwrap();
        assert_eq!(checked.len(), 1, "{label}: {checked:?}");
        assert_eq!(checked[0].1, "OK", "{label}: {checked:?}");
        let mut dump = Vec::new();
        connection
            .export(&mut dump, &ExportOptions::default())
            .unwrap();
        let dump = String::from_utf8_lossy(&dump);
        assert!(
            dump.contains("`wp_posts`") && !dump.contains("`other`") && !dump.contains("wpx_other"),
            "{label}"
        );
    }
}

#[test]
#[ignore = "needs scripts/mysql.sh up"]
fn export_streams() {
    for (label, url) in servers() {
        let mut source = Scratch::new(&url, "");
        source.exec("CREATE TABLE many (id int NOT NULL PRIMARY KEY, pad varchar(255))");
        source.exec(
            "INSERT INTO many SELECT seq, REPEAT('p', 200) FROM (
               SELECT a.n + b.n * 1000 AS seq FROM
                 (SELECT ROW_NUMBER() OVER () - 1 AS n FROM information_schema.columns LIMIT 1000) a,
                 (SELECT ROW_NUMBER() OVER () - 1 AS n FROM information_schema.columns LIMIT 200) b
             ) s",
        );
        struct Counting(u64);
        impl Write for Counting {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0 += buf.len() as u64;
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let before = rss_kib();
        let mut sink = Counting(0);
        Connection::open(&source.connection)
            .unwrap()
            .export(&mut sink, &ExportOptions::default())
            .unwrap();
        let grew_kib = rss_kib().saturating_sub(before);
        assert!(sink.0 > 40 * 1024 * 1024, "{label}: wrote {} bytes", sink.0);
        assert!(
            grew_kib < 64 * 1024,
            "{label}: memory grew by {grew_kib} KiB"
        );
    }
}

/// This process's resident memory, in KiB.
fn rss_kib() -> u64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}
