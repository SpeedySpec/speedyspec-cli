//! The speedyspec zip driver: downloads archives, verifies them and unpacks them safely.

mod driver;
mod extract;
mod install;

pub use driver::ZipDriver;
pub use extract::{Archive, ExtractError, Limits, fetch_and_extract, fetch_and_extract_with};
pub use install::{InstallError, Placement, install_sources};
pub use speedyspec_fetch::{Fetch, FetchError, HttpFetch};
