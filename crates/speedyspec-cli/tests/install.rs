//! `speedyspec app install` with the zip driver, against a loopback server.

mod common;

use std::fs;
use std::path::Path;

use common::{Run, Speedyspec, files};
use speedyspec_testkit::{Entry, Route, Server, build_zip};
use tempfile::TempDir;

/// The archives the fixture plugins point at.
fn server() -> Server {
    Server::start(vec![
        (
            "/web.zip",
            Route::ok(build_zip(&[
                ("site/", Entry::Dir),
                ("site/index.html", Entry::file("<html>")),
                ("site/app.js", Entry::file("js")),
            ])),
        ),
        (
            "/docs.zip",
            Route::ok(build_zip(&[("readme.md", Entry::file("docs"))])),
        ),
    ])
}

/// Runs `app install` with the fixtures, pointing them at `server`, non-interactively.
fn install(server: &Server, plugins: &[&str], base: &Path, args: &[&str]) -> Run {
    let mut all = vec!["--no-interaction", "app", "install"];
    all.extend_from_slice(args);
    all.push("--base");
    let base = base.to_str().unwrap();
    all.push(base);
    Speedyspec::new(plugins)
        .env("ZIP_FIXTURE_URL", server.url())
        .run(&all)
}

fn canonical(path: &Path) -> String {
    path.canonicalize().unwrap().to_string_lossy().into_owned()
}

fn no_staging_left(base: &Path) {
    for name in files(base) {
        assert!(!name.contains(".speedyspec-staging"), "left behind: {name}");
    }
    for entry in fs::read_dir(base).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(
            !name.starts_with(".speedyspec-staging"),
            "left behind: {name}"
        );
    }
}

#[test]
fn installs_default_parts() {
    let server = server();
    let base = TempDir::new().unwrap();
    let out = install(
        &server,
        &["zip-app"],
        base.path(),
        &["site", "--title", "Hi"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(files(base.path()), vec!["app.js", "index.html"]);
    assert_eq!(
        out.stdout,
        format!("before:site\nafter:Hi 1 0\n.\t{}\n", canonical(base.path()))
    );
}

#[test]
fn installs_selected_parts() {
    let server = server();
    let base = TempDir::new().unwrap();
    let args = ["site", "--part", "web", "--part", "docs", "--title", "Hi"];
    let out = install(&server, &["zip-app"], base.path(), &args);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        files(base.path()),
        vec!["app.js", "docs/readme.md", "index.html"]
    );
    let root = canonical(base.path());
    assert_eq!(
        out.stdout,
        format!("before:site\nafter:Hi 2 0\n.\t{root}\ndocs\t{root}/docs\n")
    );
}

#[test]
fn unknown_part_is_usage_error() {
    let server = server();
    let base = TempDir::new().unwrap();
    let out = install(
        &server,
        &["zip-app"],
        base.path(),
        &["site", "--part", "nope", "--title", "x"],
    );
    assert_eq!(out.code, 2);
    assert!(
        out.stderr.contains("web") && out.stderr.contains("docs"),
        "{}",
        out.stderr
    );
    assert_eq!(files(base.path()), Vec::<String>::new());
}

#[test]
fn missing_required_option_is_usage_error() {
    let server = server();
    let base = TempDir::new().unwrap();
    let out = install(&server, &["zip-app"], base.path(), &["site"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("--title"), "{}", out.stderr);
    assert_eq!(files(base.path()), Vec::<String>::new());
    assert_eq!(out.stdout, "");
}

#[test]
fn secret_option_is_passed_to_the_plugin() {
    let server = server();
    let base = TempDir::new().unwrap();
    let args = ["site", "--title", "Hi", "--token", "s3cret"];
    let out = install(&server, &["zip-app"], base.path(), &args);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("after:Hi 1 6"), "{}", out.stdout);
    assert!(!out.stdout.contains("s3cret") && !out.stderr.contains("s3cret"));
}

#[test]
fn refuses_non_empty_base_without_force() {
    let server = server();
    let base = TempDir::new().unwrap();
    fs::write(base.path().join("keep.txt"), "mine").unwrap();
    let out = install(
        &server,
        &["zip-app"],
        base.path(),
        &["site", "--title", "Hi"],
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains(&canonical(base.path())),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("--force"), "{}", out.stderr);
    assert_eq!(files(base.path()), vec!["keep.txt"]);
    assert_eq!(
        fs::read_to_string(base.path().join("keep.txt")).unwrap(),
        "mine"
    );
    no_staging_left(base.path());
}

#[test]
fn force_merges() {
    let server = server();
    let base = TempDir::new().unwrap();
    fs::write(base.path().join("keep.txt"), "mine").unwrap();
    let out = install(
        &server,
        &["zip-app"],
        base.path(),
        &["site", "--title", "Hi", "--force"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(files(base.path()), vec!["app.js", "index.html", "keep.txt"]);
}

#[test]
fn failed_download_installs_nothing() {
    let server = server();
    server.route("/web.zip", Route::status(500));
    let base = TempDir::new().unwrap();
    let out = install(
        &server,
        &["zip-app"],
        base.path(),
        &["site", "--title", "Hi"],
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("/web.zip") && out.stderr.contains("500"),
        "{}",
        out.stderr
    );
    assert_eq!(
        fs::read_dir(base.path()).unwrap().count(),
        0,
        "base must be untouched"
    );
    assert_eq!(out.stdout, "before:site\n", "the after-hook must not run");
}

#[test]
fn driver_is_chosen_when_only_one_is_usable() {
    let server = server();
    let base = TempDir::new().unwrap();
    // Lists zip and sftp; only zip is compiled into this binary.
    let out = install(&server, &["zip-two-drivers"], base.path(), &["multi"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(files(base.path()), vec!["readme.md"]);
}

#[test]
fn unusable_driver_is_reported() {
    let server = server();
    let base = TempDir::new().unwrap();
    let out = install(&server, &["zip-k8s-only"], base.path(), &["cluster"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("sftp"), "{}", out.stderr);
}

#[test]
fn given_unknown_driver_is_usage_error() {
    let server = server();
    let base = TempDir::new().unwrap();
    let out = install(
        &server,
        &["zip-app"],
        base.path(),
        &["site", "--driver", "ftp"],
    );
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("ftp"), "{}", out.stderr);
}

#[test]
fn bad_answer_refuses_the_install() {
    let server = server();
    let base = TempDir::new().unwrap();
    let plugins = ["zip-app", "zip-bad-answer"];
    let out = install(&server, &plugins, base.path(), &["site", "--title", "Hi"]);
    assert_eq!(out.code, 1);
    assert_eq!(
        fs::read_dir(base.path()).unwrap().count(),
        0,
        "base must be untouched"
    );
    assert!(
        out.stderr.contains(
            "speedyspec: speedyspec-fixture/zip-bad-answer: speedyspec_driver_zip: [1].url"
        ),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("speedyspec: nothing was installed"),
        "{}",
        out.stderr
    );
    assert!(!out.stderr.contains("fixture/zip-app"), "{}", out.stderr);
    assert!(!out.stdout.contains("after:"), "{}", out.stdout);
}

#[test]
fn clashing_option_refuses_the_install() {
    let server = server();
    let base = TempDir::new().unwrap();
    let plugins = ["zip-app", "zip-bad-option"];
    let out = install(&server, &plugins, base.path(), &["site", "--title", "Hi"]);
    assert_eq!(out.code, 1);
    assert_eq!(
        fs::read_dir(base.path()).unwrap().count(),
        0,
        "base must be untouched"
    );
    let who = "speedyspec: speedyspec-fixture/zip-bad-option: speedyspec_install_options:";
    assert!(
        out.stderr
            .contains(&format!("{who} [2].name: \"driver\" is a built-in flag")),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr
            .contains(&format!("{who} [3].name: duplicate name \"title\"")),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("speedyspec: nothing was installed"),
        "{}",
        out.stderr
    );
}

#[test]
fn before_hook_can_veto() {
    let server = server();
    let base = TempDir::new().unwrap();
    let plugins = ["zip-app", "zip-veto"];
    let out = install(&server, &plugins, base.path(), &["site", "--title", "Hi"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .contains("speedyspec: speedyspec-fixture/zip-veto: speedyspec_install_before:"),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("not allowed"), "{}", out.stderr);
    assert_eq!(fs::read_dir(base.path()).unwrap().count(), 0);
}

#[test]
fn after_hook_failure_keeps_the_install() {
    let server = server();
    let base = TempDir::new().unwrap();
    let plugins = ["zip-app", "zip-after-fails"];
    let out = install(&server, &plugins, base.path(), &["site", "--title", "Hi"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("installed, but"), "{}", out.stderr);
    assert!(out.stderr.contains("post step failed"), "{}", out.stderr);
    assert_eq!(files(base.path()), vec!["app.js", "index.html"]);
}

#[test]
fn nothing_to_install_is_an_error() {
    let server = server();
    let base = TempDir::new().unwrap();
    let out = install(&server, &["zip-silent"], base.path(), &["empty"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .contains("no plugin supplied anything to install"),
        "{}",
        out.stderr
    );
}

#[test]
fn unknown_app_lists_apps() {
    let server = server();
    let base = TempDir::new().unwrap();
    let out = install(&server, &["zip-app"], base.path(), &["nope"]);
    assert_eq!(out.code, 2);
    assert!(
        out.stderr.contains("nope") && out.stderr.contains("site"),
        "{}",
        out.stderr
    );
}

#[test]
fn no_terminal_means_no_prompt() {
    let server = server();
    let base = TempDir::new().unwrap();
    // No --no-interaction: stdin is not a terminal, so the CLI must not wait for an answer.
    let out = Speedyspec::new(&["zip-app"])
        .env("ZIP_FIXTURE_URL", server.url())
        .run(&[
            "app",
            "install",
            "site",
            "--base",
            base.path().to_str().unwrap(),
        ]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("--title"), "{}", out.stderr);
}

#[test]
fn refuses_to_install_on_a_poisoned_engine() {
    let server = server();
    let base = TempDir::new().unwrap();
    let plugins = ["zip-poison", "zip-app"];
    let out = install(&server, &plugins, base.path(), &["site", "--title", "Hi"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("nothing will be installed"),
        "{}",
        out.stderr
    );
    assert_eq!(fs::read_dir(base.path()).unwrap().count(), 0);
    assert_eq!(out.stdout, "");
}

#[test]
fn option_flags_need_a_known_driver() {
    let out =
        Speedyspec::new(&["zip-app"]).run(&["--no-interaction", "app", "install", "--title", "x"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("--title"), "{}", out.stderr);
    assert!(
        out.stderr
            .contains("speedyspec: install options can be given as flags once --driver is known"),
        "{}",
        out.stderr
    );
}

#[test]
fn app_list_marks_only_unimplemented_drivers() {
    let out = Speedyspec::new(&["zip-two-drivers"]).run(&["app", "list"]);
    assert_eq!(out.code, 0);
    assert_eq!(out.stdout, "multi\tMulti\tzip,sftp?\n");
}

#[test]
fn help_shows_driver_and_option_flags() {
    let out = Speedyspec::new(&["zip-app"]).run(&["app", "install", "site", "--help"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    for flag in [
        "--base", "--force", "--part", "--driver", "--title", "--token",
    ] {
        assert!(
            out.stdout.contains(flag),
            "{flag} missing from:\n{}",
            out.stdout
        );
    }
}

#[test]
fn base_defaults_to_the_working_directory() {
    let server = server();
    let cwd = TempDir::new().unwrap();
    let out = Speedyspec::new(&["zip-app"])
        .env("ZIP_FIXTURE_URL", server.url())
        .cwd(cwd.path())
        .run(&[
            "--no-interaction",
            "app",
            "install",
            "site",
            "--title",
            "Hi",
        ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(files(cwd.path()), vec!["app.js", "index.html"]);
}
