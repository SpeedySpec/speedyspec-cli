//! Drives the built `speedyspec` binary. Every run gets an empty `SPEEDYSPEC_HOME`, so the
//! developer's own plugins never load, and plugins come from the fixtures by `--plugin-dir`.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/plugins")
        .join(name)
}

/// Runs `speedyspec` in `cwd` with the fixtures named in `plugins` passed by `--plugin-dir`, in
/// order, ahead of `args`.
fn run_in(cwd: &Path, plugins: &[&str], args: &[&str]) -> Run {
    let home = tempfile::tempdir().expect("temp home");
    let mut command = Command::new(env!("CARGO_BIN_EXE_speedyspec"));
    command.current_dir(cwd).env("SPEEDYSPEC_HOME", home.path());
    for plugin in plugins {
        command.arg("--plugin-dir").arg(fixture(plugin));
    }
    let output = command.args(args).output().expect("speedyspec runs");
    Run {
        code: output.status.code().expect("exited normally"),
        stdout: String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
    }
}

fn run(plugins: &[&str], args: &[&str]) -> Run {
    run_in(Path::new(env!("CARGO_MANIFEST_DIR")), plugins, args)
}

#[test]
fn version_prints_three_lines() {
    let out = run(&[], &["--version"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let lines: Vec<&str> = out.stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{:?}", out.stdout);
    assert!(lines[0].starts_with("speedyspec "), "{}", lines[0]);
    assert!(lines[1].starts_with("php 8."), "{}", lines[1]);
    assert_eq!(lines[2], "contracts 1.0.0");
    assert_eq!(out.stderr, "");
}

#[test]
fn runs_with_no_plugins_at_all() {
    let out = run(&[], &["app", "list"]);
    assert_eq!(out.code, 0);
    assert_eq!(out.stdout, "");
    assert!(out.stderr.contains("No apps"), "{}", out.stderr);
}

#[test]
fn php_command_runs_a_file_with_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("hooks.php");
    std::fs::write(
        &script,
        "<?php\nadd_filter('answer', fn (int $v): int => $v + 2);\necho apply_filters('answer', 40);\n",
    )
    .unwrap();
    let out = run(&[], &["php", script.to_str().unwrap()]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "42");
    assert_eq!(out.stderr, "");
}

#[test]
fn php_command_passes_arguments_through() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("argv.php");
    std::fs::write(
        &script,
        "<?php\necho implode(',', array_slice($argv, 1));\n",
    )
    .unwrap();
    let out = run(
        &[],
        &[
            "php",
            script.to_str().unwrap(),
            "--flag",
            "x",
            "--no-plugins",
        ],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "--flag,x,--no-plugins");
}

#[test]
fn php_command_exit_code_is_scripts() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("exit.php");
    std::fs::write(&script, "<?php\nexit(4);\n").unwrap();
    let out = run(&[], &["php", script.to_str().unwrap()]);
    assert_eq!(out.code, 4);
}

#[test]
fn php_command_missing_file_fails_with_one_line() {
    let out = run(&[], &["php", "/no/such/file.php"]);
    assert_eq!(out.code, 1);
    assert_eq!(out.stdout, "");
    assert!(out.stderr.starts_with("speedyspec: "), "{}", out.stderr);
    assert_eq!(out.stderr.lines().count(), 1, "{}", out.stderr);
}

#[test]
fn plugin_app_is_listed() {
    let out = run(&["good"], &["app", "list"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "demo\tDemo\tzip?\n");
}

#[test]
fn plugin_command_runs() {
    let out = run(&["good"], &["hello", "Ada"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "hello Ada\n");
    let out = run(&["good"], &["hello", "Ada", "--shout"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "HELLO ADA\n");
}

#[test]
fn plugin_command_appears_in_help() {
    let out = run(&["good"], &["--help"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("hello"), "{}", out.stdout);
    assert!(out.stdout.contains("--plugin-dir"), "{}", out.stdout);
}

#[test]
fn plugin_command_has_its_own_help() {
    let out = run(&["good"], &["hello", "--help"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("Greets someone."), "{}", out.stdout);
    assert!(out.stdout.contains("--shout"), "{}", out.stdout);
}

#[test]
fn missing_required_argument_is_usage_error() {
    let out = run(&["good"], &["hello"]);
    assert_eq!(out.code, 2);
    assert_eq!(out.stdout, "");
}

#[test]
fn plugin_dir_accepts_equals_form_and_comes_anywhere() {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_speedyspec"))
        .env("SPEEDYSPEC_HOME", home.path())
        .arg("hello")
        .arg(format!("--plugin-dir={}", fixture("good").display()))
        .arg("Ada")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello Ada\n");
}

#[test]
fn plugin_list_shows_loaded() {
    let out = run(&["good"], &["plugin", "list"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains("speedyspec-fixture/good\t1.0.0\t"),
        "{}",
        out.stdout
    );
}

#[test]
fn no_plugins_flag_skips_loading() {
    let out = run(&["good"], &["--no-plugins", "hello", "Ada"]);
    assert_eq!(out.code, 2);
    assert_eq!(out.stdout, "");
}

#[test]
fn load_output_goes_to_stderr() {
    let out = run(&["noisy"], &["--version"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout.lines().count(), 3, "{:?}", out.stdout);
    assert!(!out.stdout.contains("loading noisy"), "{}", out.stdout);
    assert!(out.stderr.contains("loading noisy"), "{}", out.stderr);
}

#[test]
fn exit_at_load_skips_and_names_plugin() {
    let out = run(&["exits", "good"], &["hello", "Ada"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "hello Ada\n");
    assert!(
        out.stderr
            .contains("speedyspec: plugin speedyspec-fixture/exits skipped"),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("exit(7)"), "{}", out.stderr);
}

#[test]
fn fatal_at_load_skips_and_names_plugin() {
    let out = run(&["fatal", "good"], &["hello", "Ada"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "hello Ada\n");
    assert!(
        out.stderr
            .contains("speedyspec: plugin speedyspec-fixture/fatal skipped"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("undefined_function_xyz"),
        "{}",
        out.stderr
    );
}

#[test]
fn contract_range_mismatch_is_reported() {
    let out = run(&["needs-v2"], &["--version"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stderr.contains("^2.0"), "{}", out.stderr);
    assert!(out.stderr.contains("1.0.0"), "{}", out.stderr);
    assert!(!out.stderr.contains("must never load"), "{}", out.stderr);
    assert_eq!(out.stdout.lines().count(), 3, "{:?}", out.stdout);
}

#[test]
fn wrong_filter_return_is_reported() {
    let out = run(&["bad-apps"], &["--version"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr.contains("speedyspec-fixture/bad-apps"),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("speedyspec_apps"), "{}", out.stderr);
    assert_eq!(out.stdout.lines().count(), 3, "{:?}", out.stdout);
}

#[test]
fn bad_field_names_plugin_hook_and_field() {
    let out = run(&["bad-app-field"], &["--version"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr.contains("speedyspec-fixture/bad-app-field"),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("speedyspec_apps"), "{}", out.stderr);
    assert!(out.stderr.contains("[0].id"), "{}", out.stderr);
}

#[test]
fn builtin_wins_over_plugin_command() {
    let out = run(&["clash"], &["app", "list"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "");
    assert!(out.stderr.contains("No apps"), "{}", out.stderr);
    assert!(
        out.stderr.contains(
            r#"speedyspec: speedyspec-fixture/clash: speedyspec_commands: [0].name: "app" is a built-in command"#
        ),
        "{}",
        out.stderr
    );
}

#[test]
fn first_plugin_wins_duplicate_command() {
    let out = run(&["good", "clash"], &["hello", "Ada"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "hello Ada\n");
    assert!(
        out.stderr.contains(
            r#"speedyspec: speedyspec-fixture/clash: speedyspec_commands: [2].name: duplicate name "hello" (the first one wins)"#
        ),
        "{}",
        out.stderr
    );
    assert!(!out.stderr.contains("fixture/good"), "{}", out.stderr);
}

/// Every fixture plugin directory, in name order.
fn all_fixtures() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixture(""))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// The lines of `stderr` that report a contract problem on `hook`.
fn contract_lines<'a>(stderr: &'a str, hook: &str) -> Vec<&'a str> {
    stderr
        .lines()
        .filter(|line| line.starts_with("speedyspec: ") && line.contains(&format!(": {hook}: ")))
        .collect()
}

#[test]
fn contract_errors_name_exactly_the_plugin_with_all_fixtures_loaded() {
    let names = all_fixtures();
    let plugins: Vec<&str> = names.iter().map(String::as_str).collect();
    let out = run(&plugins, &["app", "list"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let apps = contract_lines(&out.stderr, "speedyspec_apps");
    for line in &apps {
        assert!(!line.contains("fixture/good"), "{}", out.stderr);
        assert!(!line.contains("one of"), "{}", out.stderr);
    }
    assert!(
        apps.iter().any(|line| line.starts_with(
            "speedyspec: speedyspec-fixture/bad-apps: speedyspec_apps: expected a list"
        )),
        "{}",
        out.stderr
    );
    // The good plugins' apps survive the bad ones.
    assert!(out.stdout.contains("demo\tDemo\tzip?\n"), "{}", out.stdout);
    assert!(
        out.stdout.contains("quiet\tQuiet\tzip?\n"),
        "{}",
        out.stdout
    );
}

#[test]
fn one_bad_app_does_not_hide_the_others() {
    for plugins in [["bad-app-field", "good"], ["good", "bad-app-field"]] {
        let out = run(&plugins, &["app", "list"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert_eq!(out.stdout, "demo\tDemo\tzip?\n", "{}", out.stderr);
        let apps = contract_lines(&out.stderr, "speedyspec_apps");
        assert_eq!(apps.len(), 1, "{}", out.stderr);
        assert!(
            apps[0].starts_with("speedyspec: speedyspec-fixture/bad-app-field: speedyspec_apps: ["),
            "{}",
            apps[0]
        );
        assert!(apps[0].contains("].id: must match"), "{}", apps[0]);
    }
}

#[test]
fn callback_added_from_init_is_attributed_to_its_plugin() {
    let out = run(&["good", "late-bad"], &["app", "list"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "demo\tDemo\tzip?\n");
    assert_eq!(
        contract_lines(&out.stderr, "speedyspec_apps"),
        [
            "speedyspec: speedyspec-fixture/late-bad: speedyspec_apps: expected a list (an array keyed 0, 1, 2, ...)"
        ],
        "{}",
        out.stderr
    );
}

#[test]
fn exit_in_handler_is_exit_code() {
    let out = run(&["exit-handler"], &["quit"]);
    assert_eq!(out.code, 5);
    assert_eq!(out.stdout, "");
}

#[test]
fn exception_in_handler_is_reported() {
    let out = run(&["throws"], &["boom"]);
    assert_eq!(out.code, 1);
    assert_eq!(out.stdout, "");
    assert!(
        out.stderr
            .contains("speedyspec: boom: RuntimeException: kaboom"),
        "{}",
        out.stderr
    );
}

#[test]
fn unknown_command_is_usage_error() {
    let out = run(&[], &["nope"]);
    assert_eq!(out.code, 2);
    assert_eq!(out.stdout, "");
}

#[test]
fn no_arguments_prints_help_and_is_usage_error() {
    let out = run(&["good"], &[]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("Usage"), "{}", out.stderr);
    assert!(out.stderr.contains("hello"), "{}", out.stderr);
}

#[test]
fn working_directory_plugins_are_not_loaded() {
    let cwd = tempfile::tempdir().unwrap();
    let plugin = cwd.path().join(".speedyspec/plugins/good");
    std::fs::create_dir_all(&plugin).unwrap();
    for file in ["speedyspec-plugin.json", "plugin.php"] {
        std::fs::copy(fixture("good").join(file), plugin.join(file)).unwrap();
    }
    let out = run_in(cwd.path(), &[], &["hello", "Ada"]);
    assert_eq!(out.code, 2);
}

/// Memory exhaustion while loading is a fatal error of that plugin. PHP unwinds it like
/// `exit(255)`, but it must not be reported as one, nor leave the engine refusing every later
/// call, which would take the healthy plugin's command and app down with it.
#[test]
fn memory_exhaustion_at_load_is_survived() {
    let out = run(&["oom", "good"], &["hello", "Ada"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "hello Ada\n");
    let skipped: Vec<&str> = out
        .stderr
        .lines()
        .filter(|line| line.starts_with("speedyspec: plugin speedyspec-fixture/oom skipped"))
        .collect();
    assert_eq!(skipped.len(), 1, "{}", out.stderr);
    assert!(skipped[0].contains("Allowed memory size"), "{}", skipped[0]);
    assert!(!out.stderr.contains("exit(255)"), "{}", out.stderr);
    assert!(!out.stderr.contains("refused"), "{}", out.stderr);
    assert!(
        !out.stderr.contains("speedyspec-fixture/good"),
        "{}",
        out.stderr
    );

    let out = run(&["oom", "good"], &["app", "list"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "demo\tDemo\tzip?\n");
}

#[test]
fn memory_exhaustion_in_php_script_exits_1() {
    let out = run(
        &[],
        &["php", fixture("oom").join("plugin.php").to_str().unwrap()],
    );
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert_eq!(out.stdout, "");
    assert!(out.stderr.contains("Allowed memory size"), "{}", out.stderr);
}

/// An exception is not a fatal error; a redeclared function is, and bails PHP out. The binary
/// must report the plugin, keep stdout clean and carry on with the other plugins.
#[test]
fn real_fatal_error_at_load_is_survived() {
    let dir = tempfile::tempdir().unwrap();
    let plugin = dir.path().join("redeclares");
    std::fs::create_dir(&plugin).unwrap();
    std::fs::write(
        plugin.join("speedyspec-plugin.json"),
        r#"{"name":"speedyspec-fixture/redeclares","version":"1.0.0","contracts":"^1.0","entry":"plugin.php"}"#,
    )
    .unwrap();
    std::fs::write(
        plugin.join("plugin.php"),
        "<?php\nfunction twice() {}\nfunction twice() {}\n",
    )
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_speedyspec"))
        .env("SPEEDYSPEC_HOME", home.path())
        .arg("--plugin-dir")
        .arg(&plugin)
        .arg("--plugin-dir")
        .arg(fixture("good"))
        .args(["hello", "Ada"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hello Ada\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("plugin speedyspec-fixture/redeclares skipped"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Cannot redeclare function twice()"),
        "{stderr}"
    );
}

#[test]
fn start_up_hook_output_goes_to_stderr() {
    let out = run(&["noisy-hooks"], &["app", "list"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "quiet\tQuiet\tzip?\n");
    for noise in ["noise from init", "noise from commands", "noise from apps"] {
        assert!(out.stderr.contains(noise), "{}", out.stderr);
    }
    let out = run(&["noisy-hooks"], &["--version"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout.lines().count(), 3, "{:?}", out.stdout);
    assert!(!out.stdout.contains("noise"), "{}", out.stdout);
}

#[test]
fn fatal_in_handler_is_reported_once_and_keeps_stdout_clean() {
    let out = run(&["fatal-handler"], &["crash"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert_eq!(out.stdout, "");
    let ours: Vec<&str> = out
        .stderr
        .lines()
        .filter(|line| line.starts_with("speedyspec: crash:"))
        .collect();
    assert_eq!(ours.len(), 1, "{}", out.stderr);
    assert!(
        ours[0].contains("Cannot redeclare function declared_twice()"),
        "{}",
        ours[0]
    );
}

#[test]
fn warnings_in_handler_go_to_stderr() {
    let out = run(&["warns"], &["grumble"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "done");
    assert!(out.stderr.contains("careful now"), "{}", out.stderr);
    assert!(out.stderr.contains("old way"), "{}", out.stderr);
}

#[test]
fn fatal_in_php_script_keeps_stdout_clean() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("fatal.php");
    std::fs::write(&script, "<?php\nfunction twice() {}\nfunction twice() {}\n").unwrap();
    let out = run(&[], &["php", script.to_str().unwrap()]);
    // The php built-in reports any failure of the script as exit 1.
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert_eq!(out.stdout, "");
    assert!(
        out.stderr.contains("Cannot redeclare function twice()"),
        "{}",
        out.stderr
    );
}
