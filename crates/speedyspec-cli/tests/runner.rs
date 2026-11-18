//! The runner: an app's own PHP, run in a child process of the binary. Each test writes a tiny
//! fake app and a task into a temporary directory and drives the hidden `self task` command,
//! which runs the task through the runner and prints what came back as JSON.

mod common;

use std::fs;
use std::path::Path;

use common::{Run, Speedyspec};
use serde_json::Value as Json;
use tempfile::TempDir;

/// An app whose entry file defines `tiny_app_name()` and sets a global, as `wp-config.php` does.
fn app(entry: &str) -> TempDir {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("entry.php"), entry).unwrap();
    root
}

const ENTRY: &str = "<?php\n$tiny_global = 'set at the top level';\nfunction tiny_app_name(): string { return 'tiny'; }\n";

fn task(root: &Path, name: &str, body: &str) -> String {
    let file = root.join(name);
    fs::write(
        &file,
        format!("<?php\n\ndeclare(strict_types=1);\n\n{body}\n"),
    )
    .unwrap();
    file.to_string_lossy().into_owned()
}

fn run_task(root: &Path, file: &str, extra: &[&str]) -> Run {
    let mut args = vec![
        "self",
        "task",
        "--root",
        root.to_str().unwrap(),
        "--file",
        file,
        "--entry",
        "entry.php",
    ];
    args.extend_from_slice(extra);
    Speedyspec::new(&[]).run(&args)
}

fn outcome(out: &Run) -> Json {
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    serde_json::from_str(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", out.stdout))
}

#[test]
fn runs_a_task_inside_the_app() {
    let root = app(ENTRY);
    let file = task(
        root.path(),
        "task.php",
        "return static fn (array $input): array => ['name' => tiny_app_name(), 'global' => $GLOBALS['tiny_global'], 'got' => $input['x'], 'cwd' => getcwd()];",
    );
    let got = outcome(&run_task(root.path(), &file, &["--input", r#"{"x": 41}"#]));
    assert_eq!(got["result"]["name"], "tiny");
    assert_eq!(got["result"]["global"], "set at the top level");
    assert_eq!(got["result"]["got"], 41);
    assert_eq!(
        Path::new(got["result"]["cwd"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        root.path().canonicalize().unwrap()
    );
    assert_eq!(got["diagnostics"], serde_json::json!([]));
}

#[test]
fn task_without_the_app() {
    let root = app("<?php\nthrow new RuntimeException('the entry must not load');\n");
    let file = task(
        root.path(),
        "task.php",
        "return static fn (array $input): string => function_exists('tiny_app_name') ? 'loaded' : 'not loaded';",
    );
    let got = outcome(&run_task(root.path(), &file, &["--no-app"]));
    assert_eq!(got["result"], "not loaded");
}

#[test]
fn warnings_are_reported_with_file_and_line() {
    let root = app(ENTRY);
    let file = task(
        root.path(),
        "task.php",
        "return static function (array $input): int {\n    trigger_error('careful', E_USER_WARNING);\n    return 1;\n};",
    );
    let got = outcome(&run_task(root.path(), &file, &[]));
    assert_eq!(got["result"], 1);
    let diagnostic = &got["diagnostics"][0];
    assert_eq!(diagnostic["level"], "warning");
    assert_eq!(diagnostic["message"], "careful");
    assert!(
        diagnostic["file"].as_str().unwrap().ends_with("task.php"),
        "{diagnostic}"
    );
    assert_eq!(diagnostic["line"], 6);
}

#[test]
fn exit_in_the_app_is_reported() {
    let root = app("<?php\necho 'bye';\nexit(3);\n");
    let file = task(
        root.path(),
        "task.php",
        "return static fn (array $input): int => 1;",
    );
    let out = run_task(root.path(), &file, &[]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(
        out.stderr.contains("ended before it returned"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("bye"),
        "the app's output is shown: {}",
        out.stderr
    );
}

#[test]
fn fatal_error_is_reported() {
    let root = app(ENTRY);
    let file = task(
        root.path(),
        "task.php",
        "return static fn (array $input): int => undefined_function_xyz();",
    );
    let out = run_task(root.path(), &file, &[]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(
        out.stderr.contains("undefined_function_xyz"),
        "{}",
        out.stderr
    );
}

#[test]
fn uncaught_exception_is_reported() {
    let root = app(ENTRY);
    let file = task(
        root.path(),
        "task.php",
        "return static fn (array $input): int => throw new LogicException('nope');",
    );
    let out = run_task(root.path(), &file, &[]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(
        out.stderr.contains("LogicException: nope"),
        "{}",
        out.stderr
    );
}

#[test]
fn output_does_not_corrupt_the_result() {
    let root =
        app("<?php\necho \"{not json\\n\";\nfunction tiny_app_name(): string { return 'tiny'; }\n");
    let file = task(
        root.path(),
        "task.php",
        "return static function (array $input): string { echo 'more noise'; return tiny_app_name(); };",
    );
    let got = outcome(&run_task(root.path(), &file, &[]));
    assert_eq!(got["result"], "tiny");
    assert!(
        got["output"].as_str().unwrap().contains("{not json"),
        "{got}"
    );
}

#[test]
fn own_error_handler_takes_over_after_it_is_set() {
    let root = app(
        "<?php\ntrigger_error('before', E_USER_NOTICE);\nset_error_handler(static fn (): bool => true);\ntrigger_error('after', E_USER_NOTICE);\n",
    );
    let file = task(
        root.path(),
        "task.php",
        "return static fn (array $input): int => undefined_function_xyz();",
    );
    let out = run_task(root.path(), &file, &[]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(out.stderr.contains("before"), "{}", out.stderr);
    assert!(!out.stderr.contains("after"), "{}", out.stderr);
    assert!(
        out.stderr.contains("undefined_function_xyz"),
        "the fatal error is still seen: {}",
        out.stderr
    );
}

#[test]
fn slow_task_is_killed_at_its_timeout() {
    let root = app(ENTRY);
    let marker = root.path().join("pid");
    let file = task(
        root.path(),
        "task.php",
        &format!(
            "return static function (array $input): int {{ file_put_contents({:?}, (string) getmypid()); sleep(60); return 1; }};",
            marker.to_str().unwrap()
        ),
    );
    let started = std::time::Instant::now();
    let out = run_task(root.path(), &file, &["--timeout", "2"]);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(
        out.stderr.contains("did not finish within 2s"),
        "{}",
        out.stderr
    );
    let pid = fs::read_to_string(&marker).unwrap();
    let alive = std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .status()
        .unwrap()
        .success();
    assert!(!alive, "the child {pid} is still running");
}

fn is_alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// Waits briefly for a pid file a shell writes in the background.
fn read_pid(marker: &Path) -> String {
    for _ in 0..100 {
        if let Ok(pid) = fs::read_to_string(marker)
            && !pid.trim().is_empty()
        {
            return pid;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("{} was never written", marker.display());
}

#[test]
fn a_process_the_task_started_is_stopped_with_it_at_the_timeout() {
    let root = app(ENTRY);
    let marker = root.path().join("pid");
    let file = task(
        root.path(),
        "task.php",
        &format!(
            "return static function (array $input): string {{ return (string) shell_exec('sleep 30 & echo $! > {}; wait'); }};",
            marker.to_str().unwrap()
        ),
    );
    let started = std::time::Instant::now();
    let out = run_task(root.path(), &file, &["--timeout", "2"]);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert!(
        out.stderr.contains("did not finish within 2s"),
        "{}",
        out.stderr
    );
    let pid = read_pid(&marker);
    assert!(!is_alive(&pid), "the task's sleep {pid} is still running");
}

#[test]
fn a_background_process_does_not_outlive_a_finished_task() {
    let root = app(ENTRY);
    let marker = root.path().join("pid");
    // The sleep keeps the task's stderr open, so the CLI must not wait for it.
    let file = task(
        root.path(),
        "task.php",
        &format!(
            "return static function (array $input): string {{ shell_exec('sleep 30 > /dev/null & echo $! > {}'); return 'done'; }};",
            marker.to_str().unwrap()
        ),
    );
    let started = std::time::Instant::now();
    let out = run_task(root.path(), &file, &[]);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(outcome(&out)["result"], "done");
    let pid = read_pid(&marker);
    assert!(!is_alive(&pid), "the task's sleep {pid} is still running");
}

#[test]
fn diagnostics_so_far_are_kept_when_a_task_is_stopped() {
    let root = app(ENTRY);
    let file = task(
        root.path(),
        "task.php",
        "return static function (array $input): int { trigger_error('almost there', E_USER_WARNING); sleep(60); return 1; };",
    );
    let out = run_task(root.path(), &file, &["--timeout", "2"]);
    assert!(
        out.stderr.contains("did not finish within 2s"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("warning: almost there ("),
        "{}",
        out.stderr
    );
}

#[test]
fn interrupting_the_cli_stops_the_task_too() {
    for signal in ["INT", "TERM"] {
        let root = app(ENTRY);
        let marker = root.path().join("pid");
        let file = task(
            root.path(),
            "task.php",
            &format!(
                "return static function (array $input): string {{ return (string) shell_exec('sleep 30 & echo $! > {}; wait'); }};",
                marker.to_str().unwrap()
            ),
        );
        let (mut cli, _home) = Speedyspec::new(&[]).spawn(&[
            "self",
            "task",
            "--root",
            root.path().to_str().unwrap(),
            "--file",
            &file,
            "--entry",
            "entry.php",
        ]);
        let pid = read_pid(&marker);
        let sent = std::process::Command::new("kill")
            .args([&format!("-{signal}"), &cli.id().to_string()])
            .status()
            .unwrap();
        assert!(sent.success());
        let status = cli.wait().unwrap();
        assert!(!status.success(), "{signal}: {status:?}");
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            !is_alive(&pid),
            "{signal}: the task's sleep {pid} is still running"
        );
    }
}

#[test]
fn a_root_that_cannot_be_entered_fails_the_task() {
    let root = app(ENTRY);
    let file = task(
        root.path(),
        "task.php",
        "return static fn (array $input): string => getcwd();",
    );
    let missing = root.path().join("not-here");
    let mut args = vec![
        "self",
        "task",
        "--root",
        missing.to_str().unwrap(),
        "--file",
        &file,
    ];
    args.push("--no-app");
    let out = Speedyspec::new(&[]).run(&args);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(out.stderr.contains("not-here"), "{}", out.stderr);
}

#[test]
fn fatal_error_backtraces_leave_argument_values_out() {
    let root = app(ENTRY);
    let file = task(
        root.path(),
        "task.php",
        "function connect_with(string $password): string { trigger_error('refuses', E_USER_ERROR); return ''; }\n\
         return static fn (array $input): string => connect_with('hunter2-secret-pw');",
    );
    let out = run_task(root.path(), &file, &[]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(out.stderr.contains("connect_with("), "{}", out.stderr);
    assert!(
        !out.stderr.contains("hunter2") && !out.stdout.contains("hunter2"),
        "{}",
        out.stderr
    );
}

#[test]
fn request_and_result_files_are_removed() {
    let root = app(ENTRY);
    let tmp = TempDir::new().unwrap();
    let file = task(
        root.path(),
        "task.php",
        "return static fn (array $input): int => 1;",
    );
    let out = Speedyspec::new(&[]).env("TMPDIR", tmp.path()).run(&[
        "self",
        "task",
        "--root",
        root.path().to_str().unwrap(),
        "--file",
        &file,
        "--entry",
        "entry.php",
    ]);
    outcome(&out);
    let left: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn password_in_input_is_not_in_argv_or_env() {
    let root = app(ENTRY);
    let file = task(
        root.path(),
        "task.php",
        "return static function (array $input): array {
            $args = is_readable('/proc/self/cmdline') ? file_get_contents('/proc/self/cmdline') : shell_exec('ps -o args= -p ' . getmypid());
            return ['args' => $args, 'env' => getenv(), 'marker' => getenv('SPEEDYSPEC_TEST_MARKER')];
        };",
    );
    let secret = "pw-0f6c1a-never-in-argv";
    let out = Speedyspec::new(&[])
        .env("SPEEDYSPEC_TEST_MARKER", "inherited")
        .run(&[
            "self",
            "task",
            "--root",
            root.path().to_str().unwrap(),
            "--file",
            &file,
            "--entry",
            "entry.php",
            "--input",
            &format!(r#"{{"password": "{secret}"}}"#),
        ]);
    let got = outcome(&out);
    let args = got["result"]["args"].as_str().unwrap();
    assert!(!args.contains(secret), "{args}");
    assert!(!got["result"]["env"].to_string().contains(secret));
    assert_eq!(
        got["result"]["marker"], "inherited",
        "the environment is inherited"
    );
}
