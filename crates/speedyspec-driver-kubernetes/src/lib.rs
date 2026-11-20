//! The speedyspec Kubernetes driver: applies the resources a plugin returns to a cluster, waits
//! for them and records the install so it can be removed again.

mod cluster;
mod connect;
mod driver;
mod exec;
mod install;
mod manifest;
mod readiness;
mod record;
mod runner;
mod uninstall;

pub use cluster::ClusterError;
pub use connect::{ConnectError, ConnectFlags, Connection, connect};
pub use driver::{
    KubernetesDriver, TOKEN_VARIABLE, cluster_flags, into_cluster, open_cluster, unfinished_setup,
};
pub use install::{Applied, InstallOptions, apply_install, record_setup, wait_and_finish};
pub use manifest::{
    InstallIdentity, ResourceId, identify, install_name_problem, namespace_problem,
    prepare_manifest, record_name,
};
pub use readiness::{KNOWN_KINDS, Readiness, default_wait, parse_timeout, readiness};
pub use record::{Record, Recorded, SCHEMA, Status};
pub use runner::{
    Placement, RUNNER_DIR, RunnerError, RunnerPod, RunnerSpec, place_runner, runner_job,
};
pub use uninstall::{Removal, Removed, find_install, remove_install, summarise};
