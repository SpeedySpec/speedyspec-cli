//! The command tree, built at run time from the built-ins plus what plugins contributed, and
//! dispatch to whichever command was asked for.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{Arg, ArgAction, ArgMatches, Command as Clap};
use speedyspec_driver::{Command as PluginCommand, InstallOption};
use speedyspec_hooks::{Key, Value};
use speedyspec_php::{PHP_VERSION, PhpError};

use crate::boot::{CONTRACTS_VERSION, Runtime, boot};
use crate::drivers;

/// Names a plugin command may not take: the built-in commands, and `help`, which is clap's own.
pub(crate) const RESERVED: &[&str] = &["app", "plugin", "theme", "composer", "self", "php", "help"];

/// Names no plugin argument or option may take: they would clash with the global flags.
const GLOBAL_NAMES: &[&str] = &[
    "help",
    "version",
    "plugin-dir",
    "no-plugins",
    "no-interaction",
];

/// Runs the whole program and returns its exit code. Runtime (and with it PHP) is dropped
/// before returning, so PHP shuts down cleanly and its output is flushed.
pub fn run(args: &[String]) -> ExitCode {
    // `self` needs neither PHP nor plugins; a broken plugin must not stop an update.
    if crate::self_cmd::runs_before_plugins(args) {
        let code = run_without_plugins(args);
        let _ = std::io::stdout().flush();
        return ExitCode::from(exit_byte(i64::from(code)));
    }
    let code = match boot(args) {
        Ok(runtime) => {
            let code = dispatch(&runtime, args);
            let _ = std::io::stdout().flush();
            drop(runtime);
            code
        }
        Err(error) => {
            eprintln!("speedyspec: {error}");
            1
        }
    };
    let _ = std::io::stdout().flush();
    ExitCode::from(exit_byte(i64::from(code)))
}

/// The exit status for what a handler returned: 0 and 1 to 255 as they are; anything else
/// (negative, or too big for an exit status) is a failure, 1, never wrapped round to 0.
fn handler_status(status: i64) -> i32 {
    match status {
        0..=255 => i32::try_from(status).expect("0..=255 fits an i32"),
        _ => 1,
    }
}

/// An exit status as the operating system will see it.
fn exit_byte(status: i64) -> u8 {
    u8::try_from(status.rem_euclid(256)).expect("rem_euclid(256) fits a byte")
}

/// Parses the arguments with the built-in commands only and runs `self`.
fn run_without_plugins(args: &[String]) -> i32 {
    let tree = build_tree(&[], &[]);
    match tree
        .try_get_matches_from(std::iter::once("speedyspec".to_owned()).chain(args.iter().cloned()))
    {
        Ok(matches) => match matches.subcommand() {
            Some(("self", sub)) => crate::self_cmd::run(sub),
            _ => unreachable!("runs_before_plugins found a self command"),
        },
        Err(error) => {
            let code = error.exit_code();
            let _ = error.print();
            code
        }
    }
}

fn dispatch(runtime: &Runtime, args: &[String]) -> i32 {
    // Start-up has already dropped (and reported) every command that cannot be offered.
    let plugin_commands = &runtime.commands;
    // Install options depend on the app and driver, so plugins are asked for them before the
    // tree is built whenever the arguments already say which those are.
    let prepass = crate::app::prepass(runtime, args);
    let install_options = prepass.as_ref().map_or(&[][..], |p| &p.options[..]);
    let tree = build_tree(plugin_commands, install_options);
    let matches = match tree
        .clone()
        .try_get_matches_from(std::iter::once("speedyspec".to_owned()).chain(args.iter().cloned()))
    {
        Ok(matches) => matches,
        Err(error) => {
            let code = error.exit_code();
            let _ = error.print();
            if error.kind() == ErrorKind::UnknownArgument
                && prepass.is_none()
                && crate::app::is_install(args)
            {
                eprintln!(
                    "speedyspec: install options can be given as flags once --driver is known"
                );
            }
            return code;
        }
    };

    if matches.get_flag("version") {
        // Never println!: it panics when stdout is a closed pipe (`speedyspec --version | true`).
        // A write that fails is dropped and the command still succeeds.
        let _ = write!(
            std::io::stdout().lock(),
            "speedyspec {}\nphp {PHP_VERSION}\ncontracts {CONTRACTS_VERSION}\n",
            crate::self_cmd::VERSION
        );
        return 0;
    }
    match matches.subcommand() {
        None => {
            eprintln!("{}", tree.clone().render_help());
            2
        }
        Some(("php", sub)) => run_php(runtime, sub),
        Some(("self", sub)) => match sub.subcommand() {
            Some(("database", database)) => crate::app_target::print_database(runtime, database),
            Some(("plugin", plugin)) => match plugin.subcommand() {
                Some(("list", _)) => list_plugins(runtime),
                Some(("install", install)) => crate::plugin_cmd::install(install),
                Some(("remove", remove)) => crate::plugin_cmd::remove(remove),
                _ => unreachable!("clap requires a plugin subcommand"),
            },
            _ => crate::self_cmd::run(sub),
        },
        Some(("composer", sub)) => crate::composer_cmd::run(sub),
        Some(("plugin", sub)) => {
            crate::extension_cmd::run(runtime, speedyspec_driver::Kind::Plugin, sub)
        }
        Some(("theme", sub)) => {
            crate::extension_cmd::run(runtime, speedyspec_driver::Kind::Theme, sub)
        }
        Some(("app", sub)) => match sub.subcommand() {
            Some(("list", _)) => crate::app::list(&runtime.apps),
            Some(("installs", _)) => crate::installs::print_list(),
            Some(("db", db)) => crate::db_cmd::run(runtime, db),
            Some(("backup", backup)) => crate::backup_cmd::run_backup(runtime, backup),
            Some(("restore", restore)) => crate::backup_cmd::run_restore(runtime, restore),
            Some(("install", install)) => crate::app::install(runtime, install, prepass),
            Some(("uninstall", uninstall)) => crate::app::uninstall(uninstall),
            _ => unreachable!("clap requires an app subcommand"),
        },
        Some((name, sub)) => {
            let command = plugin_commands
                .iter()
                .find(|c| c.name == name)
                .expect("clap only matches commands in the tree");
            run_plugin_command(runtime, command, sub)
        }
    }
}

/// What would make clap refuse or panic on this command's declared arguments and options.
pub(crate) fn clap_problem(command: &PluginCommand) -> Option<String> {
    let mut names: Vec<&str> = Vec::new();
    let names_in_use = command
        .arguments
        .iter()
        .map(|a| a.name.as_str())
        .chain(command.options.iter().map(|o| o.name.as_str()));
    for name in names_in_use {
        if GLOBAL_NAMES.contains(&name) {
            return Some(format!("\"{name}\" is reserved for a global option"));
        }
        if names.contains(&name) {
            return Some(format!("\"{name}\" is declared twice"));
        }
        names.push(name);
    }
    let mut optional_seen = false;
    for argument in &command.arguments {
        if !argument.required {
            optional_seen = true;
        } else if optional_seen {
            return Some(format!(
                "required argument \"{}\" follows an optional one",
                argument.name
            ));
        }
    }
    None
}

/// `self plugin`: the CLI's own plugins.
fn self_plugin_clap() -> Clap {
    Clap::new("plugin")
        .about("Work with plugins")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(Clap::new("list").about("List the loaded plugins"))
        .subcommand(
            Clap::new("install")
                .about("Install a plugin from a zip URL or a GitHub release")
                .arg(
                    Arg::new("source")
                        .required(true)
                        .value_name("SOURCE")
                        .help("https://... zip URL, owner/repo or owner/repo@tag"),
                )
                .arg(
                    Arg::new("allow-composer-plugins")
                        .long("allow-composer-plugins")
                        .action(ArgAction::SetTrue)
                        .help("Run the composer plugins and scripts of the plugin's composer packages"),
                )
                .arg(yes_flag()),
        )
        .subcommand(
            Clap::new("remove")
                .about("Remove an installed plugin")
                .arg(
                    Arg::new("name")
                        .required(true)
                        .value_name("NAME")
                        .help("The plugin's name, as `self plugin list` shows it"),
                )
                .arg(yes_flag()),
        )
}

fn build_tree(plugin_commands: &[PluginCommand], install_options: &[InstallOption]) -> Clap {
    let mut tree = Clap::new("speedyspec")
        .about("Installs apps that plugins describe.")
        .disable_version_flag(true)
        .arg(
            Arg::new("version")
                .long("version")
                .short('V')
                .action(ArgAction::SetTrue)
                .help("Print the speedyspec, PHP and contracts versions"),
        )
        .arg(
            Arg::new("no-plugins")
                .long("no-plugins")
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Do not load any plugins"),
        )
        .arg(
            Arg::new("no-interaction")
                .long("no-interaction")
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Never ask questions; use defaults"),
        )
        .arg(
            Arg::new("plugin-dir")
                .long("plugin-dir")
                .value_name("PATH")
                .global(true)
                .action(ArgAction::Append)
                .help("Load plugins from this directory as well (repeatable)"),
        )
        .subcommand(
            Clap::new("php")
                .about("Run a PHP file with the embedded interpreter")
                .arg(Arg::new("file").required(true).help("The PHP file to run"))
                .arg(
                    Arg::new("args")
                        .num_args(0..)
                        .trailing_var_arg(true)
                        .allow_hyphen_values(true)
                        .help("Arguments for the script"),
                ),
        )
        .subcommand(crate::self_cmd::clap().subcommand(self_plugin_clap()))
        .subcommand(
            Clap::new("app")
                .about("Work with the apps plugins provide")
                .subcommand_required(true)
                .arg_required_else_help(true)
                .subcommand(Clap::new("list").about("List the apps that can be installed"))
                .subcommand(
                    Clap::new("installs")
                        .about("List the installs speedyspec recorded on this machine"),
                )
                .subcommand(install_clap(install_options))
                .subcommand(uninstall_clap())
                .subcommand(crate::db_cmd::clap())
                .subcommand(crate::backup_cmd::backup_clap())
                .subcommand(crate::backup_cmd::restore_clap()),
        )
        .subcommand(crate::composer_cmd::clap())
        .subcommand(crate::extension_cmd::clap(speedyspec_driver::Kind::Plugin))
        .subcommand(crate::extension_cmd::clap(speedyspec_driver::Kind::Theme));
    for command in plugin_commands {
        tree = tree.subcommand(plugin_clap(command));
    }
    tree
}

/// `app uninstall`: the flags of every compiled-in driver that can uninstall.
fn uninstall_clap() -> Clap {
    let mut clap = Clap::new("uninstall")
        .about("Remove an install")
        .arg(
            Arg::new("install-name")
                .required(true)
                .value_name("NAME")
                .help("The install's name (the app's id unless --name was given)"),
        )
        .arg(
            Arg::new("driver")
                .long("driver")
                .value_name("ID")
                .help("The driver that installed it"),
        )
        .arg(yes_flag());
    let mut seen: Vec<&str> = Vec::new();
    for driver in drivers::all() {
        for flag in driver.uninstall_flags() {
            if seen.contains(&flag.name) {
                continue;
            }
            seen.push(flag.name);
            let arg = Arg::new(flag.name).long(flag.name).help(format!(
                "{} [{}]",
                flag.help,
                driver.id()
            ));
            clap = clap.arg(if flag.takes_value {
                arg.action(ArgAction::Set)
                    .value_name(flag.name.to_uppercase())
            } else {
                arg.action(ArgAction::SetTrue)
            });
        }
    }
    clap
}

fn yes_flag() -> Arg {
    Arg::new("yes")
        .long("yes")
        .short('y')
        .action(ArgAction::SetTrue)
        .help("Do not ask for confirmation")
}

/// `app install`: its own flags, every compiled-in driver's flags, and one flag per install
/// option the plugins declared for the app and driver the arguments name.
fn install_clap(install_options: &[InstallOption]) -> Clap {
    let mut clap = Clap::new("install")
        .about("Install an app")
        .arg(Arg::new("app").value_name("APP").help("The app to install"))
        .arg(
            Arg::new("part")
                .long("part")
                .value_name("ID")
                .action(ArgAction::Append)
                .help("A part of the app to install (repeatable; default: its default parts)"),
        )
        .arg(
            Arg::new("driver")
                .long("driver")
                .value_name("ID")
                .help("How to install it"),
        );
    let mut seen: Vec<&str> = Vec::new();
    for driver in drivers::all() {
        for flag in driver.flags() {
            if seen.contains(&flag.name) {
                continue;
            }
            seen.push(flag.name);
            let help = match flag.default {
                Some(default) => format!("{} [{}: default {default}]", flag.help, driver.id()),
                None => format!("{} [{}]", flag.help, driver.id()),
            };
            let arg = Arg::new(flag.name).long(flag.name).help(help);
            clap = clap.arg(if flag.takes_value {
                arg.action(ArgAction::Set)
                    .value_name(flag.name.to_uppercase())
            } else {
                arg.action(ArgAction::SetTrue)
            });
        }
    }
    for option in install_options {
        let help = match (&option.default, option.required) {
            (Some(default), _) if !option.secret => {
                format!("{} [default: {default}]", option.label)
            }
            (None, true) => format!("{} [required]", option.label),
            _ => option.label.clone(),
        };
        clap = clap.arg(
            Arg::new(option.name.clone())
                .long(option.name.clone())
                .action(ArgAction::Set)
                .value_name("VALUE")
                .help(help),
        );
    }
    clap
}

fn plugin_clap(command: &PluginCommand) -> Clap {
    let mut clap = Clap::new(command.name.clone());
    if !command.description.is_empty() {
        clap = clap.about(command.description.clone());
    }
    for argument in &command.arguments {
        let mut arg = Arg::new(argument.name.clone())
            .value_name(argument.name.to_uppercase())
            .required(argument.required);
        if !argument.description.is_empty() {
            arg = arg.help(argument.description.clone());
        }
        clap = clap.arg(arg);
    }
    for option in &command.options {
        let mut arg = Arg::new(option.name.clone()).long(option.name.clone());
        if !option.description.is_empty() {
            arg = arg.help(option.description.clone());
        }
        arg = if option.takes_value {
            let arg = arg
                .action(ArgAction::Set)
                .value_name(option.name.to_uppercase());
            match &option.default {
                Some(default) => arg.default_value(default.clone()),
                None => arg,
            }
        } else {
            arg.action(ArgAction::SetTrue)
        };
        clap = clap.arg(arg);
    }
    clap
}

fn run_php(runtime: &Runtime, matches: &ArgMatches) -> i32 {
    let file = matches
        .get_one::<String>("file")
        .expect("clap requires the file");
    let script_args: Vec<String> = matches
        .get_many::<String>("args")
        .map(|values| values.cloned().collect())
        .unwrap_or_default();
    match runtime.php.run_file(Path::new(file), &script_args) {
        Ok(status) => status,
        Err(error) => {
            eprintln!("speedyspec: {error}");
            1
        }
    }
}

fn list_plugins(runtime: &Runtime) -> i32 {
    let mut out = std::io::stdout().lock();
    for plugin in &runtime.loaded {
        let _ = writeln!(
            out,
            "{}\t{}\t{}",
            plugin.manifest.name,
            plugin.manifest.version,
            plugin.dir.display()
        );
    }
    0
}

/// Calls a plugin command's handler with its arguments and options, both keyed by name: an
/// argument is its string or null; a flag is a bool; a value option is its string, its default
/// or null.
fn run_plugin_command(runtime: &Runtime, command: &PluginCommand, matches: &ArgMatches) -> i32 {
    let text = |name: &str| {
        matches
            .get_one::<String>(name)
            .map_or(Value::Null, |v| Value::Str(v.clone()))
    };
    let arguments = Value::Array(
        command
            .arguments
            .iter()
            .map(|a| (Key::Str(a.name.clone()), text(&a.name)))
            .collect(),
    );
    let options = Value::Array(
        command
            .options
            .iter()
            .map(|o| {
                let value = if o.takes_value {
                    text(&o.name)
                } else {
                    Value::Bool(matches.get_flag(&o.name))
                };
                (Key::Str(o.name.clone()), value)
            })
            .collect(),
    );
    match runtime.php.call(&command.handler, &[arguments, options]) {
        Ok(Value::Int(status)) => handler_status(status),
        Ok(_) => 0,
        Err(PhpError::Exit(status)) => i32::from(exit_byte(i64::from(status))),
        Err(PhpError::Exception { class, message, .. }) => {
            eprintln!("speedyspec: {}: {class}: {message}", command.name);
            1
        }
        Err(error) => {
            eprintln!("speedyspec: {}: {error}", command.name);
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handler_status_never_wraps_to_success() {
        for (returned, status) in [
            (0, 0),
            (1, 1),
            (7, 7),
            (255, 255),
            (256, 1),
            (512, 1),
            (-1, 1),
            (-256, 1),
            (i64::MAX, 1),
            (i64::MIN, 1),
        ] {
            assert_eq!(handler_status(returned), status, "{returned}");
        }
    }
}
