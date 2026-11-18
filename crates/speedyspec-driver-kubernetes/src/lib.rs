//! The speedyspec Kubernetes driver: applies the resources a plugin returns to a cluster, waits
//! for them and records the install so it can be removed again.

mod cluster;
mod connect;
mod driver;
mod install;
mod manifest;
mod readiness;
mod record;

pub use cluster::ClusterError;
pub use connect::{ConnectError, ConnectFlags, Connection, connect};
pub use driver::{KubernetesDriver, TOKEN_VARIABLE};
pub use install::{Applied, InstallOptions, apply_install, wait_and_finish};
pub use manifest::{
    InstallIdentity, ResourceId, identify, install_name_problem, namespace_problem,
    prepare_manifest, record_name,
};
pub use readiness::{KNOWN_KINDS, Readiness, default_wait, parse_timeout, readiness};
pub use record::{Record, Recorded, SCHEMA, Status};
