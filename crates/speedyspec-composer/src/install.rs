//! Installing a lock: each package's dist is downloaded (its sha1 checked when the lock has one), unpacked with the
//! zip driver's checked extraction, and put under `vendor/<name>`. Everything is built in `vendor.speedyspec-new/`
//! first and swapped in at the end, so a failure leaves the old `vendor/` as it was.

use std::cell::RefCell;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};
use speedyspec_driver_zip::{Archive, Fetch, FetchError, fetch_and_extract};

use crate::ComposerError;
use crate::autoload::Maps;
use crate::lock::{Lock, Package};
use crate::platform::{Platform, check};

const STAGING: &str = "vendor.speedyspec-new";
const OLD: &str = "vendor.speedyspec-old";

/// What an install did.
#[derive(Debug)]
pub struct Installed {
    /// Platform constraints that could not be read, and so were not checked.
    pub warnings: Vec<String>,
    pub maps: Maps,
}

/// A `Fetch` that also hashes what passes through it.
struct Sha1Fetch<'a> {
    inner: &'a dyn Fetch,
    digest: RefCell<Sha1>,
}

struct Tee<'w, 'd> {
    to: &'w mut dyn Write,
    digest: &'d RefCell<Sha1>,
}

impl Write for Tee<'_, '_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let written = self.to.write(data)?;
        self.digest.borrow_mut().update(&data[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.to.flush()
    }
}

impl Fetch for Sha1Fetch<'_> {
    fn fetch(&self, url: &str, to: &mut dyn Write) -> Result<u64, FetchError> {
        let mut tee = Tee {
            to,
            digest: &self.digest,
        };
        self.inner.fetch(url, &mut tee)
    }
}

fn io(path: &Path, what: &str, e: io::Error) -> ComposerError {
    ComposerError::new(format!("cannot {what} {}: {e}", path.display()))
}

/// Downloads and unpacks one package into `staging/<name>`.
fn place(package: &Package, staging: &Path, fetch: &dyn Fetch) -> Result<(), ComposerError> {
    let named = |message: String| {
        ComposerError::new(format!("{} {}: {message}", package.name, package.version))
    };
    let work = tempfile::Builder::new()
        .prefix(".dist-")
        .tempdir_in(staging)
        .map_err(|e| io(staging, "write in", e))?;
    let hashing = Sha1Fetch {
        inner: fetch,
        digest: RefCell::new(Sha1::new()),
    };
    fetch_and_extract(
        &Archive {
            url: &package.dist.url,
            sha256: None,
            strip_components: 0,
        },
        work.path(),
        &hashing,
    )
    .map_err(|e| named(e.to_string()))?;
    if let Some(expected) = &package.dist.shasum {
        let actual: String = hashing
            .digest
            .into_inner()
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if &actual != expected {
            return Err(named(format!(
                "{}: checksum mismatch (expected sha1 {expected}, got {actual})",
                package.dist.url
            )));
        }
    }
    // A single top-level directory is the package itself, as composer strips it.
    let entries: Vec<PathBuf> = fs::read_dir(work.path())
        .map_err(|e| io(work.path(), "read", e))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    let root = match entries.as_slice() {
        [only]
            if fs::symlink_metadata(only)
                .map(|m| m.is_dir())
                .unwrap_or(false) =>
        {
            only.clone()
        }
        _ => work.path().to_path_buf(),
    };
    let target = staging.join(&package.name);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| io(parent, "create", e))?;
    }
    if root == work.path() {
        let kept = work.keep();
        fs::rename(&kept, &target).map_err(|e| io(&target, "create", e))?;
    } else {
        fs::rename(&root, &target).map_err(|e| io(&target, "create", e))?;
    }
    Ok(())
}

/// Installs `lock`'s packages into `dir/vendor` and writes the autoload maps.
pub fn install(
    dir: &Path,
    lock: &Lock,
    fetch: &dyn Fetch,
    platform: &Platform,
) -> Result<Installed, ComposerError> {
    let warnings = check(&lock.packages, platform)?;
    let staging = dir.join(STAGING);
    let old = dir.join(OLD);
    for leftover in [&staging, &old] {
        if leftover.exists() {
            fs::remove_dir_all(leftover).map_err(|e| io(leftover, "remove", e))?;
        }
    }
    fs::create_dir_all(&staging).map_err(|e| io(&staging, "create", e))?;
    let built = lock
        .packages
        .iter()
        .try_for_each(|package| place(package, &staging, fetch));
    if let Err(error) = built {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    let vendor = dir.join("vendor");
    let had_vendor = vendor.exists();
    if had_vendor {
        fs::rename(&vendor, &old).map_err(|e| io(&vendor, "move aside", e))?;
    }
    if let Err(e) = fs::rename(&staging, &vendor) {
        if had_vendor {
            let _ = fs::rename(&old, &vendor);
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(io(&vendor, "create", e));
    }
    if had_vendor {
        let _ = fs::remove_dir_all(&old);
    }
    let maps = Maps::build(dir, lock);
    maps.write(dir)?;
    Ok(Installed { warnings, maps })
}
