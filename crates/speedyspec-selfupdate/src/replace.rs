//! Putting a release binary in place of the running one.
//!
//! The new binary is written to a temporary file in the executable's own directory and renamed
//! over it, so the swap is atomic and the running process keeps the file it started from. The
//! existing file is never written into: macOS kills a process whose signed binary changes in
//! place, and Linux will not run a file that is still open for writing.

use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use speedyspec_fetch::Fetch;

use crate::release::{Limited, Release, SUMS, UpdateError, parse_sums};

/// The largest `SHA256SUMS` read.
const MAX_SUMS: u64 = 64 * 1024;
/// The largest binary downloaded, whatever the release says.
const MAX_ASSET: u64 = 512 * 1024 * 1024;
/// How long the new binary has to print its version.
const SMOKE_TIMEOUT: Duration = Duration::from_secs(30);
const TEMP_PREFIX: &str = ".speedyspec-update-";

/// The file to replace: the real one behind any symlink, so a link keeps pointing at it.
pub fn target_executable(invoked: &Path) -> Result<PathBuf, UpdateError> {
    invoked.canonicalize().map_err(|e| {
        UpdateError(format!(
            "cannot find the running speedyspec at {}: {e}",
            invoked.display()
        ))
    })
}

/// Why `exe` cannot be replaced by this user, if it cannot.
pub fn writable_problem(exe: &Path, asset: &str) -> Option<String> {
    let dir = exe.parent().unwrap_or(Path::new("."));
    match tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .tempfile_in(dir)
    {
        // Dropping the probe deletes it.
        Ok(_) => None,
        Err(_) => Some(format!(
            "cannot replace {}: {} is not writable by you. Run `sudo speedyspec self update`, or \
             download https://github.com/SpeedySpec/speedyspec-cli/releases/latest/download/{asset} \
             yourself",
            exe.display(),
            dir.display()
        )),
    }
}

/// Hashes everything written through it.
struct Hashing<W> {
    inner: W,
    hasher: Sha256,
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Downloads `asset` of `release`, checks it, runs it once, and renames it over `exe`. Nothing
/// is changed unless every check passes. `progress` is told each stage.
pub fn install(
    fetch: &dyn Fetch,
    release: &Release,
    asset: &str,
    exe: &Path,
    progress: &mut dyn FnMut(&str),
) -> Result<(), UpdateError> {
    if let Some(problem) = writable_problem(exe, asset) {
        return Err(UpdateError(problem));
    }
    let tag = &release.tag;
    let unchanged = |message: String| UpdateError(format!("{message}; nothing was changed"));
    let binary = release.asset(asset).ok_or_else(|| {
        UpdateError(format!(
            "release {tag} has no {asset} yet; if it was published in the last few minutes its \
             files are still being uploaded, so try again shortly"
        ))
    })?;
    let sums_asset = release.asset(SUMS).ok_or_else(|| {
        unchanged(format!(
            "release {tag} has no {SUMS}, so its download cannot be verified"
        ))
    })?;

    let mut sums = Limited::new(Vec::new(), MAX_SUMS);
    fetch
        .fetch(&sums_asset.url, &mut sums)
        .map_err(|e| unchanged(format!("could not download {SUMS} of release {tag}: {e}")))?;
    let sums = String::from_utf8(sums.inner)
        .map_err(|_| unchanged(format!("{SUMS} of release {tag} is not text")))
        .and_then(|text| parse_sums(&text).map_err(unchanged))?;
    let expected = sums
        .get(asset)
        .ok_or_else(|| unchanged(format!("{SUMS} of release {tag} does not list {asset}")))?;

    progress(&format!("downloading {asset} ({tag})"));
    let dir = exe.parent().unwrap_or(Path::new("."));
    let file = tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .tempfile_in(dir)
        .map_err(|e| unchanged(format!("cannot write in {}: {e}", dir.display())))?;
    let mut writer = Hashing {
        inner: Limited::new(file, binary.size.min(MAX_ASSET)),
        hasher: Sha256::new(),
    };
    if let Err(error) = fetch.fetch(&binary.url, &mut writer) {
        return Err(if writer.inner.refused {
            unchanged(format!(
                "{asset} is larger than release {tag} says ({} bytes)",
                binary.size
            ))
        } else {
            unchanged(format!("could not download {asset}: {error}"))
        });
    }

    progress("verifying");
    let actual = hex(&writer.hasher.finalize());
    if &actual != expected {
        return Err(unchanged(format!(
            "the download of {asset} does not match {SUMS} (expected {expected}, got {actual})"
        )));
    }
    let file = writer.inner.inner;
    file.as_file()
        .sync_all()
        .map_err(|e| unchanged(format!("could not write {asset}: {e}")))?;
    // Closing the file before it is run: Linux refuses to run a file open for writing.
    let path = file.into_temp_path();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| unchanged(format!("could not make {asset} executable: {e}")))?;

    let wanted = format!("speedyspec {}", release.version);
    smoke(&path, &wanted)
        .map_err(|reason| unchanged(format!("the downloaded {asset} did not run ({reason})")))?;

    path.persist(exe).map_err(|e| {
        unchanged(format!(
            "could not put the new binary in place at {}: {}",
            exe.display(),
            e.error
        ))
    })
}

/// `ETXTBSY` on Linux and macOS: the file is open for writing somewhere.
const TEXT_FILE_BUSY: i32 = 26;
/// How long a file that is busy that way is waited for.
const BUSY_WAIT: Duration = Duration::from_secs(2);

/// Starts the new binary. This process closed its own write handle before, but a child that
/// another thread forked and has not yet exec'd still holds a copy, and Linux refuses to run a
/// file open for writing; that lasts only until the child execs, so it is tried again.
fn spawn_when_not_busy(path: &Path) -> io::Result<std::process::Child> {
    let started = Instant::now();
    loop {
        let spawned = Command::new(path)
            .args(["--no-plugins", "--version"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Err(error)
                if error.raw_os_error() == Some(TEXT_FILE_BUSY)
                    && started.elapsed() < BUSY_WAIT =>
            {
                std::thread::sleep(Duration::from_millis(25));
            }
            other => return other,
        }
    }
}

/// Runs the new binary with `--no-plugins --version` and checks its first line.
fn smoke(path: &Path, wanted: &str) -> Result<(), String> {
    let mut child = spawn_when_not_busy(path).map_err(|e| e.to_string())?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if started.elapsed() >= SMOKE_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "it did not finish within {}s",
                SMOKE_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut output);
    }
    if !status.success() {
        return Err(match status.code() {
            Some(code) => format!("exit status {code}"),
            None => "it was stopped by a signal".to_owned(),
        });
    }
    let first = output.lines().next().unwrap_or_default();
    if first != wanted {
        return Err(format!("it printed \"{first}\""));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Another process (a child forked by another thread, not yet exec'd) can briefly hold a
    /// copy of the file's write handle. Linux then refuses to run it ("Text file busy").
    #[test]
    fn a_briefly_busy_binary_still_runs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new");
        std::fs::write(&path, "#!/bin/sh\necho \"speedyspec 0.3.0\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let busy = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            drop(busy);
        });
        let result = smoke(&path, "speedyspec 0.3.0");
        release.join().unwrap();
        assert_eq!(result, Ok(()));
    }
}
