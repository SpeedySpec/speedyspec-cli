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

/// A `Fetch` that gives a file already downloaded, whatever the URL.
struct FileFetch(PathBuf);

impl Fetch for FileFetch {
    fn fetch(&self, url: &str, to: &mut dyn Write) -> Result<u64, FetchError> {
        let mut file = fs::File::open(&self.0).map_err(|e| FetchError::Io {
            url: url.to_owned(),
            message: e.to_string(),
        })?;
        io::copy(&mut file, to).map_err(|e| FetchError::Io {
            url: url.to_owned(),
            message: e.to_string(),
        })
    }
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
    // Downloaded first and its checksum checked, so nothing from an archive that does not match is ever unpacked.
    let download = work.path().join(".download");
    let hashing = Sha1Fetch {
        inner: fetch,
        digest: RefCell::new(Sha1::new()),
    };
    {
        let mut file = fs::File::create(&download).map_err(|e| io(&download, "create", e))?;
        hashing
            .fetch(&package.dist.url, &mut file)
            .map_err(|e| named(e.to_string()))?;
    }
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
    let unpacked = work.path().join("unpacked");
    fs::create_dir(&unpacked).map_err(|e| io(&unpacked, "create", e))?;
    fetch_and_extract(
        &Archive {
            url: &package.dist.url,
            sha256: None,
            strip_components: 0,
        },
        &unpacked,
        &FileFetch(download.clone()),
    )
    .map_err(|e| named(e.to_string()))?;
    let _ = fs::remove_file(&download);
    // A single top-level directory is the package itself, as composer strips it.
    let entries: Vec<PathBuf> = fs::read_dir(&unpacked)
        .map_err(|e| io(&unpacked, "read", e))?
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
        _ => unpacked.clone(),
    };
    let target = staging.join(&package.name);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| io(parent, "create", e))?;
    }
    fs::rename(&root, &target).map_err(|e| io(&target, "create", e))?;
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
    let vendor = dir.join("vendor");
    // A crash between moving vendor/ aside and moving the new one in leaves the old one only here.
    if old.exists() {
        if vendor.exists() {
            fs::remove_dir_all(&old).map_err(|e| io(&old, "remove", e))?;
        } else {
            fs::rename(&old, &vendor).map_err(|e| io(&old, "put back", e))?;
        }
    }
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|e| io(&staging, "remove", e))?;
    }
    fs::create_dir_all(&staging).map_err(|e| io(&staging, "create", e))?;
    let built = lock
        .packages
        .iter()
        .try_for_each(|package| place(package, &staging, fetch))
        .and_then(|()| {
            // The maps go in with the packages, so vendor/ is never without them.
            let maps = Maps::build_in(dir, STAGING, lock);
            maps.write_to(&staging.join("speedyspec-autoload.json"))?;
            Ok(maps)
        });
    let maps = match built {
        Ok(maps) => maps,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
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
    Ok(Installed { warnings, maps })
}
