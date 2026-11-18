//! The speedyspec Kubernetes driver: applies the resources a plugin returns to a cluster, waits
//! for them and records the install so it can be removed again.

mod connect;

pub use connect::{ConnectError, ConnectFlags, Connection, connect};
