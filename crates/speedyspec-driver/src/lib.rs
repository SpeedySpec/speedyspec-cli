//! Rust mirror of the PHP contracts in `SpeedySpec\\Cli\\Contracts`, and the interface every
//! install driver implements.

mod app;
mod backup;
mod contracts;
mod driver;
mod extensions;
#[cfg(test)]
mod fake_reader;
mod kubernetes;
mod names;

pub use app::{
    AppRuntime, AppTask, DatabaseConnection, DatabaseOrTask, Install, MaintenanceFile, Tls,
    Workload, WorkloadKind, database_connection_from_json, decode_app_runtime, decode_app_task,
    decode_database_connection, decode_database_or_task,
};
pub use backup::{BackupSpec, PrefixedColumn, SiteUrl, decode_backup_spec, is_sql_name};
pub use contracts::{
    App, AppPart, Command, CommandArgument, CommandOption, ContractError, InstallOption,
    ObjectReader, ZipSource, decode_app, decode_apps, decode_command, decode_commands,
    decode_install_option, decode_zip_source, is_fetchable_url,
};
pub use driver::{
    Driver, DriverError, DriverFlag, DriverInput, LocalInstall, PluginRef, Prepared, Progress,
    UninstallInput, UninstallPlan,
};
pub use extensions::{
    Extension, ExtensionArchive, ExtensionLayout, ExtensionSource, Kind, SetupPlan,
    decode_extension, decode_extension_archive, decode_extension_layout, decode_extension_sources,
    decode_setup_plan, slug_problem,
};
pub use kubernetes::{KubernetesResources, WaitFor, decode_kubernetes_resources, value_to_json};
pub use names::{install_name_problem, label_problem, namespace_problem};
