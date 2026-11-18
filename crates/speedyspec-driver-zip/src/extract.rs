//! One archive into one directory: download, verify, unpack, and nothing anywhere else.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use speedyspec_fetch::{Fetch, FetchError};

/// What to download and how to unpack it.
#[derive(Clone, Copy, Debug)]
pub struct Archive<'a> {
    pub url: &'a str,
    /// Lower-case hex; the download is checked against it before anything is unpacked.
    pub sha256: Option<&'a str>,
    /// Leading path segments dropped from every entry.
    pub strip_components: u32,
}

/// How much one archive may take, so a hostile or broken server cannot fill the disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most bytes one download may be.
    pub download_bytes: u64,
    /// The most bytes one archive may unpack to.
    pub unpacked_bytes: u64,
    /// The longest target a symbolic link entry may have.
    pub link_target_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            download_bytes: 4 << 30,
            unpacked_bytes: 16 << 30,
            link_target_bytes: 4096,
        }
    }
}

/// Why an archive was not unpacked.
#[derive(Debug)]
pub enum ExtractError {
    Fetch(FetchError),
    Checksum {
        url: String,
        expected: String,
        actual: String,
    },
    NotAZip {
        url: String,
        message: String,
    },
    /// An entry that would have written outside the target, or cannot be placed.
    Unsafe {
        entry: String,
        problem: String,
    },
    /// The download, or what it unpacks to, is over a [`Limits`] value.
    TooLarge {
        url: String,
        what: &'static str,
        limit: u64,
    },
    /// Nothing was left to unpack (after stripping).
    Empty {
        url: String,
    },
    Io {
        path: PathBuf,
        message: String,
    },
}

impl fmt::Display for ExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fetch(error) => error.fmt(f),
            Self::Checksum {
                url,
                expected,
                actual,
            } => write!(
                f,
                "{url}: checksum mismatch (expected sha256 {expected}, got {actual})"
            ),
            Self::NotAZip { url, message } => {
                write!(f, "{url}: not a usable zip archive: {message}")
            }
            Self::Unsafe { entry, problem } => {
                write!(f, "archive entry {entry:?} was refused: {problem}")
            }
            Self::TooLarge { url, what, limit } => {
                write!(f, "{url}: {what} more than the limit of {limit} bytes")
            }
            Self::Empty { url } => write!(f, "{url}: the archive holds no files to install"),
            Self::Io { path, message } => write!(f, "{}: {message}", path.display()),
        }
    }
}

impl std::error::Error for ExtractError {}

/// Downloads `archive` to a temporary file beside `into`, verifies it, and unpacks it into the
/// directory `into`, which the caller owns. Returns how many files were written. On any error
/// `into` is emptied and the temporary file is gone.
pub fn fetch_and_extract(
    archive: &Archive<'_>,
    into: &Path,
    fetch: &dyn Fetch,
) -> Result<u64, ExtractError> {
    fetch_and_extract_with(archive, into, fetch, &Limits::default())
}

/// [`fetch_and_extract`] with other limits than the defaults.
pub fn fetch_and_extract_with(
    archive: &Archive<'_>,
    into: &Path,
    fetch: &dyn Fetch,
    limits: &Limits,
) -> Result<u64, ExtractError> {
    let result = download(archive, into, fetch, limits)
        .and_then(|file| unpack(archive, file, into, limits))
        .and_then(|written| {
            let root = into.canonicalize().map_err(|e| ExtractError::Io {
                path: into.to_path_buf(),
                message: e.to_string(),
            })?;
            verify_links(&root, &root).map(|()| written)
        });
    if result.is_err() {
        empty_directory(into);
    }
    result
}

/// Fetches the archive into an unnamed-on-drop temporary file and checks its digest.
fn download(
    archive: &Archive<'_>,
    into: &Path,
    fetch: &dyn Fetch,
    limits: &Limits,
) -> Result<File, ExtractError> {
    let beside = into.parent().unwrap_or(into);
    let io_error = |e: io::Error| ExtractError::Io {
        path: beside.to_path_buf(),
        message: format!("cannot create a temporary file: {e}"),
    };
    // Unlinked as soon as it is created, so it can never be left behind.
    let mut file = tempfile::tempfile_in(beside).map_err(io_error)?;
    let mut hashing = Hashing {
        inner: &mut file,
        digest: Sha256::new(),
        remaining: limits.download_bytes,
        over: false,
    };
    let fetched = fetch.fetch(archive.url, &mut hashing);
    if hashing.over {
        return Err(ExtractError::TooLarge {
            url: archive.url.to_owned(),
            what: "the download is",
            limit: limits.download_bytes,
        });
    }
    fetched.map_err(ExtractError::Fetch)?;
    let actual: String = hashing
        .digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if let Some(expected) = archive.sha256
        && !expected.eq_ignore_ascii_case(&actual)
    {
        return Err(ExtractError::Checksum {
            url: archive.url.to_owned(),
            expected: expected.to_owned(),
            actual,
        });
    }
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    Ok(file)
}

/// Writes through to a file while hashing what passes.
struct Hashing<'a> {
    inner: &'a mut File,
    digest: Sha256,
    /// Bytes still allowed.
    remaining: u64,
    /// Set when the download tried to go past the limit.
    over: bool,
}

impl Write for Hashing<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() as u64 > self.remaining {
            self.over = true;
            return Err(io::Error::other("the download is larger than allowed"));
        }
        self.remaining -= buffer.len() as u64;
        let written = self.inner.write(buffer)?;
        self.digest.update(&buffer[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn unpack(
    archive: &Archive<'_>,
    file: File,
    into: &Path,
    limits: &Limits,
) -> Result<u64, ExtractError> {
    let mut room = limits.unpacked_bytes;
    let not_a_zip = |message: String| ExtractError::NotAZip {
        url: archive.url.to_owned(),
        message,
    };
    let root = into.canonicalize().map_err(|e| ExtractError::Io {
        path: into.to_path_buf(),
        message: e.to_string(),
    })?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| not_a_zip(e.to_string()))?;
    let mut written = 0;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|e| not_a_zip(e.to_string()))?;
        let shown = String::from_utf8_lossy(entry.name_raw()).into_owned();
        let refuse = |problem: &str| ExtractError::Unsafe {
            entry: shown.clone(),
            problem: problem.to_owned(),
        };
        let name = std::str::from_utf8(entry.name_raw())
            .map_err(|_| refuse("the name is not valid UTF-8"))?;
        let segments = segments(name).map_err(refuse)?;
        let Some(kept) = segments.get(archive.strip_components as usize..) else {
            continue;
        };
        if kept.is_empty() {
            continue;
        }
        let (parents, last) = kept.split_at(kept.len() - 1);
        let directory = descend(&root, parents).map_err(refuse)?;
        let target = directory.join(last[0]);
        let existing = fs::symlink_metadata(&target).ok();
        let io_error = |e: io::Error| ExtractError::Io {
            path: target.clone(),
            message: e.to_string(),
        };

        if entry.is_dir() {
            match existing {
                Some(found) if found.is_dir() => {}
                Some(_) => return Err(refuse("a file already has this directory's name")),
                None => fs::create_dir(&target).map_err(io_error)?,
            }
            continue;
        }
        if existing.as_ref().is_some_and(fs::Metadata::is_dir) {
            return Err(refuse("a directory already has this file's name"));
        }
        if existing.is_some() {
            // A later entry of the same name replaces the earlier one. Never written through:
            // the old name is removed, whatever it was.
            fs::remove_file(&target).map_err(io_error)?;
        }
        if entry.is_symlink() {
            let mut link = String::new();
            (&mut entry)
                .take(limits.link_target_bytes + 1)
                .read_to_string(&mut link)
                .map_err(|_| refuse("the link target is not valid UTF-8"))?;
            if link.len() as u64 > limits.link_target_bytes {
                return Err(refuse("the link target is too long"));
            }
            if !stays_inside(&root, &directory, &link) {
                return Err(refuse("a symbolic link pointing outside the destination"));
            }
            make_symlink(&link, &target).map_err(io_error)?;
        } else {
            let mode = entry.unix_mode();
            let mut out = File::create_new(&target).map_err(io_error)?;
            // One byte more than there is room for shows that the entry does not fit.
            let copied = io::copy(&mut (&mut entry).take(room.saturating_add(1)), &mut out)
                .map_err(|e| not_a_zip(e.to_string()))?;
            if copied > room {
                return Err(ExtractError::TooLarge {
                    url: archive.url.to_owned(),
                    what: "the archive unpacks to",
                    limit: limits.unpacked_bytes,
                });
            }
            room -= copied;
            set_mode(&out, mode).map_err(io_error)?;
        }
        written += 1;
    }
    if written == 0 {
        return Err(ExtractError::Empty {
            url: archive.url.to_owned(),
        });
    }
    Ok(written)
}

/// Checks every symbolic link under `directory` by resolving it for real: each must lead to
/// something that exists inside `root`. Reading a link's target as text is not enough, because
/// a chain of links (`a -> .`, `up -> a/..`) can climb out one harmless-looking step at a time.
fn verify_links(root: &Path, directory: &Path) -> Result<(), ExtractError> {
    let entries = fs::read_dir(directory).map_err(|e| ExtractError::Io {
        path: directory.to_path_buf(),
        message: e.to_string(),
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(found) = fs::symlink_metadata(&path) else {
            continue;
        };
        if found.is_dir() {
            verify_links(root, &path)?;
        } else if found.file_type().is_symlink() {
            let shown = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let refuse = |problem: &str| ExtractError::Unsafe {
                entry: shown.clone(),
                problem: problem.to_owned(),
            };
            let resolved = path
                .canonicalize()
                .map_err(|_| refuse("a symbolic link that leads nowhere"))?;
            if !resolved.starts_with(root) {
                return Err(refuse("a symbolic link that leads outside the destination"));
            }
        }
    }
    Ok(())
}

/// An entry name as path segments, or why it cannot be one. A trailing slash (a directory
/// entry) is allowed; nothing else odd is.
fn segments(name: &str) -> Result<Vec<&str>, &'static str> {
    if name.contains('\0') {
        return Err("the name contains a NUL character");
    }
    if name.contains('\\') {
        return Err("the name contains a backslash");
    }
    if name.starts_with('/') {
        return Err("the name is an absolute path");
    }
    if name.as_bytes().get(1) == Some(&b':') {
        return Err("the name starts with a drive letter");
    }
    let trimmed = name.strip_suffix('/').unwrap_or(name);
    let segments: Vec<&str> = trimmed.split('/').collect();
    for segment in &segments {
        match *segment {
            "" => return Err("the name has an empty path segment"),
            ".." => return Err("the name has a \"..\" segment"),
            "." => return Err("the name has a \".\" segment"),
            _ => {}
        }
    }
    Ok(segments)
}

/// Walks `parents` down from `root`, creating directories as needed, and returns the canonical
/// directory reached. Refuses to pass through anything that leads outside `root`.
fn descend(root: &Path, parents: &[&str]) -> Result<PathBuf, &'static str> {
    let mut current = root.to_path_buf();
    for segment in parents {
        let next = current.join(segment);
        match fs::symlink_metadata(&next) {
            Ok(found) if found.is_dir() => current = next,
            Ok(found) if found.file_type().is_symlink() => {
                let resolved = next
                    .canonicalize()
                    .map_err(|_| "the path passes through a broken symbolic link")?;
                if !resolved.starts_with(root) {
                    return Err(
                        "the path passes through a symbolic link leading outside the destination",
                    );
                }
                if !resolved.is_dir() {
                    return Err("the path passes through a file");
                }
                current = resolved;
            }
            Ok(_) => return Err("the path passes through a file"),
            Err(_) => {
                fs::create_dir(&next).map_err(|_| "a directory could not be created")?;
                current = next;
            }
        }
    }
    Ok(current)
}

/// Whether a link at `directory` (inside `root`) with this target resolves, taken literally,
/// to somewhere inside `root`.
fn stays_inside(root: &Path, directory: &Path, link: &str) -> bool {
    let target = Path::new(link);
    if target.is_absolute() || link.contains('\0') || link.contains('\\') {
        return false;
    }
    let mut resolved = directory.to_path_buf();
    for component in target.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if resolved == root || !resolved.pop() {
                    return false;
                }
            }
            Component::Normal(part) => resolved.push(part),
            Component::RootDir | Component::Prefix(_) => return false,
        }
        if !resolved.starts_with(root) {
            return false;
        }
    }
    true
}

#[cfg(unix)]
fn make_symlink(link: &str, at: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(link, at)
}

#[cfg(not(unix))]
fn make_symlink(_link: &str, _at: &Path) -> io::Result<()> {
    Err(io::Error::other(
        "symbolic links are not supported on this platform",
    ))
}

/// Applies the entry's permission bits, never more than `rwxrwxrwx` (no setuid, setgid, sticky).
#[cfg(unix)]
fn set_mode(file: &File, mode: Option<u32>) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    match mode {
        Some(mode) => file.set_permissions(fs::Permissions::from_mode(mode & 0o777)),
        None => Ok(()),
    }
}

#[cfg(not(unix))]
fn set_mode(_file: &File, _mode: Option<u32>) -> io::Result<()> {
    Ok(())
}

/// Removes everything inside `directory`, never following a symbolic link. Best effort: this
/// runs while reporting another error.
fn empty_directory(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_real_directory = fs::symlink_metadata(&path).is_ok_and(|found| found.is_dir());
        let _ = if is_real_directory {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
    }
}
