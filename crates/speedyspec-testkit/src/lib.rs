//! Test support shared by the workspace's test suites. Never a dependency of shipped code.

mod kube;
mod server;
mod zipfile;

pub use kube::FakeCluster;
pub use server::{Behaviour, Received, Reply, Request, Route, Server};
pub use zipfile::{Entry, build_zip};
