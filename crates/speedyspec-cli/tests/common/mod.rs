//! Shared by the integration tests that drive the built binary.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/plugins")
        .join(name)
}

/// A `speedyspec` invocation with an empty `SPEEDYSPEC_HOME` unless one is given, no terminal on
/// stdin, and the named fixtures passed by `--plugin-dir`.
pub struct Speedyspec {
    command: Command,
    _home: Option<tempfile::TempDir>,
}

impl Speedyspec {
    pub fn new(plugins: &[&str]) -> Speedyspec {
        let home = tempfile::tempdir().expect("temp home");
        let mut run = Self::with_home(plugins, home.path());
        run._home = Some(home);
        run
    }

    /// As `new`, but with a `SPEEDYSPEC_HOME` the test keeps (to run several commands on it).
    pub fn with_home(plugins: &[&str], home: &Path) -> Speedyspec {
        let mut command = Command::new(env!("CARGO_BIN_EXE_speedyspec"));
        command
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .env("SPEEDYSPEC_HOME", home)
            .env_remove("GITHUB_TOKEN")
            .stdin(Stdio::null());
        for plugin in plugins {
            command.arg("--plugin-dir").arg(fixture(plugin));
        }
        Speedyspec {
            command,
            _home: None,
        }
    }

    pub fn env(mut self, name: &str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        self.command.env(name, value);
        self
    }

    pub fn env_remove(mut self, name: &str) -> Self {
        self.command.env_remove(name);
        self
    }

    pub fn cwd(mut self, dir: &Path) -> Self {
        self.command.current_dir(dir);
        self
    }

    pub fn run(mut self, args: &[&str]) -> Run {
        let output = self.command.args(args).output().expect("speedyspec runs");
        Run {
            code: output.status.code().expect("exited normally"),
            stdout: String::from_utf8(output.stdout).expect("stdout is UTF-8"),
            stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        }
    }
}

/// Every file under `dir`, as sorted relative paths with forward slashes.
pub fn files(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if std::fs::symlink_metadata(&path).unwrap().is_dir() {
                walk(&path, root, out);
            } else {
                let relative = path.strip_prefix(root).unwrap();
                out.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}
