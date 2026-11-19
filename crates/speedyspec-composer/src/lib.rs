//! speedyspec's composer, in Rust. It installs exactly what a plugin's `composer.lock` lists (no dependency
//! resolving: the plugin ships the lock), checks the packages against the embedded PHP, and keeps the autoload maps
//! as data that the CLI resolves in Rust. The only PHP that runs is the packages' own code.

mod autoload;
mod classmap;
mod install;
mod lock;
mod platform;

use std::fmt;

pub use autoload::{MAPS_FILE, Maps};
pub use classmap::declared;
pub use install::{Installed, install};
pub use lock::{Autoload, Dist, Lock, Package, is_package_name, is_platform};
pub use platform::{Platform, allows, check};

/// Why composer work failed, as one message naming what failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposerError(String);

impl ComposerError {
    pub(crate) fn new(message: impl Into<String>) -> ComposerError {
        ComposerError(message.into())
    }
}

impl fmt::Display for ComposerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ComposerError {}
