//! An install, and what its app's plugin says about it: how to run the app
//! (`speedyspec_app_runtime`) and where its database is (`speedyspec_app_database`). Both
//! filters start from null and are called with the `Install` contract; each answer is checked
//! as it comes back and a bad one is blamed on its plugin.
//!
//! An install is found on this machine (its local record), or in a cluster when the command is given the Kubernetes
//! driver's flags (its record in the namespace).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use clap::{Arg, ArgAction, ArgMatches, Command as Clap};
use speedyspec_driver::{
    AppRuntime, ContractError, DatabaseConnection, DatabaseOrTask, Install, ObjectReader, Workload,
    database_connection_from_json, decode_app_runtime, decode_database_or_task,
};
use speedyspec_driver_kubernetes::{Connection, cluster_flags, find_install, open_cluster};
use speedyspec_hooks::{Hooks, Value};
use speedyspec_php::Output;

use crate::attribution::{Checked, Validator};
use crate::boot::{PhpReader, Runtime, report_hook_error};
use crate::installs;

const HOOK_RUNTIME: &str = "speedyspec_app_runtime";
const HOOK_DATABASE: &str = "speedyspec_app_database";
const INSTALL: &str = "SpeedySpec\\Cli\\Contracts\\Install";

/// The cluster a Kubernetes install is in, and the namespace of its record.
pub struct Cluster {
    pub connection: Connection,
    pub namespace: String,
}

/// An install found by name: on this machine, or in a cluster.
pub struct Target {
    pub install: Install,
    /// Where a Kubernetes install is; `None` for one on this machine.
    pub cluster: Option<Cluster>,
}

/// Adds the Kubernetes driver's flags that find an install in its cluster to `command`.
pub fn with_cluster_args(mut command: Clap) -> Clap {
    for flag in cluster_flags() {
        let arg = Arg::new(flag.name).long(flag.name).help(flag.help);
        command = command.arg(if flag.takes_value {
            arg.action(ArgAction::Set)
                .value_name(flag.name.to_uppercase())
        } else {
            arg.action(ArgAction::SetTrue)
        });
    }
    command
}

/// `--install <NAME>`, and the Kubernetes driver's flags that find the install in its cluster.
pub fn install_args() -> Vec<Arg> {
    let mut args = vec![
        Arg::new("install")
            .long("install")
            .required(true)
            .value_name("NAME")
            .help("The install, as app installs lists it"),
    ];
    args.extend(with_cluster_args(Clap::new("x")).get_arguments().cloned());
    args.extend(crate::remote::runner_args());
    args
}

/// The cluster flags given to a command built with `with_cluster_args`; `None` when there are none, and the install
/// is on this machine.
pub fn given_cluster_flags(matches: &ArgMatches) -> Option<BTreeMap<String, String>> {
    let mut given = BTreeMap::new();
    for flag in cluster_flags() {
        // A command may not have every flag (app install has each driver's own): one it lacks is not given.
        if flag.takes_value {
            if let Ok(Some(value)) = matches.try_get_one::<String>(flag.name) {
                given.insert(flag.name.to_owned(), value.clone());
            }
        } else if let Ok(Some(true)) = matches.try_get_one::<bool>(flag.name) {
            given.insert(flag.name.to_owned(), "1".to_owned());
        }
    }
    (!given.is_empty()).then_some(given)
}

impl Target {
    /// Install `name`: on this machine without `cluster` flags, in the cluster they name with them.
    pub fn find(name: &str, cluster: Option<&BTreeMap<String, String>>) -> Result<Target, String> {
        if let Some(flags) = cluster {
            return Target::in_cluster(name, flags);
        }
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
        Ok(Target {
            install,
            cluster: None,
        })
    }

    /// Install `name` from the command's own flags.
    pub fn from_matches(name: &str, matches: &ArgMatches) -> Result<Target, String> {
        Target::find(name, given_cluster_flags(matches).as_ref())
    }

    fn in_cluster(name: &str, flags: &BTreeMap<String, String>) -> Result<Target, String> {
        let (connection, namespace) = open_cluster(flags)?;
        let found = find_install(&connection, name, &namespace)?;
        let install = Install {
            name: found.record.name.clone(),
            app: found.record.app.clone(),
            parts: found.record.parts.clone(),
            driver: "kubernetes".to_owned(),
            root: None,
        };
        Ok(Target {
            install,
            cluster: Some(Cluster {
                connection,
                namespace,
            }),
        })
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

/// The workload that serves a Kubernetes install, as its app's plugin says.
pub fn workload(runtime: &Runtime, target: &Target) -> Result<Workload, String> {
    self::runtime(runtime, target)?.workload.ok_or_else(|| {
        format!(
            "{}'s plugin does not say which workload serves it on Kubernetes",
            target.install.app
        )
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
/// with. A Kubernetes install's task runs in `remote`, its runner pod.
pub fn database(
    runtime: &Runtime,
    target: &Target,
    remote: Option<&crate::remote::Remote<'_>>,
) -> Result<DatabaseConnection, String> {
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
            let outcome = match (&target.cluster, remote) {
                (None, _) => crate::runner::run(target.root()?, &app, &task),
                (Some(_), Some(remote)) => remote.run_task(&app, &task),
                (Some(_), None) => {
                    return Err(format!(
                        "{}'s plugin finds its database with a task, which runs in a runner pod on Kubernetes",
                        target.install.app
                    ));
                }
            }
            .map_err(|e| e.0)?;
            database_connection_from_json(&outcome.result)
        }
    }
}

/// `self database --install <name>`: the connection a plugin gives, without the password.
pub fn print_database(runtime: &Runtime, matches: &ArgMatches) -> i32 {
    let name = matches.get_one::<String>("install").expect("required");
    let found =
        Target::from_matches(name, matches).and_then(|target| database(runtime, &target, None));
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
