//! Start-up: boots PHP, loads plugins safely and collects what they contribute.
//!
//! Plugins load before the command tree exists, because they are what adds to it. Nothing a
//! plugin does while loading may reach stdout, break a built-in command or stop another plugin,
//! so every failure here becomes one `speedyspec: ...` line on stderr naming the plugin.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use semver::Version;
use speedyspec_driver::{App, Command, ContractError, ObjectReader, decode_apps, decode_commands};
use speedyspec_hooks::{
    AddOptions, Callback, CallbackId, HookError, Hooks, Key, SharedHooks, Value,
};
use speedyspec_hooks_memory::MemoryHooks;
use speedyspec_php::{BootOptions, Output, Php, PhpError};
use speedyspec_plugin::{Loader, Plugin, PluginError, discover, load_all, user_plugin_dir};

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

/// Wraps the real hook store to remember which plugin registered a callback for which hook.
///
/// A contract error (a filter returned the wrong thing) cannot say whose callback returned it,
/// because the hook contract only reports owners for callbacks that fail. The names recorded
/// here let the message point at the plugins that took part.
struct OwnerTracking {
    inner: SharedHooks,
    adds: RefCell<Vec<(String, String)>>,
}

impl OwnerTracking {
    /// "a plugin" when no owner is known, the name when one is, "one of a, b" otherwise.
    fn describe_owners(&self, hook: &str) -> String {
        let mut owners: Vec<String> = Vec::new();
        for (name, owner) in self.adds.borrow().iter() {
            if name == hook && !owners.contains(owner) {
                owners.push(owner.clone());
            }
        }
        match owners.len() {
            0 => "a plugin".to_owned(),
            1 => owners.remove(0),
            _ => format!("one of {}", owners.join(", ")),
        }
    }
}

impl Hooks for OwnerTracking {
    fn add(
        &self,
        hook: &str,
        callback: Rc<Callback>,
        options: AddOptions,
    ) -> Result<CallbackId, HookError> {
        if let Some(owner) = &options.owner {
            self.adds
                .borrow_mut()
                .push((hook.to_owned(), owner.clone()));
        }
        self.inner.add(hook, callback, options)
    }

    fn remove(&self, hook: &str, id: CallbackId) -> Result<bool, HookError> {
        self.inner.remove(hook, id)
    }

    fn remove_keyed(&self, hook: &str, key: &str, priority: i32) -> Result<bool, HookError> {
        self.inner.remove_keyed(hook, key, priority)
    }

    fn has(&self, hook: &str) -> Result<bool, HookError> {
        self.inner.has(hook)
    }

    fn has_keyed(&self, hook: &str, key: &str) -> Result<bool, HookError> {
        self.inner.has_keyed(hook, key)
    }

    fn run(&self, hook: &str, args: &[Value]) -> Result<(), HookError> {
        self.inner.run(hook, args)
    }

    fn filter(&self, hook: &str, value: Value, args: &[Value]) -> Result<Value, HookError> {
        self.inner.filter(hook, value, args)
    }

    fn ran(&self, hook: &str) -> Result<u64, HookError> {
        self.inner.ran(hook)
    }
}

/// `Php` as the driver crate's object reader. A wrapper, because neither the trait nor `Php` is
/// local to this crate.
struct PhpReader<'a>(&'a Php);

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
        // Output while loading is noise, never command output, so it goes to stderr; the owner
        // makes the plugin's hook registrations traceable.
        php.set_owner(Some(&plugin.manifest.name));
        php.redirect_output(Output::Stderr);
        let result = php.require(&plugin.entry_path());
        php.redirect_output(Output::Stdout);
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
        other => eprintln!(
            "speedyspec: plugin {} skipped: {other}",
            other.plugin_label()
        ),
    }
}

/// Everything start-up produced. `php` is the last field so it is dropped last: the values
/// before it may hold PHP objects, which are inert once PHP has shut down.
pub struct Runtime {
    pub loaded: Vec<Plugin>,
    pub apps: Vec<App>,
    pub commands: Vec<Command>,
    pub php: Php,
}

/// Runs the start-up sequence. An error is a reason the binary cannot run at all (PHP did not
/// start); everything a plugin does wrong is reported and survived.
pub fn boot(args: &[String]) -> Result<Runtime, String> {
    let options = prescan(args);
    let bundled = Version::parse(CONTRACTS_VERSION)
        .map_err(|e| format!("bundled contracts version {CONTRACTS_VERSION:?} is invalid: {e}"))?;

    let hooks = Rc::new(OwnerTracking {
        inner: Rc::new(MemoryHooks::new()),
        adds: RefCell::new(Vec::new()),
    });
    let php = Php::boot(BootOptions {
        hooks: hooks.clone(),
        preload: CONTRACTS,
    })
    .map_err(|e| e.to_string())?;

    let mut loaded = Vec::new();
    if !options.no_plugins {
        let mut dirs: Vec<PathBuf> = user_plugin_dir().into_iter().collect();
        dirs.extend(options.plugin_dirs.iter().cloned());
        let (plugins, manifest_errors) = discover(&dirs);
        manifest_errors.iter().for_each(report_skipped);
        let report = load_all(plugins, &bundled, &PhpLoader(&php));
        report.skipped.iter().for_each(report_skipped);
        loaded = report.loaded;
    }

    if let Err(error) = hooks.run(HOOK_INIT, &[]) {
        report_hook_error(&hooks, &error);
    }
    let reader = PhpReader(&php);
    let commands = collect(&hooks, HOOK_COMMANDS, |v| {
        decode_each_command(v, &reader, &hooks)
    });
    let apps = collect(&hooks, HOOK_APPS, |v| decode_apps(v, &reader));

    Ok(Runtime {
        loaded,
        apps,
        commands,
        php,
    })
}

/// Decodes commands one at a time. `decode_commands` refuses a whole list that repeats a name, but
/// two plugins adding the same name is expected here (the first one wins, in `commands`), and one
/// plugin's bad command should not take the others down with it. A problem is reported against
/// the command's index in the full list.
fn decode_each_command(
    value: &Value,
    reader: &dyn ObjectReader,
    hooks: &OwnerTracking,
) -> Result<Vec<Command>, ContractError> {
    let Value::Array(items) = value else {
        return decode_commands(value, reader);
    };
    let mut commands = Vec::new();
    for (index, (key, item)) in items.iter().enumerate() {
        let single = Value::Array(vec![(Key::Int(0), item.clone())]);
        let decoded = if *key == Key::Int(index as i64) {
            decode_commands(&single, reader)
        } else {
            // Not a list: let the decoder say so for the whole value.
            return decode_commands(value, reader);
        };
        match decoded {
            Ok(mut one) => commands.append(&mut one),
            Err(mut error) => {
                error.field = error.field.replacen("[0]", &format!("[{index}]"), 1);
                eprintln!(
                    "speedyspec: {}: {HOOK_COMMANDS}: {error}",
                    hooks.describe_owners(HOOK_COMMANDS)
                );
            }
        }
    }
    Ok(commands)
}

/// Fires a filter over an empty list and decodes the result; any problem is reported and the
/// list counts as empty.
fn collect<T>(
    hooks: &OwnerTracking,
    hook: &str,
    decode: impl FnOnce(&Value) -> Result<Vec<T>, ContractError>,
) -> Vec<T> {
    match hooks.filter(hook, Value::Array(Vec::new()), &[]) {
        Ok(value) => decode(&value).unwrap_or_else(|error| {
            eprintln!(
                "speedyspec: {}: {hook}: {error}",
                hooks.describe_owners(hook)
            );
            Vec::new()
        }),
        Err(error) => {
            report_hook_error(hooks, &error);
            Vec::new()
        }
    }
}

fn report_hook_error(hooks: &OwnerTracking, error: &HookError) {
    let who = error
        .owner
        .clone()
        .unwrap_or_else(|| hooks.describe_owners(&error.hook));
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
