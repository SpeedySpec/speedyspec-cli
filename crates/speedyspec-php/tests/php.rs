//! Tests for the embedded PHP runtime.
//!
//! PHP is process-global and must stay on the thread that booted it, so this
//! file has its own `main()` (`harness = false`): it boots PHP once, runs every
//! case in order on the main thread, and exits 1 if any case failed.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use speedyspec_php::{Php, PhpError};

type Case = fn(&Php) -> Result<(), String>;

const CASES: &[(&str, Case)] = &[
    ("version", case_version),
    ("php_calls_rust", case_php_calls_rust),
    ("ini_not_read", case_ini_not_read),
];

fn main() -> ExitCode {
    let php = match Php::boot() {
        Ok(php) => php,
        Err(err) => {
            println!("FAILED: boot: {err}");
            return ExitCode::FAILURE;
        }
    };

    let mut failed = 0;
    for (name, case) in CASES {
        match case(&php) {
            Ok(()) => println!("ok: {name}"),
            Err(why) => {
                failed += 1;
                println!("FAILED: {name}: {why}");
            }
        }
    }

    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn case_version(php: &Php) -> Result<(), String> {
    let out = capture_stdout(|| php.eval("t", "echo PHP_VERSION;"))?;
    eq(out.as_str(), speedyspec_php::PHP_VERSION)
}

fn case_php_calls_rust(php: &Php) -> Result<(), String> {
    // fixtures/calls_rust.php: <?php echo speedyspec_proof_add(2, 40);
    let out = capture_stdout(|| php.run_file(&fixture("calls_rust.php"), &[]))?;
    eq(out.as_str(), "42")
}

fn case_ini_not_read(php: &Php) -> Result<(), String> {
    let out = capture_stdout(|| php.eval("t", "var_export(php_ini_loaded_file());"))?;
    eq(out.as_str(), "false")
}

fn eq(actual: &str, expected: &str) -> Result<(), String> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("expected {expected:?}, got {actual:?}"))
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Runs `f` with file descriptor 1 pointed at a temporary file and returns
/// what was written to it. PHP writes through the process's stdout, so this
/// sees exactly what a user of the CLI would see.
fn capture_stdout<T>(f: impl FnOnce() -> Result<T, PhpError>) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!("speedyspec-php-test-{}.out", std::process::id()));
    let mut file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    let _ = std::fs::remove_file(&path);

    std::io::stdout().flush().map_err(|e| e.to_string())?;
    // SAFETY: plain descriptor juggling on fds this function owns or restores.
    let saved = unsafe { libc::dup(1) };
    if saved < 0 || unsafe { libc::dup2(file.as_raw_fd(), 1) } < 0 {
        return Err("could not redirect stdout".to_owned());
    }

    let result = f();

    let _ = std::io::stdout().flush();
    unsafe {
        libc::dup2(saved, 1);
        libc::close(saved);
    }

    result.map_err(|e| e.to_string())?;

    let mut out = String::new();
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    file.read_to_string(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}
