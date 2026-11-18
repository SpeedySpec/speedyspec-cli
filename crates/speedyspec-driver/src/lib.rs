//! Rust mirror of the PHP contracts in `SpeedySpec\\Cli\\Contracts`, and the interface every
//! install driver implements.

mod contracts;
mod driver;
mod kubernetes;

pub use contracts::{
    App, AppPart, Command, CommandArgument, CommandOption, ContractError, InstallOption,
    ObjectReader, ZipSource, decode_app, decode_apps, decode_command, decode_commands,
    decode_install_option, decode_zip_source, is_fetchable_url,
};
pub use driver::{
    Driver, DriverError, DriverFlag, DriverInput, PluginRef, Prepared, Progress, UninstallInput,
    UninstallPlan,
};
pub use kubernetes::{KubernetesResources, WaitFor, decode_kubernetes_resources, value_to_json};
