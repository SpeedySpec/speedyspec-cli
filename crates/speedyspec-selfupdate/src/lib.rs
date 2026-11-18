//! Updating speedyspec to its latest release: find it on GitHub, download the binary for this
//! platform, check it against the release's `SHA256SUMS`, and put it in place of the running one.

mod release;

pub use release::{
    Asset, Check, REPOSITORY, Release, SUMS, UpdateError, Version, asset_name, check,
    current_asset, latest_release, parse_sums,
};
