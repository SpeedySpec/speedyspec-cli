//! The `speedyspec` executable. All the work is in [`commands::run`].

mod app;
mod app_target;
mod attribution;
mod backup_cmd;
mod boot;
mod commands;
mod composer_area;
mod composer_child;
mod composer_cmd;
mod composer_host;
mod db_cmd;
mod drivers;
mod extension_cmd;
mod extensions;
mod install;
mod installs;
mod plugin_cmd;
mod prompt;
mod remote;
mod runner;
mod runner_child;
mod self_cmd;
mod setup;

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = Vec::new();
    for arg in std::env::args_os().skip(1) {
        match arg.into_string() {
            Ok(arg) => args.push(arg),
            Err(arg) => {
                eprintln!("speedyspec: argument {arg:?} is not valid UTF-8");
                return ExitCode::from(2);
            }
        }
    }
    commands::run(&args)
}
