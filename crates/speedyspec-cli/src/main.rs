//! The `speedyspec` executable. All the work is in [`commands::run`].

mod app;
mod attribution;
mod boot;
mod commands;
mod drivers;
mod install;
mod installs;
mod plugin_cmd;
mod prompt;
mod runner;
mod runner_child;
mod self_cmd;

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
