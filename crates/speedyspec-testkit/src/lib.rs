//! Test support shared by the workspace's test suites. Never a dependency of shipped code.

mod server;
mod zipfile;

pub use server::{Behaviour, Request, Route, Server};
pub use zipfile::{Entry, build_zip};
