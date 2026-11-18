//! Installing several archives under one base directory.
//!
//! Everything is downloaded, verified and unpacked into a staging directory first, so a failure
//! up to that point changes nothing outside it. Only then are the sources moved into place, in
//! the order given.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use speedyspec_driver::{Progress, ZipSource};

use crate::extract::{Archive, ExtractError, fetch_and_extract};
use crate::fetch::Fetch;

/// Where one source ended up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    pub url: String,
    /// As the plugin gave it.
    pub destination: String,
    /// Absolute.
    pub path: PathBuf,
}

/// Why an install did not happen, or did not finish.
#[derive(Debug)]
pub enum InstallError {
    Extract(ExtractError),
    /// A destination already holds files and `--force` was not given.
    Occupied {
        path: PathBuf,
    },
    /// A destination leads, through an existing symbolic link, outside the base directory.
    OutsideBase {
        destination: String,
    },
    Io {
        path: PathBuf,
        message: String,
    },
    /// Only possible with `--force`, where what was replaced cannot be put back.
    Partial {
        placed: Vec<Placement>,
        failed: String,
        message: String,
    },
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Extract(error) => error.fmt(f),
            Self::Occupied { path } => write!(
                f,
                "{} is not empty; use --force to merge into it (files of the same path are \
                 replaced, nothing is deleted)",
                path.display()
            ),
            Self::OutsideBase { destination } => write!(
                f,
                "destination {destination:?} leads outside the base directory through a \
                 symbolic link"
            ),
            Self::Io { path, message } => write!(f, "{}: {message}", path.display()),
            Self::Partial {
                placed,
                failed,
                message,
            } => {
                write!(f, "stopped while placing {failed:?}: {message}; ")?;
                if placed.is_empty() {
                    write!(f, "no destination was completed")
                } else {
                    let done: Vec<&str> = placed.iter().map(|p| p.destination.as_str()).collect();
                    write!(f, "already in place: {}", done.join(", "))
                }
            }
        }
    }
}

impl std::error::Error for InstallError {}

/// Installs `sources` under `base`. See the module documentation for the order of events.
pub fn install_sources(
    sources: &[ZipSource],
    base: &Path,
    force: bool,
    fetch: &dyn Fetch,
    progress: &mut dyn Progress,
) -> Result<Vec<Placement>, InstallError> {
    fs::create_dir_all(base).map_err(|e| io_error(base, &e))?;
    let base = base.canonicalize().map_err(|e| io_error(base, &e))?;

    let mut targets = Vec::with_capacity(sources.len());
    for source in sources {
        let target = resolve_destination(&base, &source.destination).ok_or_else(|| {
            InstallError::OutsideBase {
                destination: source.destination.clone(),
            }
        })?;
        targets.push(target);
    }
    // Judged once, against what is on disk before this install touches anything.
    if !force {
        for target in &targets {
            if holds_files(target) {
                return Err(InstallError::Occupied {
                    path: target.clone(),
                });
            }
        }
    }

    // Removed when dropped, on every path out of this function.
    let staging = tempfile::Builder::new()
        .prefix(".speedyspec-staging-")
        .tempdir_in(&base)
        .map_err(|e| io_error(&base, &e))?;
    let mut staged = Vec::with_capacity(sources.len());
    for (index, source) in sources.iter().enumerate() {
        progress.step(&format!("downloading {}", source.url));
        let into = staging.path().join(index.to_string());
        fs::create_dir(&into).map_err(|e| io_error(&into, &e))?;
        let archive = Archive {
            url: &source.url,
            sha256: source.sha256.as_deref(),
            strip_components: source.strip_components,
        };
        fetch_and_extract(&archive, &into, fetch).map_err(InstallError::Extract)?;
        staged.push(into);
    }

    let mut journal = Journal::default();
    let mut placed = Vec::with_capacity(sources.len());
    for ((source, target), from) in sources.iter().zip(&targets).zip(&staged) {
        progress.step(&format!("placing {}", source.destination));
        let moved = create_directories(target, &mut journal)
            .and_then(|()| merge(from, target, &base, &mut journal));
        if let Err((path, message)) = moved {
            if force {
                return Err(InstallError::Partial {
                    placed,
                    failed: source.destination.clone(),
                    message: format!("{}: {message}", path.display()),
                });
            }
            // Nothing was there before, so everything this run placed can go again.
            journal.undo();
            return Err(InstallError::Io { path, message });
        }
        placed.push(Placement {
            url: source.url.clone(),
            destination: source.destination.clone(),
            path: target.clone(),
        });
    }
    Ok(placed)
}

fn io_error(path: &Path, error: &io::Error) -> InstallError {
    InstallError::Io {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

/// `destination` under the canonical `base`, or `None` if an existing symbolic link on the way
/// leads outside it. Components that do not exist yet are taken literally.
fn resolve_destination(base: &Path, destination: &str) -> Option<PathBuf> {
    let mut current = base.to_path_buf();
    for segment in destination
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
    {
        if segment == ".." {
            return None;
        }
        let next = current.join(segment);
        current = match fs::symlink_metadata(&next) {
            Ok(found) if found.file_type().is_symlink() => {
                let resolved = next.canonicalize().ok()?;
                if !resolved.starts_with(base) {
                    return None;
                }
                resolved
            }
            _ => next,
        };
    }
    Some(current)
}

/// Whether `path` is, or contains anywhere below it, something other than a directory.
fn holds_files(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Err(_) => false,
        Ok(found) if !found.is_dir() => true,
        Ok(_) => fs::read_dir(path)
            .is_ok_and(|entries| entries.flatten().any(|entry| holds_files(&entry.path()))),
    }
}

/// What this run put on disk, so it can be taken away again.
#[derive(Default)]
struct Journal {
    files: Vec<PathBuf>,
    directories: Vec<PathBuf>,
}

impl Journal {
    /// Removes the files this run placed, then the directories it created that are now empty.
    fn undo(&self) {
        for file in &self.files {
            let _ = fs::remove_file(file);
        }
        for directory in self.directories.iter().rev() {
            let _ = fs::remove_dir(directory);
        }
    }
}

type MoveError = (PathBuf, String);

fn move_error(path: &Path, error: &io::Error) -> MoveError {
    (path.to_path_buf(), error.to_string())
}

/// Creates `target` and any missing parents, recording each directory it creates.
fn create_directories(target: &Path, journal: &mut Journal) -> Result<(), MoveError> {
    let mut missing = Vec::new();
    let mut current = target;
    while fs::symlink_metadata(current).is_err() {
        missing.push(current.to_path_buf());
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    for directory in missing.into_iter().rev() {
        fs::create_dir(&directory).map_err(|e| move_error(&directory, &e))?;
        journal.directories.push(directory);
    }
    Ok(())
}

/// Moves everything in `from` into `to`, merging directories and replacing files of the same
/// path. Never writes through a symbolic link that leads outside `base`.
fn merge(from: &Path, to: &Path, base: &Path, journal: &mut Journal) -> Result<(), MoveError> {
    let mut entries: Vec<PathBuf> = fs::read_dir(from)
        .map_err(|e| move_error(from, &e))?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for source in entries {
        let name = source.file_name().expect("read_dir entries have names");
        let target = to.join(name);
        let source_is_directory = fs::symlink_metadata(&source).is_ok_and(|found| found.is_dir());
        let existing = fs::symlink_metadata(&target).ok();

        if !source_is_directory {
            match existing {
                Some(found) if found.is_dir() => {
                    return Err((target, "a directory is in the way of a file".to_owned()));
                }
                // Whatever is there (a file, or a link of any kind) is replaced, not written
                // through.
                Some(_) => fs::remove_file(&target).map_err(|e| move_error(&target, &e))?,
                None => {}
            }
            fs::rename(&source, &target).map_err(|e| move_error(&target, &e))?;
            journal.files.push(target);
            continue;
        }

        let into = match existing {
            None => {
                fs::create_dir(&target).map_err(|e| move_error(&target, &e))?;
                journal.directories.push(target.clone());
                target
            }
            Some(found) if found.is_dir() => target,
            Some(found) if found.file_type().is_symlink() => {
                let resolved = target.canonicalize().map_err(|e| move_error(&target, &e))?;
                if !resolved.starts_with(base) || !resolved.is_dir() {
                    return Err((
                        target,
                        "refusing to write through a symbolic link that leads outside the base \
                         directory"
                            .to_owned(),
                    ));
                }
                resolved
            }
            Some(_) => {
                return Err((target, "a file is in the way of a directory".to_owned()));
            }
        };
        merge(&source, &into, base, journal)?;
    }
    Ok(())
}
