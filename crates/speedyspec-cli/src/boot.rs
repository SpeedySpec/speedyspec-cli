//! Start-up: boots PHP, loads plugins safely and collects what they contribute.
//!
//! Plugins load before the command tree exists, because they are what adds to it. Nothing a
//! plugin does while loading may reach stdout, break a built-in command or stop another plugin,
//! so every failure here becomes one `speedyspec: ...` line on stderr naming the plugin.

use std::path::PathBuf;
use std::rc::Rc;

use semver::Version;
use speedyspec_driver::{App, Command, ContractError, ObjectReader, decode_app, decode_command};
use speedyspec_hooks::{HookError, Hooks, Key, Value};
use speedyspec_hooks_memory::MemoryHooks;
use speedyspec_php::{BootOptions, Output, Php, PhpError, execution_context};
use speedyspec_plugin::{
    Loader, Plugin, PluginError, SearchDir, discover, load_all, user_plugin_dir,
};

use crate::attribution::{AttributedHooks, Checked, Validator};
use crate::commands::{RESERVED, clap_problem};

include!(concat!(env!("OUT_DIR"), "/contracts.rs"));

/// The contracts version bundled into this binary, from the contracts package's `composer.json`.
pub const CONTRACTS_VERSION: &str = env!("CONTRACTS_VERSION");

/// The hook fired once every plugin has loaded.
const HOOK_INIT: &str = "speedyspec_init";
/// The filter plugins append [`Command`] objects to.
const HOOK_COMMANDS: &str = "speedyspec_commands";
/// The filter plugins append [`App`] objects to.
const HOOK_APPS: &str = "speedyspec_apps";

/// The command-line flags that must be known before plugins load.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Prescan {
    pub no_plugins: bool,
    /// Accepted now so scripts can pass it; prompts arrive in a later plan.
    #[allow(dead_code)]
    pub no_interaction: bool,
    pub plugin_dirs: Vec<PathBuf>,
}

/// Finds the global flags without clap, which cannot run yet because the command tree depends on
/// plugins. The scan agrees with clap about where a command's own arguments start: a script's
/// arguments after `php <file>` (and anything after `--`) are never read as flags.
pub fn prescan(args: &[String]) -> Prescan {
    let mut found = Prescan::default();
    let mut iter = args.iter();
    let mut command: Option<&str> = None;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--" => break,
            "--no-plugins" => found.no_plugins = true,
            "--no-interaction" => found.no_interaction = true,
            "--plugin-dir" => {
                if let Some(dir) = iter.next() {
                    found.plugin_dirs.push(PathBuf::from(dir));
                }
            }
            other if other.starts_with("--plugin-dir=") => {
                found
                    .plugin_dirs
                    .push(PathBuf::from(&other["--plugin-dir=".len()..]));
            }
            other if other.starts_with('-') => {}
            other => match command {
                None => command = Some(other),
                // `php <file> ...`: everything after the file belongs to the script.
                Some("php") => break,
                Some(_) => {}
            },
        }
    }
    found
}

/// `Php` as the driver crate's object reader. A wrapper, because neither the trait nor `Php` is
/// local to this crate.
pub(crate) struct PhpReader<'a>(pub(crate) &'a Php);

impl ObjectReader for PhpReader<'_> {
    fn class_of(&self, value: &Value) -> Option<String> {
        self.0.class_of(value)
    }

    fn properties_of(&self, value: &Value) -> Option<Vec<(String, Value)>> {
        self.0.properties_of(value)
    }
}

/// `Php` as the plugin crate's loader.
struct PhpLoader<'a>(&'a Php);

impl Loader for PhpLoader<'_> {
    fn load(&self, plugin: &Plugin) -> Result<(), String> {
        let php = self.0;
        // The owner makes the plugin's hook registrations traceable. Output stays on stderr
        // because `boot` redirects it for the whole of start-up.
        php.set_owner(Some(&plugin.manifest.name));
        let result = php.require(plugin.entry_path());
        php.set_owner(None);
        result.map(drop).map_err(|error| load_reason(&error))
    }
}

/// One line saying why PHP code failed while a plugin was loading.
fn load_reason(error: &PhpError) -> String {
    match error {
        PhpError::Exit(status) => format!("called exit({status}) while loading"),
        PhpError::Exception {
            class,
            message,
            file,
            line,
        } => format!("{class}: {message} ({file}:{line})"),
        PhpError::Fatal {
            message,
            file,
            line,
        } => format!("fatal error: {message} ({file}:{line})"),
        other => other.to_string(),
    }
}

/// Prints the one-line report for a plugin that was not loaded.
fn report_skipped(error: &PluginError) {
    match error {
        PluginError::Manifest { dir, problem } => {
            eprintln!("speedyspec: plugin {} skipped: {problem}", dir.display());
        }
        PluginError::Directory { .. } => eprintln!("speedyspec: {error}"),
        other => eprintln!(
            "speedyspec: plugin {} skipped: {other}",
            other.plugin_label()
        ),
    }
}

/// The speedyspec_wordpress extension's areas: the service container (`SpeedySpec\Container\Container`) and
/// WordPress's Hook API (`add_filter` and the rest, `WP_Hook`, the hook globals), each over its own Rust state.
/// The CLI's plugin API (`SpeedySpec\Hooks\Hook`) is separate and unchanged.
pub fn wordpress_areas() -> Vec<Rc<dyn speedyspec_php::NativeArea>> {
    let container = Rc::new(speedyspec_container::Container::new());
    let memory = Rc::new(speedyspec_wp_hooks_memory::MemoryHookStore::new());
    let hooks = speedyspec_wp_hooks_php::WpHooksArea::new(container.clone(), memory);
    let container_area = speedyspec_container_php::ContainerArea::new("cli", container);
    vec![speedyspec_php_ext::twins_area(), Rc::new(container_area), Rc::new(hooks)]
}

/// Everything start-up produced. `php` is the last field so it is dropped last: the values
/// before it may hold PHP objects, which are inert once PHP has shut down.
pub struct Runtime {
    pub loaded: Vec<Plugin>,
    pub apps: Vec<App>,
    pub commands: Vec<Command>,
    /// The hook registry plugins registered on, for the hooks commands fire later.
    pub hooks: Rc<AttributedHooks>,
    /// Shared with contract checks while they are installed; the only reference otherwise.
    pub php: Rc<Php>,
}

/// Runs the start-up sequence. An error is a reason the binary cannot run at all (PHP did not
/// start); everything a plugin does wrong is reported and survived.
pub fn boot(args: &[String]) -> Result<Runtime, String> {
    let options = prescan(args);
    let bundled = Version::parse(CONTRACTS_VERSION)
        .map_err(|e| format!("bundled contracts version {CONTRACTS_VERSION:?} is invalid: {e}"))?;

    let hooks = Rc::new(AttributedHooks::new(
        Rc::new(MemoryHooks::new()),
        execution_context,
    ));
    let php = Rc::new(
        Php::boot(BootOptions {
            hooks: hooks.clone(),
            preload: CONTRACTS,
            areas: wordpress_areas(),
        })
        .map_err(|e| e.to_string())?,
    );

    // Everything plugin code prints from here until dispatch (while loading, and from the init
    // hook and both filters) is noise, never command output, so it goes to stderr. Nothing
    // below returns early, so stdout is always restored.
    php.redirect_output(Output::Stderr);
    let mut loaded = Vec::new();
    if !options.no_plugins {
        let user = user_plugin_dir();
        for warning in &user.warnings {
            eprintln!("speedyspec: {warning}");
        }
        let mut dirs: Vec<SearchDir> = user.dir.into_iter().map(SearchDir::implicit).collect();
        dirs.extend(options.plugin_dirs.iter().cloned().map(SearchDir::explicit));
        let (plugins, manifest_errors) = discover(&dirs);
        manifest_errors.iter().for_each(report_skipped);
        let report = load_all(plugins, &bundled, &PhpLoader(&php));
        report.skipped.iter().for_each(report_skipped);
        loaded = report.loaded;
    }

    if let Err(error) = hooks.run(HOOK_INIT, &[]) {
        report_hook_error(&error);
    }
    let (commands, apps) = {
        // Each plugin callback's return value is checked as it comes back, so a bad value is
        // blamed on exactly that plugin and dropped before the next plugin sees it. The
        // validators share `php`; the guard drops them (and those references) on every path.
        let _checks = Checks::install(&hooks, &php);
        let reader = PhpReader(&php);
        let commands = collect(&hooks, HOOK_COMMANDS, |v| decode_command(v, &reader));
        let apps = collect(&hooks, HOOK_APPS, |v| decode_app(v, &reader));
        (commands, apps)
    };
    php.redirect_output(Output::Stdout);

    Ok(Runtime {
        loaded,
        apps,
        commands,
        hooks,
        php,
    })
}

/// The contract checks on `speedyspec_commands` and `speedyspec_apps`, for as long as it lives.
struct Checks<'a>(&'a AttributedHooks);

impl<'a> Checks<'a> {
    fn install(hooks: &'a AttributedHooks, php: &Rc<Php>) -> Self {
        let commands_php = Rc::clone(php);
        let commands: Rc<Validator> = Rc::new(move |given, returned| {
            check_list(given, returned, |item, index, seen| {
                let command = decode_command(item, &PhpReader(&commands_php))
                    .map_err(|e| at_index(index, &e))?;
                if RESERVED.contains(&command.name.as_str()) {
                    return Err(format!(
                        "[{index}].name: \"{}\" is a built-in command",
                        command.name
                    ));
                }
                if seen.contains(&command.name) {
                    return Err(format!(
                        "[{index}].name: duplicate name \"{}\" (the first one wins)",
                        command.name
                    ));
                }
                if let Some(problem) = clap_problem(&command) {
                    return Err(format!("[{index}]: {problem}"));
                }
                seen.push(command.name);
                Ok(())
            })
        });
        let apps_php = Rc::clone(php);
        let apps: Rc<Validator> = Rc::new(move |given, returned| {
            check_list(given, returned, |item, index, seen| {
                let app =
                    decode_app(item, &PhpReader(&apps_php)).map_err(|e| at_index(index, &e))?;
                if seen.contains(&app.id) {
                    return Err(format!(
                        "[{index}].id: duplicate id \"{}\" (the first one wins)",
                        app.id
                    ));
                }
                seen.push(app.id);
                Ok(())
            })
        });
        hooks.validate(HOOK_COMMANDS, commands);
        hooks.validate(HOOK_APPS, apps);
        Self(hooks)
    }
}

impl Drop for Checks<'_> {
    fn drop(&mut self) {
        self.0.clear_validators();
    }
}

/// Checks a list one callback returned: every item must pass `check` (given the item, its index
/// and the keys accepted before it), and failing items are dropped. A value that is not a list
/// is dropped whole, and what the callback was given continues instead.
pub(crate) fn check_list(
    given: &Value,
    returned: Value,
    check: impl Fn(&Value, usize, &mut Vec<String>) -> Result<(), String>,
) -> Checked {
    let items = match returned {
        Value::Array(items)
            if items
                .iter()
                .enumerate()
                .all(|(i, (key, _))| *key == Key::Int(i as i64)) =>
        {
            items
        }
        _ => {
            return Checked {
                value: given.clone(),
                problems: vec!["expected a list (an array keyed 0, 1, 2, ...)".to_owned()],
            };
        }
    };
    let mut seen = Vec::new();
    let mut kept = Vec::new();
    let mut problems = Vec::new();
    for (index, (_, item)) in items.into_iter().enumerate() {
        match check(&item, index, &mut seen) {
            Ok(()) => kept.push((Key::Int(kept.len() as i64), item)),
            Err(problem) => problems.push(problem),
        }
    }
    Checked {
        value: Value::Array(kept),
        problems,
    }
}

/// A single item's contract error as `[index].field: problem`.
pub(crate) fn at_index(index: usize, error: &ContractError) -> String {
    if error.field.is_empty() {
        format!("[{index}]: {}", error.problem)
    } else {
        format!("[{index}].{}: {}", error.field, error.problem)
    }
}

/// Fires a filter over an empty list and decodes each item of the result. The checks have
/// already dropped (and reported) every bad item, against the plugin whose callback returned it;
/// whatever still fails here came from no known plugin.
fn collect<T>(
    hooks: &AttributedHooks,
    hook: &str,
    decode: impl Fn(&Value) -> Result<T, ContractError>,
) -> Vec<T> {
    let result = hooks.filter(hook, Value::Array(Vec::new()), &[]);
    report_rejections(hooks);
    let items = match result {
        Ok(Value::Array(items)) => items,
        Ok(_) => {
            eprintln!(
                "speedyspec: a plugin: {hook}: expected a list (an array keyed 0, 1, 2, ...)"
            );
            return Vec::new();
        }
        Err(error) => {
            report_hook_error(&error);
            return Vec::new();
        }
    };
    let mut decoded = Vec::new();
    for (index, (_, item)) in items.iter().enumerate() {
        match decode(item) {
            Ok(one) => decoded.push(one),
            Err(error) => eprintln!("speedyspec: a plugin: {hook}: {}", at_index(index, &error)),
        }
    }
    decoded
}

/// One line per problem the contract checks found, naming the plugin.
pub(crate) fn report_rejections(hooks: &AttributedHooks) {
    for rejection in hooks.take_rejections() {
        let who = rejection.owner.as_deref().unwrap_or("a plugin");
        eprintln!(
            "speedyspec: {who}: {}: {}",
            rejection.hook, rejection.problem
        );
    }
}

pub(crate) fn report_hook_error(error: &HookError) {
    let who = error.owner.as_deref().unwrap_or("a plugin");
    eprintln!("speedyspec: {who}: {}: {}", error.hook, error.message);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(args: &[&str]) -> Prescan {
        prescan(&args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn finds_flags_anywhere_before_a_script_argument() {
        let found = scan(&[
            "--plugin-dir",
            "/a",
            "hello",
            "--no-plugins",
            "--plugin-dir=/b",
            "--no-interaction",
        ]);
        assert!(found.no_plugins && found.no_interaction);
        assert_eq!(
            found.plugin_dirs,
            [PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }

    #[test]
    fn script_arguments_after_php_file_are_not_flags() {
        let found = scan(&[
            "--plugin-dir",
            "/a",
            "php",
            "x.php",
            "--no-plugins",
            "--plugin-dir",
            "/b",
        ]);
        assert!(!found.no_plugins);
        assert_eq!(found.plugin_dirs, [PathBuf::from("/a")]);
    }

    #[test]
    fn nothing_after_a_double_dash_is_a_flag() {
        assert!(!scan(&["hello", "--", "--no-plugins"]).no_plugins);
    }

    #[test]
    fn plugin_dir_value_is_never_a_flag() {
        let found = scan(&["--plugin-dir", "--no-plugins"]);
        assert!(!found.no_plugins);
        assert_eq!(found.plugin_dirs, [PathBuf::from("--no-plugins")]);
    }
}
