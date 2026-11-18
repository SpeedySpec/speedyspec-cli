//! Asking an app's plugin where its database is, through `speedyspec_app_database`. The hidden
//! `self database --install <name>` prints the answer (without the password).

mod common;

use std::fs;
use std::path::Path;

use common::{Run, Speedyspec};
use serde_json::{Value as Json, json};
use tempfile::TempDir;

/// An install record for `app` at `root`, as `app install` would leave it.
fn record(home: &Path, name: &str, app: &str, root: &Path) {
    let dir = home.join("installs");
    fs::create_dir_all(&dir).unwrap();
    let record = json!({
        "schema": 1, "name": name, "app": app, "parts": [], "driver": "zip",
        "location": root.to_string_lossy(), "cli_version": "0.0.0",
        "installed_at": "2026-11-18T00:00:00Z", "status": "installed",
    });
    fs::write(dir.join(format!("{name}.json")), record.to_string()).unwrap();
}

fn database(home: &Path, plugins: &[&str], name: &str, env: &[(&str, &str)]) -> Run {
    let mut run = Speedyspec::with_home(plugins, home);
    for (key, value) in env {
        run = run.env(key, value);
    }
    run.run(&["self", "database", "--install", name])
}

fn answer(out: &Run) -> Json {
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    serde_json::from_str(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", out.stdout))
}

#[test]
fn database_from_a_direct_answer() {
    let (home, root) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    fs::write(
        root.path().join("db.json"),
        r#"{"host": "db.example.test", "database": "shop", "user": "shop", "password": "pw-direct-1", "port": 3307, "tablePrefix": "s_"}"#,
    )
    .unwrap();
    record(home.path(), "shop", "db-app", root.path());
    let out = database(home.path(), &["db-app"], "shop", &[]);
    let got = answer(&out);
    assert_eq!(
        got,
        json!({"host": "db.example.test", "port": 3307, "database": "shop", "user": "shop", "table_prefix": "s_", "charset": "utf8mb4"})
    );
    assert!(!out.stdout.contains("pw-direct-1") && !out.stderr.contains("pw-direct-1"));
}

#[test]
fn database_from_a_task() {
    let (home, root) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    fs::write(
        root.path().join("config.php"),
        "<?php\ndefine('DB_HOST', getenv('DB_HOST'));\ndefine('DB_NAME', getenv('DB_NAME'));\n\
         define('DB_USER', getenv('DB_USER'));\ndefine('DB_PASSWORD', getenv('DB_PASSWORD'));\n\
         define('DB_PORT', getenv('DB_PORT'));\n$table_prefix = 'cfg_';\nrequire_once ABSPATH . 'settings.php';\n",
    )
    .unwrap();
    record(home.path(), "blog", "config-app", root.path());
    let env = [
        ("DB_HOST", "mysql.internal"),
        ("DB_NAME", "blog"),
        ("DB_USER", "blogger"),
        ("DB_PASSWORD", "pw-from-env-2"),
        ("DB_PORT", "3308"),
    ];
    let out = database(home.path(), &["db-app-task"], "blog", &env);
    let got = answer(&out);
    assert_eq!(got["host"], "mysql.internal");
    assert_eq!(got["port"], 3308);
    assert_eq!(got["table_prefix"], "cfg_");
    assert!(!out.stdout.contains("pw-from-env-2"));
}

#[test]
fn no_plugin_answers() {
    let (home, root) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    record(home.path(), "shop", "db-app", root.path());
    let out = database(home.path(), &[], "shop", &[]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(
        out.stderr
            .contains("no plugin says where the database of db-app is"),
        "{}",
        out.stderr
    );
}

#[test]
fn bad_answer_is_blamed_on_the_plugin() {
    let (home, root) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    fs::write(
        root.path().join("db.json"),
        r#"{"host": "db", "database": "shop", "user": "shop", "password": "x", "port": 0}"#,
    )
    .unwrap();
    record(home.path(), "shop", "db-app", root.path());
    let out = database(home.path(), &["db-app"], "shop", &[]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(
        out.stderr
            .contains("speedyspec-fixture/db-app: speedyspec_app_database: ")
            && out.stderr.contains("port"),
        "{}",
        out.stderr
    );
}

#[test]
fn unknown_install() {
    let home = TempDir::new().unwrap();
    let out = database(home.path(), &["db-app"], "nope", &[]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(
        out.stderr
            .contains("no install named nope; app installs lists them"),
        "{}",
        out.stderr
    );
}

#[test]
fn an_install_whose_directory_is_gone_is_named() {
    let (home, root) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let gone = root.path().join("site");
    record(home.path(), "gone", "db-app", &gone);
    let out = database(home.path(), &["db-app"], "gone", &[]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(
        out.stderr.contains(&format!(
            "install gone's directory {} is gone",
            gone.display()
        )),
        "{}",
        out.stderr
    );
    assert!(!out.stderr.contains("JsonException"), "{}", out.stderr);

    let listed = Speedyspec::with_home(&[], home.path()).run(&["app", "installs"]);
    assert_eq!(listed.code, 0, "{}", listed.stderr);
    assert!(listed.stdout.starts_with("gone\t"), "{}", listed.stdout);
    assert!(
        listed.stderr.contains(&format!(
            "install gone's directory {} is gone",
            gone.display()
        )),
        "{}",
        listed.stderr
    );
}
