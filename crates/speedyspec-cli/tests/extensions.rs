//! `speedyspec plugin` and `theme`: list, install, update and delete an install's plugin and theme files. Where
//! they live comes from the app's plugin (`speedyspec_app_extensions`), where to get them from source plugins
//! (`speedyspec_extension_sources`); the files are fetched, checked and swapped into place in Rust.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Run, Speedyspec};
use sha2::{Digest, Sha256};
use speedyspec_testkit::{Entry, Route, Server, build_zip};
use tempfile::TempDir;

/// A plugin or theme zip: one top-level directory `top` holding info.txt.
fn ext_zip(top: &str, name: &str, version: &str) -> Vec<u8> {
    build_zip(&[
        (&format!("{top}/"), Entry::Dir),
        (
            &format!("{top}/info.txt"),
            Entry::file(&format!("Name: {name}\nVersion: {version}\n")),
        ),
        (&format!("{top}/main.php"), Entry::file("<?php\n")),
    ])
}

struct Site {
    home: TempDir,
    base: TempDir,
    server: Server,
}

impl Site {
    fn new() -> Site {
        let server = Server::start(vec![(
            "/web.zip",
            Route::ok(build_zip(&[
                ("site/", Entry::Dir),
                ("site/index.html", Entry::file("<html>")),
            ])),
        )]);
        let site = Site {
            home: TempDir::new().unwrap(),
            base: TempDir::new().unwrap(),
            server,
        };
        let out = site.cli().run(&[
            "--no-interaction",
            "app",
            "install",
            "ext-site",
            "--name",
            "site",
            "--base",
            site.base.path().to_str().unwrap(),
        ]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        site
    }

    fn cli(&self) -> Speedyspec {
        Speedyspec::with_home(&["ext-app", "ext-source"], self.home.path())
            .env("ZIP_FIXTURE_URL", self.server.url())
            .env("EXT_SOURCE_URL", self.server.url())
    }

    fn serve(&self, kind: &str, slug: &str, version: &str, zip: Vec<u8>) {
        self.server
            .route(&format!("/{kind}/{slug}.{version}.zip"), Route::ok(zip));
    }

    fn run(&self, args: &[&str]) -> Run {
        let mut all = vec!["--no-interaction"];
        all.extend_from_slice(args);
        all.extend_from_slice(&["--install", "site"]);
        self.cli().run(&all)
    }

    fn dir(&self, kind: &str) -> PathBuf {
        self.base.path().join("ext").join(format!("{kind}s"))
    }

    fn info(&self, kind: &str, slug: &str) -> String {
        fs::read_to_string(self.dir(kind).join(slug).join("info.txt")).unwrap_or_default()
    }

    fn requests(&self) -> usize {
        self.server
            .requests()
            .iter()
            .filter(|r| r.path != "/web.zip")
            .count()
    }
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|it| {
            it.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn install_then_list() {
    let site = Site::new();
    site.serve(
        "plugin",
        "akismet",
        "2.0",
        ext_zip("akismet", "Akismet", "2.0"),
    );
    let out = site.run(&["plugin", "install", "akismet"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains("installed plugin akismet 2.0"),
        "{}",
        out.stdout
    );
    assert!(site.info("plugin", "akismet").contains("Version: 2.0"));
    fs::write(
        site.dir("plugin").join("hello.php"),
        "<?php\n// Name: Hello\n",
    )
    .unwrap();
    let out = site.run(&["plugin", "list"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "akismet\tAkismet\t2.0\nhello\tHello\t\n");
}

#[test]
fn install_a_given_version() {
    let site = Site::new();
    site.serve(
        "plugin",
        "akismet",
        "1.5",
        ext_zip("akismet", "Akismet", "1.5"),
    );
    let out = site.run(&["plugin", "install", "akismet@1.5"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(site.info("plugin", "akismet").contains("Version: 1.5"));
}

#[test]
fn install_refuses_an_existing_one() {
    let site = Site::new();
    site.serve(
        "plugin",
        "akismet",
        "2.0",
        ext_zip("akismet", "Akismet", "2.0"),
    );
    assert_eq!(site.run(&["plugin", "install", "akismet"]).code, 0);
    let out = site.run(&["plugin", "install", "akismet"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("the plugin akismet is already installed; use update"),
        "{}",
        out.stderr
    );
}

#[test]
fn install_renames_the_top_directory() {
    let site = Site::new();
    site.serve(
        "plugin",
        "akismet",
        "2.0",
        ext_zip("akismet-main", "Akismet", "2.0"),
    );
    let out = site.run(&["plugin", "install", "akismet"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(entries(&site.dir("plugin")), ["akismet"]);
}

#[test]
fn archives_that_do_not_match_are_refused_and_leave_nothing() {
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "two-tops",
            build_zip(&[
                ("a/", Entry::Dir),
                ("a/info.txt", Entry::file("Name: A")),
                ("b/", Entry::Dir),
                ("b/info.txt", Entry::file("Name: B")),
            ]),
            "one top-level directory",
        ),
        (
            "loose-file",
            build_zip(&[("info.txt", Entry::file("Name: A"))]),
            "one top-level directory",
        ),
        (
            "escapes",
            build_zip(&[("x/", Entry::Dir), ("x/../../evil.txt", Entry::file("no"))]),
            "refused",
        ),
        (
            "symlink",
            build_zip(&[
                ("x/", Entry::Dir),
                ("x/link", Entry::Symlink("/etc/passwd".into())),
            ]),
            "refused",
        ),
    ];
    let site = Site::new();
    for (slug, zip, problem) in cases {
        site.serve("plugin", slug, "2.0", zip);
        let out = site.run(&["plugin", "install", slug]);
        assert_eq!(out.code, 1, "{slug}: {}", out.stderr);
        assert!(out.stderr.contains(problem), "{slug}: {}", out.stderr);
        assert_eq!(entries(&site.dir("plugin")), Vec::<String>::new(), "{slug}");
    }
    assert!(!site.base.path().join("evil.txt").exists());
}

#[test]
fn slugs_that_are_paths_are_usage_errors() {
    let site = Site::new();
    fs::create_dir_all(site.dir("plugin").join("keep")).unwrap();
    for slug in ["../x", "a/b", ".", "..", "", "-flag", "a\\b", "akismet@"] {
        let out = site.run(&["plugin", "install", slug]);
        assert_eq!(out.code, 2, "{slug:?}: {}", out.stderr);
    }
    let out = site.run(&["plugin", "delete", "../../etc", "--yes"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert_eq!(site.requests(), 0);
    assert_eq!(entries(&site.dir("plugin")), ["keep"]);
}

#[test]
fn update_replaces_the_files() {
    let site = Site::new();
    site.serve(
        "plugin",
        "akismet",
        "1.0",
        ext_zip("akismet", "Akismet", "1.0"),
    );
    site.serve(
        "plugin",
        "akismet",
        "2.0",
        ext_zip("akismet", "Akismet", "2.0"),
    );
    assert_eq!(site.run(&["plugin", "install", "akismet@1.0"]).code, 0);
    let out = site.run(&["plugin", "update", "akismet"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains("updated plugin akismet 1.0 -> 2.0"),
        "{}",
        out.stdout
    );
    assert!(site.info("plugin", "akismet").contains("Version: 2.0"));
    assert_eq!(entries(&site.dir("plugin")), ["akismet"]);
}

#[test]
fn update_to_the_same_version_says_so() {
    let site = Site::new();
    site.serve(
        "plugin",
        "akismet",
        "2.0",
        ext_zip("akismet", "Akismet", "2.0"),
    );
    assert_eq!(site.run(&["plugin", "install", "akismet"]).code, 0);
    let out = site.run(&["plugin", "update", "akismet"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains("akismet is already at 2.0"),
        "{}",
        out.stdout
    );
}

#[test]
fn a_failed_update_keeps_the_old_version() {
    let site = Site::new();
    site.serve(
        "plugin",
        "akismet",
        "1.0",
        ext_zip("akismet", "Akismet", "1.0"),
    );
    assert_eq!(site.run(&["plugin", "install", "akismet@1.0"]).code, 0);
    // 2.0 is not served: a 404.
    let out = site.run(&["plugin", "update", "akismet"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("404"), "{}", out.stderr);
    // A checksum that does not match.
    site.serve(
        "plugin",
        "akismet",
        "2.0",
        ext_zip("akismet", "Akismet", "2.0"),
    );
    let out = site.cli().env("EXT_SHA256", "0".repeat(64)).run(&[
        "--no-interaction",
        "plugin",
        "update",
        "akismet",
        "--install",
        "site",
    ]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("checksum"), "{}", out.stderr);
    // An unsafe archive.
    site.serve(
        "plugin",
        "akismet",
        "3.0",
        build_zip(&[("x/", Entry::Dir), ("x/../../evil", Entry::file("no"))]),
    );
    let out = site.cli().env("EXT_LATEST", "3.0").run(&[
        "--no-interaction",
        "plugin",
        "update",
        "akismet",
        "--install",
        "site",
    ]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(site.info("plugin", "akismet").contains("Version: 1.0"));
    assert_eq!(entries(&site.dir("plugin")), ["akismet"]);
}

#[test]
fn a_matching_checksum_is_accepted() {
    let site = Site::new();
    let zip = ext_zip("akismet", "Akismet", "2.0");
    let sha: String = Sha256::digest(&zip)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    site.serve("plugin", "akismet", "2.0", zip);
    let out = site.cli().env("EXT_SHA256", &sha).run(&[
        "--no-interaction",
        "plugin",
        "install",
        "akismet",
        "--install",
        "site",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
}

#[test]
fn update_all_goes_on_after_a_failure() {
    let site = Site::new();
    for slug in ["alpha", "beta", "gamma"] {
        site.serve("plugin", slug, "1.0", ext_zip(slug, slug, "1.0"));
        assert_eq!(
            site.run(&["plugin", "install", &format!("{slug}@1.0")])
                .code,
            0
        );
    }
    site.serve("plugin", "alpha", "2.0", ext_zip("alpha", "alpha", "2.0"));
    site.serve("plugin", "gamma", "2.0", ext_zip("gamma", "gamma", "2.0"));
    fs::create_dir_all(site.dir("plugin").join("unknown-custom")).unwrap();
    fs::write(
        site.dir("plugin").join("unknown-custom/info.txt"),
        "Name: Custom\nVersion: 1\n",
    )
    .unwrap();
    let out = site.run(&["plugin", "update", "--all"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(site.info("plugin", "alpha").contains("2.0"));
    assert!(site.info("plugin", "beta").contains("1.0"));
    assert!(site.info("plugin", "gamma").contains("2.0"));
    assert!(out.stderr.contains("beta"), "{}", out.stderr);
    assert!(
        out.stderr
            .contains("skipped unknown-custom: no source has it"),
        "{}",
        out.stderr
    );
}

#[test]
fn delete_needs_yes_when_not_interactive() {
    let site = Site::new();
    site.serve(
        "plugin",
        "akismet",
        "2.0",
        ext_zip("akismet", "Akismet", "2.0"),
    );
    assert_eq!(site.run(&["plugin", "install", "akismet"]).code, 0);
    let out = site.run(&["plugin", "delete", "akismet"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(site.dir("plugin").join("akismet").is_dir());
    let out = site.run(&["plugin", "delete", "akismet", "--yes"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains("deleted plugin akismet"),
        "{}",
        out.stdout
    );
    assert!(!site.dir("plugin").join("akismet").exists());
}

#[test]
fn delete_removes_a_single_file_plugin() {
    let site = Site::new();
    fs::create_dir_all(site.dir("plugin")).unwrap();
    fs::write(
        site.dir("plugin").join("hello.php"),
        "<?php\n// Name: Hello\n",
    )
    .unwrap();
    let out = site.run(&["plugin", "delete", "hello", "--yes"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(!site.dir("plugin").join("hello.php").exists());
    let out = site.run(&["plugin", "delete", "hello", "--yes"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("no plugin hello"), "{}", out.stderr);
}

#[test]
fn no_layout_is_named() {
    let site = Site::new();
    let out = Speedyspec::with_home(&["ext-source"], site.home.path()).run(&[
        "--no-interaction",
        "plugin",
        "list",
        "--install",
        "site",
    ]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("no plugin says where ext-site's plugins and themes are"),
        "{}",
        out.stderr
    );
}

#[test]
fn a_kind_the_app_lacks_is_named() {
    let site = Site::new();
    let out = site.cli().env("EXT_ONLY_PLUGINS", "1").run(&[
        "--no-interaction",
        "theme",
        "list",
        "--install",
        "site",
    ]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("ext-site has no themes"),
        "{}",
        out.stderr
    );
}

#[test]
fn no_source_is_named() {
    let site = Site::new();
    let out = site.run(&["plugin", "install", "unknown-thing"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("no source has the plugin unknown-thing"),
        "{}",
        out.stderr
    );
    let out = site.run(&["plugin", "install", "unknown-thing@1.0"]);
    assert!(
        out.stderr
            .contains("no source has the plugin unknown-thing at version 1.0"),
        "{}",
        out.stderr
    );
}

#[test]
fn a_source_answering_another_slug_is_refused() {
    let site = Site::new();
    let out = site.run(&["plugin", "install", "liar"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("someone-else"), "{}", out.stderr);
    assert_eq!(site.requests(), 0);
}

#[test]
fn theme_commands_use_the_theme_directory() {
    let site = Site::new();
    site.serve(
        "theme",
        "twentytwentyfive",
        "2.0",
        ext_zip("twentytwentyfive", "Twenty Twenty-Five", "2.0"),
    );
    let out = site.run(&["theme", "install", "twentytwentyfive"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(site.dir("theme").join("twentytwentyfive").is_dir());
    let out = site.run(&["theme", "list"]);
    assert_eq!(out.stdout, "twentytwentyfive\tTwenty Twenty-Five\t2.0\n");
    assert_eq!(
        site.run(&["theme", "delete", "twentytwentyfive", "--yes"])
            .code,
        0
    );
    assert!(!site.dir("theme").join("twentytwentyfive").exists());
}

#[test]
fn theme_is_a_command_of_its_own() {
    let out = Speedyspec::new(&[]).run(&["theme", "--help"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("list"), "{}", out.stdout);
}

/// Only what the app's plugin recognizes is a plugin: WordPress's own index.php in the plugins directory is not.
#[test]
fn delete_and_update_leave_files_that_are_not_plugins() {
    let site = Site::new();
    fs::create_dir_all(site.dir("plugin")).unwrap();
    fs::write(
        site.dir("plugin").join("index.php"),
        "<?php // Silence is golden.\n",
    )
    .unwrap();
    let out = site.run(&["plugin", "delete", "index", "--yes"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("no plugin index"), "{}", out.stderr);
    assert!(site.dir("plugin").join("index.php").is_file());
    let out = site.run(&["plugin", "update", "index"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("no plugin index"), "{}", out.stderr);
    assert_eq!(site.requests(), 0);
}

#[test]
fn update_all_asks_each_source_once_per_plugin() {
    let site = Site::new();
    for slug in ["alpha", "beta"] {
        site.serve("plugin", slug, "1.0", ext_zip(slug, slug, "1.0"));
        site.serve("plugin", slug, "2.0", ext_zip(slug, slug, "2.0"));
        assert_eq!(
            site.run(&["plugin", "install", &format!("{slug}@1.0")])
                .code,
            0
        );
    }
    let log = site.home.path().join("resolved.log");
    let out = site.cli().env("EXT_RESOLVE_LOG", &log).run(&[
        "--no-interaction",
        "plugin",
        "update",
        "--all",
        "--install",
        "site",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(fs::read_to_string(&log).unwrap(), "alpha\nbeta\n");
}

#[test]
fn update_does_not_go_back_to_an_older_version() {
    let site = Site::new();
    site.serve(
        "plugin",
        "akismet",
        "2.0",
        ext_zip("akismet", "Akismet", "2.0"),
    );
    site.serve(
        "plugin",
        "akismet",
        "1.5",
        ext_zip("akismet", "Akismet", "1.5"),
    );
    assert_eq!(site.run(&["plugin", "install", "akismet"]).code, 0);
    let out = site.cli().env("EXT_LATEST", "1.5").run(&[
        "--no-interaction",
        "plugin",
        "update",
        "akismet",
        "--install",
        "site",
    ]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("older"), "{}", out.stderr);
    assert!(site.info("plugin", "akismet").contains("Version: 2.0"));
}

#[test]
fn update_all_skips_a_plugin_newer_than_its_source() {
    let site = Site::new();
    site.serve(
        "plugin",
        "akismet",
        "2.0",
        ext_zip("akismet", "Akismet", "2.0"),
    );
    assert_eq!(site.run(&["plugin", "install", "akismet"]).code, 0);
    let out = site.cli().env("EXT_LATEST", "1.5").run(&[
        "--no-interaction",
        "plugin",
        "update",
        "--all",
        "--install",
        "site",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr.contains("skipped akismet") && out.stderr.contains("newer"),
        "{}",
        out.stderr
    );
}

#[test]
fn the_same_version_written_differently_is_already_installed() {
    let site = Site::new();
    site.serve(
        "plugin",
        "akismet",
        "2.0",
        ext_zip("akismet", "Akismet", "2.0"),
    );
    assert_eq!(site.run(&["plugin", "install", "akismet"]).code, 0);
    let out = site.cli().env("EXT_LATEST", "2.0.0").run(&[
        "--no-interaction",
        "plugin",
        "update",
        "akismet",
        "--install",
        "site",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("already at"), "{}", out.stdout);
}

/// `args` on the site in maintenance mode, with the log of what each read of an entry saw.
fn in_maintenance(site: &Site, log: &Path, args: &[&str]) -> Run {
    let mut all = vec!["--no-interaction"];
    all.extend_from_slice(args);
    all.extend_from_slice(&["--install", "site"]);
    site.cli()
        .env("EXT_MAINTENANCE", "1")
        .env("EXT_MAINT_LOG", log)
        .run(&all)
}

#[test]
fn the_maintenance_file_is_there_during_an_update_and_gone_after() {
    let site = Site::new();
    site.serve(
        "plugin",
        "akismet",
        "1.0",
        ext_zip("akismet", "Akismet", "1.0"),
    );
    site.serve(
        "plugin",
        "akismet",
        "2.0",
        ext_zip("akismet", "Akismet", "2.0"),
    );
    assert_eq!(site.run(&["plugin", "install", "akismet@1.0"]).code, 0);
    let log = site.home.path().join("maintenance.log");
    let out = in_maintenance(&site, &log, &["plugin", "update", "akismet"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        fs::read_to_string(&log)
            .unwrap()
            .contains("maintenance: on\n"),
        "{:?}",
        fs::read_to_string(&log)
    );
    assert!(!site.base.path().join(".maintenance").exists());
    assert!(site.info("plugin", "akismet").contains("Version: 2.0"));
}

#[test]
fn it_is_removed_after_a_failed_update() {
    let site = Site::new();
    site.serve(
        "plugin",
        "akismet",
        "1.0",
        ext_zip("akismet", "Akismet", "1.0"),
    );
    assert_eq!(site.run(&["plugin", "install", "akismet@1.0"]).code, 0);
    // Two top-level directories: refused after it was downloaded, while in maintenance.
    site.serve(
        "plugin",
        "akismet",
        "2.0",
        build_zip(&[("a/", Entry::Dir), ("b/", Entry::Dir)]),
    );
    let log = site.home.path().join("maintenance.log");
    let out = in_maintenance(&site, &log, &["plugin", "update", "akismet"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        fs::read_to_string(&log)
            .unwrap()
            .contains("maintenance: on\n")
    );
    assert!(
        !site.base.path().join(".maintenance").exists(),
        "the site was left in maintenance"
    );
    assert!(site.info("plugin", "akismet").contains("Version: 1.0"));
}

#[test]
fn install_and_delete_do_not_put_the_site_in_maintenance() {
    let site = Site::new();
    site.serve(
        "plugin",
        "akismet",
        "1.0",
        ext_zip("akismet", "Akismet", "1.0"),
    );
    let log = site.home.path().join("maintenance.log");
    assert_eq!(
        in_maintenance(&site, &log, &["plugin", "install", "akismet@1.0"]).code,
        0
    );
    assert_eq!(
        in_maintenance(&site, &log, &["plugin", "delete", "akismet", "--yes"]).code,
        0
    );
    assert!(
        !fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("maintenance: on\n")
    );
}
