//! `speedyspec app ...`.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::rc::Rc;

use clap::ArgMatches;
use speedyspec_driver::{
    App, Driver, DriverInput, Install, InstallOption, PluginRef, Progress, UninstallInput,
    decode_install_option,
};
use speedyspec_hooks::{Hooks, Key, Value};
use speedyspec_php::Output;

use crate::attribution::{Checked, Rejection, Validator};
use crate::boot::{PhpReader, Runtime, at_index, check_list, report_hook_error};
use crate::drivers;
use crate::install::{Given, ResolveError, resolve_choices, resolve_options};
use crate::prompt::{Prompter, TerminalPrompter, confirm, interactive};

const HOOK_INSTALL_OPTIONS: &str = "speedyspec_install_options";
const HOOK_INSTALL_BEFORE: &str = "speedyspec_install_before";
const HOOK_INSTALL_AFTER: &str = "speedyspec_install_after";
const INSTALL_REQUEST: &str = "SpeedySpec\\Cli\\Contracts\\InstallRequest";

/// Flags `app install` itself owns; a plugin's install option may not take one of these names.
pub(crate) const INSTALL_FLAGS: &[&str] = &[
    "app",
    "part",
    "driver",
    "help",
    "version",
    "no-plugins",
    "no-interaction",
    "plugin-dir",
];

/// `app list`: one line per app, `<id>\t<label>\t<drivers>`. A driver this binary does not
/// implement is marked with a trailing `?`, so a listed app never promises more than it can do.
pub fn list(apps: &[App]) -> i32 {
    if apps.is_empty() {
        eprintln!("No apps. Install a plugin that provides one.");
        return 0;
    }
    let implemented = drivers::ids();
    let mut out = std::io::stdout().lock();
    for app in apps {
        let drivers: Vec<String> = app
            .drivers
            .iter()
            .map(|driver| {
                if implemented.contains(&driver.as_str()) {
                    driver.clone()
                } else {
                    format!("{driver}?")
                }
            })
            .collect();
        let _ = writeln!(out, "{}\t{}\t{}", app.id, app.label, drivers.join(","));
    }
    0
}

/// The install options plugins declared for one app and driver, asked before the command tree
/// is built so they can become flags.
pub struct Prepass {
    pub app: String,
    pub driver: String,
    pub options: Vec<InstallOption>,
    /// Declarations that were refused; they stop the install when it is attempted.
    pub rejections: Vec<Rejection>,
}

/// What `app install` was asked for, read straight from the arguments.
struct Scanned {
    app: Option<String>,
    driver: Option<String>,
}

/// Reads the app and `--driver` out of `args` if the command is `app install`. Flags it does
/// not know are skipped; the app must come before any option flag that takes a value.
fn scan_install(args: &[String], value_flags: &[&str]) -> Option<Scanned> {
    let mut positionals: Vec<&str> = Vec::new();
    let mut driver = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        let Some(flag) = arg.strip_prefix("--") else {
            if !arg.starts_with('-') {
                positionals.push(arg);
            }
            continue;
        };
        let (name, inline) = match flag.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (flag, None),
        };
        if !value_flags.contains(&name) {
            continue;
        }
        let value = inline.or_else(|| iter.next().cloned());
        if name == "driver" {
            driver = value;
        }
    }
    if positionals.first() != Some(&"app") || positionals.get(1) != Some(&"install") {
        return None;
    }
    Some(Scanned {
        app: positionals.get(2).map(|app| (*app).to_owned()),
        driver,
    })
}

/// Every flag name that takes a value on `app install`, for the scan.
fn value_flags(all: &[Rc<dyn Driver>]) -> Vec<&'static str> {
    let mut names = vec!["plugin-dir", "part", "driver"];
    for driver in all {
        for flag in driver.flags() {
            if flag.takes_value && !names.contains(&flag.name) {
                names.push(flag.name);
            }
        }
    }
    names
}

/// Whether the arguments are an `app install`, whatever else they hold.
pub fn is_install(args: &[String]) -> bool {
    scan_install(args, &value_flags(&drivers::all())).is_some()
}

/// Asks plugins for their install options when the arguments already say which app and driver:
/// the app is given, and the driver is given or is the only one the app can use here.
pub fn prepass(runtime: &Runtime, args: &[String]) -> Option<Prepass> {
    let all = drivers::all();
    let scanned = scan_install(args, &value_flags(&all))?;
    let app = runtime
        .apps
        .iter()
        .find(|app| Some(&app.id) == scanned.app.as_ref())?;
    let usable: Vec<&str> = drivers::ids()
        .into_iter()
        .filter(|id| app.drivers.iter().any(|driver| driver == id))
        .collect();
    let driver = match scanned.driver.as_deref() {
        Some(driver) if usable.contains(&driver) => driver,
        None if usable.len() == 1 => usable[0],
        _ => return None,
    };
    let (options, rejections) = declared_options(runtime, &app.id, driver, &all);
    Some(Prepass {
        app: app.id.clone(),
        driver: driver.to_owned(),
        options,
        rejections,
    })
}

/// Fires `speedyspec_install_options` and returns the accepted declarations plus what was
/// refused, each blamed on the plugin that declared it.
fn declared_options(
    runtime: &Runtime,
    app: &str,
    driver: &str,
    all: &[Rc<dyn Driver>],
) -> (Vec<InstallOption>, Vec<Rejection>) {
    let mut reserved: Vec<&'static str> = INSTALL_FLAGS.to_vec();
    for driver in all {
        reserved.extend(driver.flags().iter().map(|flag| flag.name));
    }
    let php = Rc::clone(&runtime.php);
    let validator: Rc<Validator> = Rc::new(move |given, returned| {
        check_list(given, returned, |item, index, seen| {
            let option = decode_install_option(item, &PhpReader(&php))
                .map_err(|error| at_index(index, &error))?;
            if reserved.contains(&option.name.as_str()) {
                return Err(format!(
                    "[{index}].name: \"{}\" is a built-in flag",
                    option.name
                ));
            }
            if seen.contains(&option.name) {
                return Err(format!(
                    "[{index}].name: duplicate name \"{}\"",
                    option.name
                ));
            }
            seen.push(option.name);
            Ok(())
        })
    });

    // Declaring options is not command output.
    runtime.php.redirect_output(Output::Stderr);
    runtime.hooks.validate(HOOK_INSTALL_OPTIONS, validator);
    let result = runtime.hooks.filter(
        HOOK_INSTALL_OPTIONS,
        Value::Array(Vec::new()),
        &[Value::Str(app.to_owned()), Value::Str(driver.to_owned())],
    );
    runtime.hooks.clear_validators();
    runtime.php.redirect_output(Output::Stdout);

    let mut rejections = runtime.hooks.take_rejections();
    let items = match result {
        Ok(Value::Array(items)) => items,
        Ok(_) => Vec::new(),
        Err(error) => {
            rejections.push(Rejection {
                owner: error.owner,
                hook: error.hook,
                problem: error.message,
            });
            Vec::new()
        }
    };
    let reader = PhpReader(&runtime.php);
    let options = items
        .iter()
        .filter_map(|(_, item)| decode_install_option(item, &reader).ok())
        .collect();
    (options, rejections)
}

/// Prints each refused plugin answer and says that nothing was installed. Returns the exit code.
fn refuse(rejections: &[Rejection]) -> i32 {
    for rejection in rejections {
        let who = rejection.owner.as_deref().unwrap_or("a plugin");
        eprintln!(
            "speedyspec: {who}: {}: {}",
            rejection.hook, rejection.problem
        );
    }
    eprintln!("speedyspec: nothing was installed");
    1
}

/// Progress on stderr, one line per step.
struct StderrProgress;

impl Progress for StderrProgress {
    fn step(&mut self, message: &str) {
        eprintln!("speedyspec: {message}");
    }
}

fn string_map(pairs: &[(String, String)]) -> Value {
    Value::Array(
        pairs
            .iter()
            .map(|(name, value)| (Key::Str(name.clone()), Value::Str(value.clone())))
            .collect(),
    )
}

pub(crate) fn string_list(items: &[String]) -> Value {
    Value::Array(
        items
            .iter()
            .enumerate()
            .map(|(index, item)| (Key::Int(index as i64), Value::Str(item.clone())))
            .collect(),
    )
}

/// The prompter to hand to a resolving step, or none when the CLI may not ask questions.
fn asker(may_ask: bool, terminal: &mut TerminalPrompter) -> Option<&mut dyn Prompter> {
    if may_ask { Some(terminal) } else { None }
}

/// `app install`: settle the choices, ask the plugins, run the driver, tell the plugins.
pub fn install(runtime: &Runtime, matches: &ArgMatches, prepass: Option<Prepass>) -> i32 {
    if runtime.php.poisoned() {
        eprintln!(
            "speedyspec: a plugin failed while loading, so nothing will be installed; remove it \
             or run with --no-plugins"
        );
        return 1;
    }
    let all = drivers::all();
    let may_ask = interactive(
        matches.get_flag("no-interaction"),
        std::io::stdin().is_terminal(),
    );
    let mut terminal = TerminalPrompter;
    let fail = |error: ResolveError| match error {
        ResolveError::Usage(message) => {
            eprintln!("speedyspec: {message}");
            2
        }
        ResolveError::Failure(message) => {
            eprintln!("speedyspec: {message}");
            1
        }
    };

    let parts: Vec<String> = matches
        .get_many::<String>("part")
        .map(|values| values.cloned().collect())
        .unwrap_or_default();
    let given = Given {
        app: matches.get_one::<String>("app").map(String::as_str),
        parts: &parts,
        driver: matches.get_one::<String>("driver").map(String::as_str),
    };
    let choices = match resolve_choices(
        &given,
        &runtime.apps,
        &drivers::ids(),
        asker(may_ask, &mut terminal),
    ) {
        Ok(choices) => choices,
        Err(error) => return fail(error),
    };
    let driver = all
        .iter()
        .find(|driver| driver.id() == choices.driver)
        .expect("resolve_choices only picks a compiled-in driver");

    // A flag of another driver would be silently ignored otherwise.
    let own_flags = driver.flags();
    for other in all.iter().filter(|other| other.id() != driver.id()) {
        for flag in other.flags() {
            let given = if flag.takes_value {
                matches.get_one::<String>(flag.name).is_some()
            } else {
                matches.get_flag(flag.name)
            };
            if given && !own_flags.iter().any(|own| own.name == flag.name) {
                eprintln!(
                    "speedyspec: --{} belongs to the {} driver, not {}",
                    flag.name,
                    other.id(),
                    driver.id()
                );
                return 2;
            }
        }
    }

    // The options were asked for before the command tree was built if the arguments already
    // named the app and driver; then they are flags, and their values are in `matches`.
    let (declared, rejections, as_flags) = match prepass {
        Some(prepass) if prepass.app == choices.app && prepass.driver == choices.driver => {
            (prepass.options, prepass.rejections, true)
        }
        _ => {
            let (options, rejections) =
                declared_options(runtime, &choices.app, &choices.driver, &all);
            (options, rejections, false)
        }
    };
    if !rejections.is_empty() {
        return refuse(&rejections);
    }
    let mut given_options = BTreeMap::new();
    if as_flags {
        for option in &declared {
            if let Some(value) = matches.get_one::<String>(&option.name) {
                given_options.insert(option.name.clone(), value.clone());
            }
        }
    }
    let options = match resolve_options(&declared, &given_options, asker(may_ask, &mut terminal)) {
        Ok(options) => options,
        Err(error) => return fail(error),
    };

    let mut flags = BTreeMap::new();
    for flag in &own_flags {
        if flag.takes_value {
            if let Some(value) = matches.get_one::<String>(flag.name) {
                flags.insert(flag.name.to_owned(), value.clone());
            }
        } else if matches.get_flag(flag.name) {
            flags.insert(flag.name.to_owned(), "1".to_owned());
        }
    }
    let prepared = match driver.prepare(&DriverInput {
        app: &choices.app,
        parts: &choices.parts,
        flags: &flags,
        interactive: may_ask,
    }) {
        Ok(prepared) => prepared,
        Err(error) => {
            eprintln!("speedyspec: {error}");
            return 1;
        }
    };
    // An install recorded on this machine must not take a name another one already has; checked
    // before anything is downloaded.
    let local = driver.local_install(&prepared);
    if let Some(local) = &local {
        match crate::installs::clash(local, &choices.app) {
            Ok(None) => {}
            Ok(Some(problem)) | Err(problem) => {
                eprintln!("speedyspec: {problem}");
                return 1;
            }
        }
    }
    // A record of this install whose setup did not finish means its files are already in place: only the setup
    // is left, from the first task that did not finish.
    let resume = local.as_ref().and_then(|local| {
        let record = crate::installs::find(&local.name).ok().flatten()?;
        let same = record.app == choices.app && record.location == local.location.to_string_lossy();
        // A directory that is gone (or emptied) since is installed again, not resumed.
        let present =
            std::fs::read_dir(&local.location).is_ok_and(|mut entries| entries.next().is_some());
        let same = same && present;
        (same && record.status == crate::installs::Status::Files).then_some(record.setup_done)
    });

    let request = match runtime.php.construct(
        INSTALL_REQUEST,
        &[
            Value::Str(choices.app.clone()),
            string_list(&choices.parts),
            Value::Str(choices.driver.clone()),
            string_map(&options),
            string_map(&prepared.settings),
        ],
    ) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("speedyspec: could not build the install request: {error}");
            return 1;
        }
    };

    // The before and after actions are where a plugin speaks to the user, so their output is
    // command output. A plugin may stop the install here.
    if let Err(error) = runtime
        .hooks
        .run(HOOK_INSTALL_BEFORE, std::slice::from_ref(&request))
    {
        report_hook_error(&error);
        eprintln!("speedyspec: nothing was installed");
        return 1;
    }

    let result = if resume.is_some() {
        eprintln!("speedyspec: files already in place; finishing setup");
        Value::Array(Vec::new())
    } else {
        // Each plugin's answer is checked as it comes back, so a bad one is blamed on its plugin.
        let hook = format!("speedyspec_driver_{}", driver.id());
        let (checker, php) = (Rc::clone(driver), Rc::clone(&runtime.php));
        let validator: Rc<Validator> = Rc::new(move |given, returned| {
            match checker.check_answer(&returned, &PhpReader(&php)) {
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
        runtime.hooks.validate(&hook, validator);
        let answer = runtime
            .hooks
            .filter(&hook, driver.seed(), std::slice::from_ref(&request));
        // Who answered, before the validators (and this list with them) are cleared.
        let plugins: Vec<PluginRef> = runtime
            .hooks
            .take_contributors()
            .into_iter()
            .map(|name| {
                let version = runtime
                    .loaded
                    .iter()
                    .find(|plugin| plugin.manifest.name == name)
                    .map(|plugin| plugin.manifest.version.to_string())
                    .unwrap_or_default();
                PluginRef { name, version }
            })
            .collect();
        runtime.hooks.clear_validators();
        runtime.php.redirect_output(Output::Stdout);
        let rejections = runtime.hooks.take_rejections();
        if !rejections.is_empty() {
            return refuse(&rejections);
        }
        let answer = match answer {
            Ok(answer) => answer,
            Err(error) => {
                report_hook_error(&error);
                eprintln!("speedyspec: nothing was installed");
                return 1;
            }
        };

        let reader = PhpReader(&runtime.php);
        match driver.install(&prepared, &answer, &reader, &plugins, &mut StderrProgress) {
            Ok(result) => result,
            Err(error) => {
                eprintln!("speedyspec: {error}");
                return 1;
            }
        }
    };

    let mut code = 0;
    let mut record = local.as_ref().map(|local| {
        let mut record =
            crate::installs::Record::new(local, &choices.app, &choices.parts, &choices.driver);
        record.setup_done = resume.unwrap_or(0);
        // Finishing a setup is the same install: it keeps when, by what and with which parts it was made.
        if resume.is_some()
            && let Ok(Some(earlier)) = crate::installs::find(&local.name)
        {
            record.installed_at = earlier.installed_at;
            record.cli_version = earlier.cli_version;
            if earlier.parts != choices.parts {
                eprintln!(
                    "speedyspec: warning: the setup is finished with the parts it was started with ({}), not {}",
                    earlier.parts.join(", "),
                    choices.parts.join(", ")
                );
            }
            record.parts = earlier.parts;
        }
        record
    });
    if let Some(record) = &record
        && let Err(problem) = crate::installs::write(record)
    {
        eprintln!("speedyspec: installed, but {problem}");
        code = 1;
    }
    if let (Some(local), Some(record)) = (&local, &mut record) {
        let install = Install {
            name: local.name.clone(),
            app: choices.app.clone(),
            parts: choices.parts.clone(),
            driver: choices.driver.clone(),
            root: Some(local.location.clone()),
        };
        match crate::setup::plan(runtime, &install, &request) {
            Err(problem) => {
                eprintln!("speedyspec: the files are in place, but {problem}");
                return 1;
            }
            Ok(None) => {}
            Ok(Some(plan)) => {
                let from = usize::try_from(record.setup_done).unwrap_or(usize::MAX);
                match crate::setup::run(runtime, &install, &plan, from) {
                    Ok(()) => record.setup_done = plan.tasks.len() as u64,
                    Err(failure) => {
                        record.setup_done = failure.done as u64;
                        let _ = crate::installs::write(record);
                        eprintln!(
                            "speedyspec: {}; run the same command again to finish it",
                            failure.message
                        );
                        return 1;
                    }
                }
            }
        }
    }
    if let Err(error) = runtime
        .hooks
        .run(HOOK_INSTALL_AFTER, &[request, result.clone()])
    {
        let who = error.owner.as_deref().unwrap_or("a plugin");
        eprintln!(
            "speedyspec: installed, but {who}: {}: {}",
            error.hook, error.message
        );
        code = 1;
    }
    if let Some(record) = &mut record {
        record.status = crate::installs::Status::Installed;
        if let Err(problem) = crate::installs::write(record) {
            eprintln!("speedyspec: installed, but {problem}");
            code = 1;
        }
    }
    let mut out = std::io::stdout().lock();
    for line in driver.report(&result) {
        let _ = writeln!(out, "{line}");
    }
    code
}

/// `app uninstall`: find the install, say what will go, confirm, remove it. Runs no plugin code.
pub fn uninstall(matches: &ArgMatches) -> i32 {
    let all = drivers::all();
    let able: Vec<&Rc<dyn Driver>> = all
        .iter()
        .filter(|driver| !driver.uninstall_flags().is_empty())
        .collect();
    let driver = match matches.get_one::<String>("driver") {
        Some(id) => match all.iter().find(|driver| driver.id() == id) {
            Some(driver) if driver.uninstall_flags().is_empty() => {
                eprintln!("speedyspec: the {id} driver cannot uninstall");
                return 2;
            }
            Some(driver) => driver,
            None => {
                eprintln!("speedyspec: there is no driver \"{id}\"");
                return 2;
            }
        },
        None => match able.as_slice() {
            [only] => *only,
            _ => {
                let ids: Vec<&str> = able.iter().map(|driver| driver.id()).collect();
                eprintln!("speedyspec: give --driver: {}", ids.join(", "));
                return 2;
            }
        },
    };

    let mut flags = BTreeMap::new();
    for flag in driver.uninstall_flags() {
        if flag.takes_value {
            if let Some(value) = matches.get_one::<String>(flag.name) {
                flags.insert(flag.name.to_owned(), value.clone());
            }
        } else if matches.get_flag(flag.name) {
            flags.insert(flag.name.to_owned(), "1".to_owned());
        }
    }
    let name = matches
        .get_one::<String>("install-name")
        .expect("clap requires the install name");
    let plan = match driver.plan_uninstall(&UninstallInput {
        name,
        flags: &flags,
        interactive: interactive(
            matches.get_flag("no-interaction"),
            std::io::stdin().is_terminal(),
        ),
    }) {
        Ok(plan) => plan,
        Err(error) => {
            eprintln!("speedyspec: {error}");
            return 1;
        }
    };
    for line in &plan.summary {
        eprintln!("speedyspec: {line}");
    }
    if let Err(code) = confirm(matches, "Remove it? [y/N]") {
        return code;
    }
    match driver.uninstall(&plan, &mut StderrProgress) {
        Ok(lines) => {
            let mut out = std::io::stdout().lock();
            for line in lines {
                let _ = writeln!(out, "{line}");
            }
            0
        }
        Err(error) => {
            eprintln!("speedyspec: {error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(args: &[&str]) -> Option<(Option<String>, Option<String>)> {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        scan_install(&args, &["plugin-dir", "part", "driver", "base"])
            .map(|scanned| (scanned.app, scanned.driver))
    }

    fn some(text: &str) -> Option<String> {
        Some(text.to_owned())
    }

    #[test]
    fn finds_the_app_and_driver() {
        assert_eq!(
            scan(&["app", "install", "site", "--driver", "zip"]),
            Some((some("site"), some("zip")))
        );
        assert_eq!(
            scan(&[
                "--plugin-dir",
                "/p",
                "app",
                "install",
                "--driver=zip",
                "site"
            ]),
            Some((some("site"), some("zip")))
        );
    }

    #[test]
    fn known_value_flags_do_not_hide_the_app() {
        assert_eq!(
            scan(&[
                "app",
                "install",
                "--base",
                "/srv/site",
                "--part",
                "web",
                "site"
            ]),
            Some((some("site"), None))
        );
    }

    #[test]
    fn unknown_flags_after_the_app_are_skipped() {
        assert_eq!(
            scan(&["app", "install", "site", "--title", "Hi", "--force"]),
            Some((some("site"), None))
        );
    }

    #[test]
    fn other_commands_are_not_installs() {
        assert_eq!(scan(&["app", "list"]), None);
        assert_eq!(scan(&["hello", "app", "install"]), None);
        assert_eq!(scan(&["app", "install"]), Some((None, None)));
        assert_eq!(scan(&[]), None);
    }
}
