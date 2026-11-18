//! Finds speedyspec plugins on disk, validates their manifests and hands them to a
//! [`Loader`]. This crate runs no PHP itself; the real loader lives elsewhere.

mod discover;
mod load;
mod manifest;
mod store;

pub use discover::{SearchDir, UserPluginDir, discover, user_plugin_dir};
pub use load::{LoadReport, Loader, load_all};
pub use manifest::{MANIFEST_FILE, Manifest, Plugin, PluginError, parse_manifest, valid_name};
pub use store::{directory_name, find_installed, remove_installed};
