//! The command tree, built at run time from the built-ins plus what plugins contributed, and
//! dispatch to whichever command was asked for.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use clap::{Arg, ArgAction, ArgMatches, Command as Clap};
use speedyspec_driver::Command as PluginCommand;
use speedyspec_hooks::{Key, Value};
use speedyspec_php::{PHP_VERSION, PhpError};

use crate::boot::{CONTRACTS_VERSION, Runtime, boot};

/// Names a plugin command may not take. `self` has no subcommands yet but is reserved for the
/// plan that adds them; `help` is clap's own.
const RESERVED: &[&str] = &["app", "plugin", "self", "php", "help"];

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

/// An exit status as the operating system will see it.
fn exit_byte(status: i64) -> u8 {
    u8::try_from(status.rem_euclid(256)).expect("rem_euclid(256) fits a byte")
}

fn dispatch(runtime: &Runtime, args: &[String]) -> i32 {
    let plugin_commands = accept_plugin_commands(&runtime.commands);
    let tree = build_tree(&plugin_commands);
    let matches = match tree
        .clone()
        .try_get_matches_from(std::iter::once("speedyspec".to_owned()).chain(args.iter().cloned()))
    {
        Ok(matches) => matches,
        Err(error) => {
            let code = error.exit_code();
            let _ = error.print();
            return code;
        }
    };

    if matches.get_flag("version") {
        println!("speedyspec {}", env!("CARGO_PKG_VERSION"));
        println!("php {PHP_VERSION}");
        println!("contracts {CONTRACTS_VERSION}");
        return 0;
    }
    match matches.subcommand() {
        None => {
            eprintln!("{}", tree.clone().render_help());
            2
        }
        Some(("php", sub)) => run_php(runtime, sub),
        Some(("app", sub)) => match sub.subcommand() {
            Some(("list", _)) => crate::app::list(&runtime.apps),
            _ => unreachable!("clap requires an app subcommand"),
        },
        Some(("plugin", sub)) => match sub.subcommand() {
            Some(("list", _)) => list_plugins(runtime),
            _ => unreachable!("clap requires a plugin subcommand"),
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

/// Drops plugin commands that cannot be offered, each with a stderr line saying why. A built-in
/// or an earlier plugin's command keeps its name.
fn accept_plugin_commands(commands: &[PluginCommand]) -> Vec<PluginCommand> {
    let mut accepted: Vec<PluginCommand> = Vec::new();
    for command in commands {
        let problem = if RESERVED.contains(&command.name.as_str())
            || accepted.iter().any(|c| c.name == command.name)
        {
            Some("name already taken".to_owned())
        } else {
            clap_problem(command)
        };
        match problem {
            Some(problem) => eprintln!(
                "speedyspec: command \"{}\" from a plugin ignored: {problem}",
                command.name
            ),
            None => accepted.push(command.clone()),
        }
    }
    accepted
}

/// What would make clap refuse or panic on this command's declared arguments and options.
fn clap_problem(command: &PluginCommand) -> Option<String> {
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

fn build_tree(plugin_commands: &[PluginCommand]) -> Clap {
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
        .subcommand(
            Clap::new("app")
                .about("Work with the apps plugins provide")
                .subcommand_required(true)
                .arg_required_else_help(true)
                .subcommand(Clap::new("list").about("List the apps that can be installed")),
        )
        .subcommand(
            Clap::new("plugin")
                .about("Work with plugins")
                .subcommand_required(true)
                .arg_required_else_help(true)
                .subcommand(Clap::new("list").about("List the loaded plugins")),
        );
    for command in plugin_commands {
        tree = tree.subcommand(plugin_clap(command));
    }
    tree
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
        Ok(Value::Int(status)) => i32::from(exit_byte(status)),
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
