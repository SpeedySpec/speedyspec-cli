//! `speedyspec plugin install` and `plugin remove`, against a loopback server.

mod common;

use std::fs;
use std::path::Path;

use common::{Run, Speedyspec, fixture};
use speedyspec_testkit::{Entry, Route, Server, build_zip};
use tempfile::TempDir;

const GOOD: &str = "speedyspec-fixture/good";

/// The `good` fixture as an archive, with `version` in its manifest and every path under
/// `prefix` (empty for none).
fn good_zip(version: &str, prefix: &str) -> Vec<u8> {
    let php = fs::read_to_string(fixture("good").join("plugin.php")).unwrap();
    let manifest = format!(
        r#"{{"name":"{GOOD}","version":"{version}","contracts":"^1.0","entry":"plugin.php"}}"#
    );
    plugin_zip(&manifest, &php, prefix)
}

fn plugin_zip(manifest: &str, php: &str, prefix: &str) -> Vec<u8> {
    let manifest_name = format!("{prefix}speedyspec-plugin.json");
    let php_name = format!("{prefix}plugin.php");
    build_zip(&[
        (manifest_name.as_str(), Entry::file(manifest)),
        (php_name.as_str(), Entry::file(php)),
    ])
}

fn speedyspec(home: &Path, args: &[&str]) -> Run {
    Speedyspec::with_home(&[], home).run(args)
}

fn plugins_dir(home: &Path) -> std::path::PathBuf {
    home.join("plugins")
}

fn listing(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn installs_from_a_url_then_the_plugin_loads() {
    let server = Server::start(vec![("/p.zip", Route::ok(good_zip("1.0.0", "")))]);
    let home = TempDir::new().unwrap();
    let url = format!("{}/p.zip", server.url());

    let out = speedyspec(home.path(), &["self", "plugin", "install", &url, "--yes"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let target = plugins_dir(home.path()).join("speedyspec-fixture.good");
    assert_eq!(out.stdout, format!("{GOOD}\t1.0.0\t{}\n", target.display()));
    assert_eq!(
        listing(&plugins_dir(home.path())),
        vec!["speedyspec-fixture.good"]
    );

    let out = speedyspec(home.path(), &["hello", "Ada"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "hello Ada\n");
}

#[test]
fn archive_with_one_wrapping_directory_works() {
    let server = Server::start(vec![(
        "/p.zip",
        Route::ok(good_zip("1.0.0", "good-1.0.0/")),
    )]);
    let home = TempDir::new().unwrap();
    let url = format!("{}/p.zip", server.url());
    let out = speedyspec(home.path(), &["self", "plugin", "install", &url, "--yes"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let installed = plugins_dir(home.path()).join("speedyspec-fixture.good");
    assert_eq!(
        listing(&installed),
        vec!["plugin.php", "speedyspec-plugin.json"]
    );
}

#[test]
fn shows_the_warning_and_needs_yes() {
    let server = Server::start(vec![("/p.zip", Route::ok(good_zip("1.0.0", "")))]);
    let home = TempDir::new().unwrap();
    let url = format!("{}/p.zip", server.url());
    let out = speedyspec(home.path(), &["self", "plugin", "install", &url]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains(&url), "{}", out.stderr);
    assert!(
        out.stderr
            .contains("This plugin will run as you, with full access to your files and network."),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("speedyspec: confirm with --yes"),
        "{}",
        out.stderr
    );
    assert_eq!(listing(&plugins_dir(home.path())), Vec::<String>::new());
    assert!(
        server.requests().is_empty(),
        "nothing may be downloaded before confirmation"
    );
}

#[test]
fn installs_from_owner_repo_through_the_api() {
    let server = Server::start(vec![("/asset.zip", Route::ok(good_zip("1.0.0", "")))]);
    let release = format!(
        r#"{{"tag_name":"v1.0.0","zipball_url":"{0}/zipball","assets":[{{"name":"speedyspec-plugin.zip","browser_download_url":"{0}/asset.zip"}}]}}"#,
        server.url()
    );
    server.route("/repos/acme/good/releases/latest", Route::ok(release));
    let home = TempDir::new().unwrap();
    let out = Speedyspec::with_home(&[], home.path())
        .env("SPEEDYSPEC_GITHUB_API", server.url())
        .run(&["self", "plugin", "install", "acme/good", "--yes"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.starts_with(&format!("{GOOD}\t1.0.0\t")),
        "{}",
        out.stdout
    );
    assert!(
        out.stderr.contains("acme/good") && out.stderr.contains("v1.0.0"),
        "{}",
        out.stderr
    );
}

#[test]
fn source_archive_of_a_release_is_unwrapped() {
    let server = Server::start(vec![(
        "/zipball",
        Route::ok(good_zip("1.0.0", "acme-good-abc123/")),
    )]);
    let release = format!(
        r#"{{"tag_name":"v1.0.0","zipball_url":"{}/zipball","assets":[]}}"#,
        server.url()
    );
    server.route("/repos/acme/good/releases/tags/v1.0.0", Route::ok(release));
    let home = TempDir::new().unwrap();
    let out = Speedyspec::with_home(&[], home.path())
        .env("SPEEDYSPEC_GITHUB_API", server.url())
        .run(&["self", "plugin", "install", "acme/good@v1.0.0", "--yes"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let installed = plugins_dir(home.path()).join("speedyspec-fixture.good");
    assert_eq!(
        listing(&installed),
        vec!["plugin.php", "speedyspec-plugin.json"]
    );
}

#[test]
fn archive_without_manifest_is_refused() {
    let server = Server::start(vec![(
        "/p.zip",
        Route::ok(build_zip(&[("readme.md", Entry::file("no plugin here"))])),
    )]);
    let home = TempDir::new().unwrap();
    let url = format!("{}/p.zip", server.url());
    let out = speedyspec(home.path(), &["self", "plugin", "install", &url, "--yes"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("speedyspec-plugin.json"),
        "{}",
        out.stderr
    );
    assert_eq!(listing(&plugins_dir(home.path())), Vec::<String>::new());
}

#[test]
fn incompatible_contracts_are_refused() {
    let manifest =
        format!(r#"{{"name":"{GOOD}","version":"1.0.0","contracts":"^2.0","entry":"plugin.php"}}"#);
    let server = Server::start(vec![(
        "/p.zip",
        Route::ok(plugin_zip(&manifest, "<?php\n", "")),
    )]);
    let home = TempDir::new().unwrap();
    let url = format!("{}/p.zip", server.url());
    let out = speedyspec(home.path(), &["self", "plugin", "install", &url, "--yes"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("^2.0") && out.stderr.contains("bundles 1.2.0"),
        "{}",
        out.stderr
    );
    assert_eq!(listing(&plugins_dir(home.path())), Vec::<String>::new());
}

#[test]
fn replaces_an_installed_plugin_of_the_same_name() {
    let server = Server::start(vec![
        ("/v1.zip", Route::ok(good_zip("1.0.0", ""))),
        ("/v2.zip", Route::ok(good_zip("1.1.0", ""))),
    ]);
    let home = TempDir::new().unwrap();
    for version in ["v1", "v2"] {
        let url = format!("{}/{version}.zip", server.url());
        let out = speedyspec(home.path(), &["self", "plugin", "install", &url, "--yes"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
    }
    let out = speedyspec(home.path(), &["self", "plugin", "list"]);
    let lines: Vec<&str> = out.stdout.lines().collect();
    assert_eq!(lines.len(), 1, "{}", out.stdout);
    assert!(
        lines[0].starts_with(&format!("{GOOD}\t1.1.0\t")),
        "{}",
        out.stdout
    );
    assert_eq!(
        listing(&plugins_dir(home.path())),
        vec!["speedyspec-fixture.good"]
    );
}

#[test]
fn bad_new_archive_leaves_the_old_plugin() {
    let broken = format!(
        r#"{{"name":"{GOOD}","version":"2.0.0","contracts":"^1.0","entry":"missing.php"}}"#
    );
    let server = Server::start(vec![
        ("/v1.zip", Route::ok(good_zip("1.0.0", ""))),
        ("/bad.zip", Route::ok(plugin_zip(&broken, "<?php\n", ""))),
    ]);
    let home = TempDir::new().unwrap();
    let url = format!("{}/v1.zip", server.url());
    assert_eq!(
        speedyspec(home.path(), &["self", "plugin", "install", &url, "--yes"]).code,
        0
    );

    let url = format!("{}/bad.zip", server.url());
    let out = speedyspec(home.path(), &["self", "plugin", "install", &url, "--yes"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("missing.php"), "{}", out.stderr);

    let out = speedyspec(home.path(), &["self", "plugin", "list"]);
    assert!(
        out.stdout.starts_with(&format!("{GOOD}\t1.0.0\t")),
        "{}",
        out.stdout
    );
    assert_eq!(
        listing(&plugins_dir(home.path())),
        vec!["speedyspec-fixture.good"]
    );
}

#[test]
fn works_when_an_installed_plugin_is_broken() {
    let server = Server::start(vec![("/p.zip", Route::ok(good_zip("1.0.0", "")))]);
    let home = TempDir::new().unwrap();
    let broken = plugins_dir(home.path()).join("acme.broken");
    fs::create_dir_all(&broken).unwrap();
    fs::write(
        broken.join("speedyspec-plugin.json"),
        r#"{"name":"acme/broken","version":"1.0.0","contracts":"^1.0","entry":"plugin.php"}"#,
    )
    .unwrap();
    fs::write(
        broken.join("plugin.php"),
        "<?php\neval('function broken_twice() {} function broken_twice() {}');\n",
    )
    .unwrap();

    let url = format!("{}/p.zip", server.url());
    let out = speedyspec(home.path(), &["self", "plugin", "install", &url, "--yes"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.starts_with(&format!("{GOOD}\t1.0.0\t")),
        "{}",
        out.stdout
    );

    // And the broken one can be removed.
    let out = speedyspec(
        home.path(),
        &["self", "plugin", "remove", "acme/broken", "--yes"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        listing(&plugins_dir(home.path())),
        vec!["speedyspec-fixture.good"]
    );
}

#[test]
fn remove_deletes_the_plugin() {
    let server = Server::start(vec![("/p.zip", Route::ok(good_zip("1.0.0", "")))]);
    let home = TempDir::new().unwrap();
    let url = format!("{}/p.zip", server.url());
    assert_eq!(
        speedyspec(home.path(), &["self", "plugin", "install", &url, "--yes"]).code,
        0
    );

    let out = speedyspec(home.path(), &["self", "plugin", "remove", GOOD]);
    assert_eq!(out.code, 2, "removal needs confirming");
    assert!(
        out.stderr.contains("speedyspec: confirm with --yes"),
        "{}",
        out.stderr
    );

    let out = speedyspec(home.path(), &["self", "plugin", "remove", GOOD, "--yes"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let target = plugins_dir(home.path()).join("speedyspec-fixture.good");
    assert_eq!(out.stdout, format!("{GOOD}\t{}\n", target.display()));
    assert_eq!(
        speedyspec(home.path(), &["self", "plugin", "list"]).stdout,
        ""
    );
}

#[test]
fn remove_refuses_a_plugin_dir_plugin() {
    let home = TempDir::new().unwrap();
    let out = Speedyspec::with_home(&["good"], home.path())
        .run(&["self", "plugin", "remove", GOOD, "--yes"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .contains(&format!("speedyspec: {GOOD} is not installed in")),
        "{}",
        out.stderr
    );
    assert!(fixture("good").join("plugin.php").exists());
}

#[test]
fn no_user_plugin_directory_is_reported() {
    let server = Server::start(vec![("/p.zip", Route::ok(good_zip("1.0.0", "")))]);
    let cwd = TempDir::new().unwrap();
    let url = format!("{}/p.zip", server.url());
    let out = Speedyspec::new(&[])
        .env_remove("SPEEDYSPEC_HOME")
        .env_remove("XDG_DATA_HOME")
        .env("HOME", "rel")
        .cwd(cwd.path())
        .run(&["self", "plugin", "install", &url, "--yes"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .contains("there is no user plugin directory to install into"),
        "{}",
        out.stderr
    );
    assert_eq!(
        listing(cwd.path()),
        Vec::<String>::new(),
        "nothing may be written to the working directory"
    );
}

#[test]
fn bad_source_is_a_usage_error() {
    let home = TempDir::new().unwrap();
    let out = speedyspec(
        home.path(),
        &["self", "plugin", "install", "not a source", "--yes"],
    );
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("owner/repo"), "{}", out.stderr);
}

#[test]
fn remove_refuses_names_that_are_not_plugin_names() {
    let home = TempDir::new().unwrap();
    fs::create_dir_all(plugins_dir(home.path()).join("acme.one")).unwrap();
    fs::write(home.path().join("precious.txt"), "keep").unwrap();
    for name in ["..", ".", "", "acme", "../x"] {
        let out = Speedyspec::with_home(&[], home.path()).run(&[
            "--no-plugins",
            "self",
            "plugin",
            "remove",
            "--yes",
            "--",
            name,
        ]);
        assert_eq!(out.code, 2, "{name:?}: {}", out.stderr);
        assert!(
            out.stderr.contains("is not a plugin name"),
            "{name:?}: {}",
            out.stderr
        );
        assert_eq!(out.stdout, "");
    }
    assert_eq!(listing(home.path()), vec!["plugins", "precious.txt"]);
    assert_eq!(listing(&plugins_dir(home.path())), vec!["acme.one"]);
}
