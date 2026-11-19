//! An install, and what its app's plugin says about it: how to run the app
//! (`speedyspec_app_runtime`) and where its database is (`speedyspec_app_database`). Both
//! filters start from null and are called with the `Install` contract; each answer is checked
//! as it comes back and a bad one is blamed on its plugin.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use speedyspec_driver::{
    AppRuntime, ContractError, DatabaseConnection, DatabaseOrTask, Install, ObjectReader,
    database_connection_from_json, decode_app_runtime, decode_database_or_task,
};
use speedyspec_hooks::{Hooks, Value};
use speedyspec_php::Output;

use crate::attribution::{Checked, Validator};
use crate::boot::{PhpReader, Runtime, report_hook_error};
use crate::installs;

const HOOK_RUNTIME: &str = "speedyspec_app_runtime";
const HOOK_DATABASE: &str = "speedyspec_app_database";
const INSTALL: &str = "SpeedySpec\\Cli\\Contracts\\Install";

/// An install found by name.
pub struct Target {
    pub install: Install,
}

impl Target {
    pub fn find(name: &str) -> Result<Target, String> {
        let record = installs::read(name)?;
        if let Some(problem) = installs::gone(&record) {
            return Err(problem);
        }
        let root = (record.driver == "zip").then(|| PathBuf::from(&record.location));
        let install = Install {
            name: record.name.clone(),
            app: record.app.clone(),
            parts: record.parts.clone(),
            driver: record.driver.clone(),
            root,
        };
        Ok(Target { install })
    }

    fn root(&self) -> Result<&Path, String> {
        self.install.root.as_deref().ok_or_else(|| {
            format!(
                "install {} is not on this machine; its files are where the {} driver put them",
                self.install.name, self.install.driver
            )
        })
    }
}

pub(crate) fn install_value(runtime: &Runtime, install: &Install) -> Result<Value, String> {
    let root = install.root.as_ref().map_or(Value::Null, |root| {
        Value::Str(root.to_string_lossy().into_owned())
    });
    runtime
        .php
        .construct(
            INSTALL,
            &[
                Value::Str(install.name.clone()),
                Value::Str(install.app.clone()),
                crate::app::string_list(&install.parts),
                Value::Str(install.driver.clone()),
                root,
            ],
        )
        .map_err(|e| format!("could not describe the install to plugins: {e}"))
}

/// Filters `hook` from null with the install, checking each answer with `check`.
fn ask(
    runtime: &Runtime,
    target: &Target,
    hook: &str,
    check: fn(&Value, &dyn ObjectReader) -> Result<(), ContractError>,
) -> Result<Value, String> {
    ask_with(runtime, &target.install, &[], hook, Value::Null, check)
}

/// Filters `hook` from `seed` with the install and then `extra`, checking each answer with `check` and blaming a
/// bad one on its plugin. A null answer is never checked.
pub(crate) fn ask_with(
    runtime: &Runtime,
    install: &Install,
    extra: &[Value],
    hook: &str,
    seed: Value,
    check: fn(&Value, &dyn ObjectReader) -> Result<(), ContractError>,
) -> Result<Value, String> {
    let install = install_value(runtime, install)?;
    let mut args = vec![install];
    args.extend_from_slice(extra);
    let php = Rc::clone(&runtime.php);
    let validator: Rc<Validator> = Rc::new(move |given, returned| {
        if matches!(returned, Value::Null) {
            return Checked {
                value: returned,
                problems: Vec::new(),
            };
        }
        match check(&returned, &PhpReader(&php)) {
            Ok(()) => Checked {
                value: returned,
                problems: Vec::new(),
            },
            Err(error) => Checked {
                value: given.clone(),
                problems: vec![error.to_string()],
            },
        }
    });
    runtime.php.redirect_output(Output::Stderr);
    runtime.hooks.validate(hook, validator);
    let answer = runtime.hooks.filter(hook, seed, &args);
    runtime.hooks.clear_validators();
    runtime.php.redirect_output(Output::Stdout);
    let rejections = runtime.hooks.take_rejections();
    if !rejections.is_empty() {
        return Err(rejections
            .iter()
            .map(|r| {
                format!(
                    "{}: {}: {}",
                    r.owner.as_deref().unwrap_or("a plugin"),
                    r.hook,
                    r.problem
                )
            })
            .collect::<Vec<_>>()
            .join("\nspeedyspec: "));
    }
    answer.map_err(|error| {
        report_hook_error(&error);
        format!("{hook} failed")
    })
}

/// How to load the install's app; nothing to load when no plugin says.
pub fn runtime(runtime: &Runtime, target: &Target) -> Result<AppRuntime, String> {
    let check =
        |value: &Value, reader: &dyn ObjectReader| decode_app_runtime(value, reader).map(drop);
    let answer = ask(runtime, target, HOOK_RUNTIME, check)?;
    if matches!(answer, Value::Null) {
        return Ok(AppRuntime::default());
    }
    decode_app_runtime(&answer, &PhpReader(&runtime.php)).map_err(|e| e.to_string())
}

/// Where the install's database is: a plugin's answer, or the result of the task it answered
/// with.
pub fn database(runtime: &Runtime, target: &Target) -> Result<DatabaseConnection, String> {
    let check =
        |value: &Value, reader: &dyn ObjectReader| decode_database_or_task(value, reader).map(drop);
    let answer = ask(runtime, target, HOOK_DATABASE, check)?;
    if matches!(answer, Value::Null) {
        return Err(format!(
            "no plugin says where the database of {} is; install the plugin for that app",
            target.install.app
        ));
    }
    match decode_database_or_task(&answer, &PhpReader(&runtime.php)).map_err(|e| e.to_string())? {
        DatabaseOrTask::Connection(connection) => Ok(connection),
        DatabaseOrTask::Task(task) => {
            let app = if task.loads_app {
                self::runtime(runtime, target)?
            } else {
                AppRuntime::default()
            };
            let outcome = crate::runner::run(target.root()?, &app, &task).map_err(|e| e.0)?;
            database_connection_from_json(&outcome.result)
        }
    }
}

/// `self database --install <name>`: the connection a plugin gives, without the password.
pub fn print_database(runtime: &Runtime, name: &str) -> i32 {
    let found = Target::find(name).and_then(|target| database(runtime, &target));
    match found {
        Ok(c) => {
            let shown = serde_json::json!({
                "host": c.host, "port": c.port, "database": c.database, "user": c.user,
                "table_prefix": c.table_prefix, "charset": c.charset,
            });
            let _ = std::io::Write::write_all(
                &mut std::io::stdout().lock(),
                format!("{shown}\n").as_bytes(),
            );
            0
        }
        Err(message) => {
            eprintln!("speedyspec: {message}");
            1
        }
    }
}
