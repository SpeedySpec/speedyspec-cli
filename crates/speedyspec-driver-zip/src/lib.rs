//! The speedyspec zip driver: downloads archives, verifies them and unpacks them safely.

mod extract;
mod fetch;

pub use extract::{Archive, ExtractError, fetch_and_extract};
pub use fetch::{Fetch, FetchError, HttpFetch};
