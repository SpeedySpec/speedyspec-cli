//! The speedyspec zip driver: downloads archives, verifies them and unpacks them safely.

mod driver;
mod extract;
mod fetch;
mod install;

pub use driver::ZipDriver;
pub use extract::{Archive, ExtractError, fetch_and_extract};
pub use fetch::{Fetch, FetchError, HttpFetch};
pub use install::{InstallError, Placement, install_sources};
