//! The speedyspec Kubernetes driver: applies the resources a plugin returns to a cluster, waits
//! for them and records the install so it can be removed again.

mod cluster;
mod connect;
mod install;
mod manifest;
mod record;

pub use cluster::ClusterError;
pub use connect::{ConnectError, ConnectFlags, Connection, connect};
pub use install::{Applied, InstallOptions, apply_install};
pub use manifest::{
    InstallIdentity, ResourceId, identify, install_name_problem, namespace_problem,
    prepare_manifest, record_name,
};
pub use record::{Record, Recorded, SCHEMA, Status};
