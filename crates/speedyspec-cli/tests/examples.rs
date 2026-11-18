//! The example plugins shipped with the CLI load and read as the project's PHP style asks.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn example(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join(name)
}

#[test]
fn wordpress_example_loads_and_is_listed() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_speedyspec"))
        .env("SPEEDYSPEC_HOME", home.path())
        .stdin(Stdio::null())
        .arg("--plugin-dir")
        .arg(example("install-wordpress"))
        .args(["app", "list"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "wordpress\tWordPress\tzip\n"
    );
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn kubernetes_example_loads_and_is_listed() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_speedyspec"))
        .env("SPEEDYSPEC_HOME", home.path())
        .env("KUBECONFIG", "/nonexistent/speedyspec-test-kubeconfig")
        .stdin(Stdio::null())
        .arg("--plugin-dir")
        .arg(example("install-demo-kubernetes"))
        .args(["app", "list"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "demo\tDemo\tkubernetes\n"
    );
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn kubernetes_example_follows_the_php_style() {
    assert_php_style("install-demo-kubernetes");
}

#[test]
fn wordpress_example_follows_the_php_style() {
    assert_php_style("install-wordpress");
}

fn assert_php_style(name: &str) {
    let source = fs::read_to_string(example(name).join("plugin.php")).unwrap();
    assert!(
        source.starts_with("<?php\n\ndeclare(strict_types=1);\n"),
        "{source}"
    );
    assert!(
        source.ends_with("}\n") || source.ends_with(";\n"),
        "must end with one newline"
    );
    assert!(!source.ends_with("\n\n"), "no trailing blank lines");
    for (number, line) in source.lines().enumerate() {
        let number = number + 1;
        assert!(!line.contains('\t'), "line {number} has a tab");
        assert!(
            line.len() <= 120,
            "line {number} is longer than 120 characters"
        );
        assert_eq!(
            line.trim_end(),
            line,
            "line {number} has trailing whitespace"
        );
    }
}
