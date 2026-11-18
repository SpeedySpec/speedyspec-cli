//! Rust mirror of the PHP contracts in `SpeedySpec\Cli\Contracts`.

mod contracts;

pub use contracts::{
    App, AppPart, Command, CommandArgument, CommandOption, ContractError, ObjectReader,
    decode_apps, decode_commands,
};
