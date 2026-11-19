//! The last phase of `app install`: the app's plugin answers `speedyspec_app_setup` with a `SetupPlan`, and its
//! tasks run in order through the runner, in the install's directory. The install record counts the tasks that
//! finished, so a failed setup is finished by running the same command again.

use std::path::Path;

use speedyspec_driver::{AppRuntime, Install, ObjectReader, SetupPlan, decode_setup_plan};
use speedyspec_hooks::Value;

use crate::app_target::{Target, ask_with};
use crate::boot::{PhpReader, Runtime};

const HOOK_SETUP: &str = "speedyspec_app_setup";

/// Why setup stopped, and how many tasks finished before it did.
#[derive(Debug)]
pub struct SetupFailure {
    pub done: usize,
    pub message: String,
}

/// The install's setup plan, if its plugin has one. `request` is the `InstallRequest` the install was made with.
pub fn plan(
    runtime: &Runtime,
    install: &Install,
    request: &Value,
) -> Result<Option<SetupPlan>, String> {
    let check =
        |value: &Value, reader: &dyn ObjectReader| decode_setup_plan(value, reader).map(drop);
    let answer = ask_with(
        runtime,
        install,
        std::slice::from_ref(request),
        HOOK_SETUP,
        Value::Null,
        check,
    )?;
    if matches!(answer, Value::Null) {
        return Ok(None);
    }
    decode_setup_plan(&answer, &PhpReader(&runtime.php))
        .map(Some)
        .map_err(|e| e.to_string())
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Runs the plan's tasks from index `from` on, in the install root, saying on stderr which one runs. A task whose
/// result is a string has it shown as progress.
pub fn run(
    runtime: &Runtime,
    install: &Install,
    plan: &SetupPlan,
    from: usize,
) -> Result<(), SetupFailure> {
    let total = plan.tasks.len();
    let root = install.root.as_deref().ok_or_else(|| SetupFailure {
        done: from,
        message: format!("install {} is not on this machine", install.name),
    })?;
    let target = Target {
        install: install.clone(),
        cluster: None,
    };
    for (index, task) in plan.tasks.iter().enumerate().skip(from) {
        let name = file_name(&task.file);
        eprintln!("setup: {name} ({}/{total})", index + 1);
        let fail = |message: String| SetupFailure {
            done: index,
            message: format!("setup stopped at {name}: {message}"),
        };
        let app = if task.loads_app {
            crate::app_target::runtime(runtime, &target).map_err(fail)?
        } else {
            AppRuntime::default()
        };
        let outcome = crate::runner::run(root, &app, task).map_err(|e| fail(e.0))?;
        if let Some(text) = outcome.result.as_str() {
            eprintln!("setup: {text}");
        }
    }
    Ok(())
}
