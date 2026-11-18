//! Rust mirror of the PHP contracts in `SpeedySpec\\Cli\\Contracts`, and the interface every
//! install driver implements.

mod app;
mod contracts;
mod driver;
#[cfg(test)]
mod fake_reader;
mod kubernetes;
mod names;

pub use app::{
    AppRuntime, AppTask, DatabaseConnection, DatabaseOrTask, Install,
    database_connection_from_json, decode_app_runtime, decode_app_task, decode_database_connection,
    decode_database_or_task,
};
pub use contracts::{
    App, AppPart, Command, CommandArgument, CommandOption, ContractError, InstallOption,
    ObjectReader, ZipSource, decode_app, decode_apps, decode_command, decode_commands,
    decode_install_option, decode_zip_source, is_fetchable_url,
};
pub use driver::{
    Driver, DriverError, DriverFlag, DriverInput, LocalInstall, PluginRef, Prepared, Progress,
    UninstallInput, UninstallPlan,
};
pub use kubernetes::{KubernetesResources, WaitFor, decode_kubernetes_resources, value_to_json};
pub use names::{install_name_problem, namespace_problem};
