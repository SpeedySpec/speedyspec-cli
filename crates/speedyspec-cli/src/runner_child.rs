//! Running an app's own PHP: the child side, `self run-task --request <file> --result <file>`.
//! It boots PHP with the contracts and no CLI plugins (their functions and classes could clash
//! with the app's), runs the bootstrap in `runner_bootstrap.php`, and makes sure a result is
//! written even when PHP could not write one.

use std::path::Path;
use std::rc::Rc;

use speedyspec_hooks_memory::MemoryHooks;
use speedyspec_php::{BootOptions, Php};

use crate::boot::CONTRACTS;

const BOOTSTRAP: &str = include_str!("runner_bootstrap.php");

/// Writes `{"ok": false, "error": …}` unless the bootstrap already wrote a result.
fn fallback(result: &Path, error: &str) {
    let written = std::fs::metadata(result).is_ok_and(|m| m.len() > 0);
    if !written {
        let _ = std::fs::write(
            result,
            serde_json::json!({"ok": false, "error": error, "diagnostics": []}).to_string(),
        );
    }
}

pub fn run(request: &Path, result: &Path) -> i32 {
    let Some(dir) = request.parent() else {
        fallback(result, "the request has no directory");
        return 1;
    };
    let bootstrap = dir.join("bootstrap.php");
    if let Err(e) = std::fs::write(&bootstrap, BOOTSTRAP) {
        fallback(
            result,
            &format!("could not write the runner's bootstrap: {e}"),
        );
        return 1;
    }
    let php = match Php::boot(BootOptions {
        hooks: Rc::new(MemoryHooks::new()),
        preload: CONTRACTS,
        areas: crate::boot::native_areas(),
    }) {
        Ok(php) => php,
        Err(e) => {
            fallback(result, &format!("PHP did not start: {e}"));
            return 1;
        }
    };
    let ran = php.run_file(
        &bootstrap,
        &[
            request.to_string_lossy().into_owned(),
            result.to_string_lossy().into_owned(),
        ],
    );
    // Ending the request runs the bootstrap's shutdown function, which writes the result after
    // exit() or a fatal error.
    drop(php);
    match ran {
        Ok(_) => fallback(result, "the task ended without a result"),
        Err(e) => fallback(result, &e.to_string()),
    }
    0
}
