//! The `speedyspec` executable. All the work is in [`commands::run`].

mod app;
mod boot;
mod commands;

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
