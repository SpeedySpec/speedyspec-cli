//! An install's plugins and themes on disk. Where they live, and what counts as one, comes from the app's plugin
//! (`speedyspec_app_extensions`, an `ExtensionLayout` whose `inspect` closure reads an entry); where to get them comes
//! from source plugins (`speedyspec_extension_sources`, `ExtensionSource`s whose `resolve` closure gives an
//! `ExtensionArchive`). The CLI does the file work: it downloads with the zip driver's checked extraction into a
//! temporary directory beside the others and renames directories into place, so a failure leaves what was there.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use speedyspec_driver::{
    Extension, ExtensionArchive, ExtensionLayout, Install, Kind, ObjectReader, decode_extension,
    decode_extension_archive, decode_extension_layout, decode_extension_sources,
};
use speedyspec_driver_zip::{Archive, HttpFetch, fetch_and_extract};
use speedyspec_hooks::Value;
use speedyspec_php::Output;

use crate::app_target::{Target, ask_with};
use crate::boot::{PhpReader, Runtime};

const HOOK_LAYOUT: &str = "speedyspec_app_extensions";
const HOOK_SOURCES: &str = "speedyspec_extension_sources";
/// Entries the CLI makes while it works; never plugins or themes.
const WORK_PREFIX: &str = ".speedyspec-";

/// How version `a` compares to `b` by their numeric parts (`5.3.10` after `5.3.9`, `2.0` the same as `2.0.0`); `None`
/// when either is not plain numbers, so an unusual version never blocks an update.
fn compare(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let parts = |v: &str| -> Option<Vec<u64>> { v.split('.').map(|p| p.parse().ok()).collect() };
    let (mut a, mut b) = (parts(a)?, parts(b)?);
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    Some(a.cmp(&b))
}

fn older(a: &str, b: &str) -> bool {
    compare(a, b) == Some(std::cmp::Ordering::Less)
}

fn same(a: &str, b: &str) -> bool {
    a == b || compare(a, b) == Some(std::cmp::Ordering::Equal)
}

/// One kind of extension in one install.
pub struct Shelf<'a> {
    runtime: &'a Runtime,
    install: Install,
    layout: ExtensionLayout,
    pub kind: Kind,
    /// Absolute.
    dir: PathBuf,
}

/// What `update` did.
pub enum Updated {
    AlreadyAt(String),
    /// The installed version is newer than the source's latest, so nothing was changed.
    Newer {
        installed: String,
        latest: String,
    },
    From {
        old: Option<String>,
        new: String,
    },
}

fn plural(kind: Kind) -> &'static str {
    match kind {
        Kind::Plugin => "plugins",
        Kind::Theme => "themes",
    }
}

/// Calls a plugin's closure with its output on stderr, so it never mixes with the command's own.
fn call(runtime: &Runtime, closure: &Value, args: &[Value]) -> Result<Value, String> {
    runtime.php.redirect_output(Output::Stderr);
    let result = runtime.php.call(closure, args).map_err(|e| e.to_string());
    runtime.php.redirect_output(Output::Stdout);
    result
}

impl<'a> Shelf<'a> {
    /// The `kind` shelf of install `name`.
    pub fn open(
        runtime: &'a Runtime,
        name: &str,
        cluster: Option<&BTreeMap<String, String>>,
        kind: Kind,
    ) -> Result<Shelf<'a>, String> {
        let target = Target::find(name, cluster)?;
        if target.cluster.is_some() {
            crate::app_target::workload(runtime, &target)?;
            return Err(format!(
                "{} commands on Kubernetes installs are not done yet",
                kind.as_str()
            ));
        }
        let install = target.install;
        let root = install.root.clone().ok_or_else(|| {
            format!(
                "install {} is not on this machine; its files are where the {} driver put them",
                install.name, install.driver
            )
        })?;
        let check = |value: &Value, reader: &dyn ObjectReader| {
            decode_extension_layout(value, reader).map(drop)
        };
        let answer = ask_with(runtime, &install, &[], HOOK_LAYOUT, Value::Null, check)?;
        if matches!(answer, Value::Null) {
            return Err(format!(
                "no plugin says where {}'s plugins and themes are; install the plugin for that app",
                install.app
            ));
        }
        let layout = decode_extension_layout(&answer, &PhpReader(&runtime.php))
            .map_err(|e| e.to_string())?;
        let relative = layout
            .directories
            .get(&kind)
            .ok_or_else(|| format!("{} has no {}", install.app, plural(kind)))?;
        let dir = root.join(relative);
        Ok(Shelf {
            runtime,
            install,
            layout,
            kind,
            dir,
        })
    }

    fn kind_name(&self) -> &'static str {
        self.kind.as_str()
    }

    /// What `inspect` says about one entry; `None` when it is not a plugin or theme.
    fn inspect(&self, path: &Path, expected: &str) -> Result<Option<Extension>, String> {
        let args = [
            Value::Str(self.kind_name().to_owned()),
            Value::Str(path.to_string_lossy().into_owned()),
        ];
        let answer = call(self.runtime, &self.layout.inspect, &args)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        if matches!(answer, Value::Null) {
            return Ok(None);
        }
        let found = decode_extension(&answer, &PhpReader(&self.runtime.php))
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        if found.kind != self.kind || found.slug != expected {
            return Err(format!(
                "reading {}: the app's plugin answered with the {} {}, not the {} {expected}",
                path.display(),
                found.kind.as_str(),
                found.slug,
                self.kind_name()
            ));
        }
        Ok(Some(found))
    }

    /// The installed ones, sorted by slug.
    pub fn list(&self) -> Result<Vec<Extension>, String> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("cannot read {}: {e}", self.dir.display())),
        };
        let mut found = Vec::new();
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(WORK_PREFIX) {
                continue;
            }
            let path = entry.path();
            let slug = if path.is_dir() {
                name
            } else if let Some(stem) = name.strip_suffix(".php") {
                stem.to_owned()
            } else {
                continue;
            };
            if let Some(extension) = self.inspect(&path, &slug)? {
                found.push(extension);
            }
        }
        found.sort_by(|a, b| a.slug.cmp(&b.slug));
        Ok(found)
    }

    /// Where `slug` is installed: its directory, or a single file `<slug>.php`.
    fn installed_path(&self, slug: &str) -> Option<PathBuf> {
        let dir = self.dir.join(slug);
        if dir.is_dir() {
            return Some(dir);
        }
        let file = self.dir.join(format!("{slug}.php"));
        file.is_file().then_some(file)
    }

    /// The first source's archive of `slug`; `Ok(None)` when no source has it.
    pub fn resolve(
        &self,
        slug: &str,
        version: Option<&str>,
    ) -> Result<Option<ExtensionArchive>, String> {
        let check = |value: &Value, reader: &dyn ObjectReader| {
            decode_extension_sources(value, reader).map(drop)
        };
        let answer = ask_with(
            self.runtime,
            &self.install,
            &[],
            HOOK_SOURCES,
            Value::Array(Vec::new()),
            check,
        )?;
        let reader = PhpReader(&self.runtime.php);
        let sources = decode_extension_sources(&answer, &reader).map_err(|e| e.to_string())?;
        let args = [
            Value::Str(self.kind_name().to_owned()),
            Value::Str(slug.to_owned()),
            version.map_or(Value::Null, |v| Value::Str(v.to_owned())),
        ];
        for source in sources {
            let answer = call(self.runtime, &source.resolve, &args)
                .map_err(|e| format!("the source {}: {e}", source.name))?;
            if matches!(answer, Value::Null) {
                continue;
            }
            let archive = decode_extension_archive(&answer, &reader)
                .map_err(|e| format!("the source {}: {e}", source.name))?;
            if archive.kind != self.kind || archive.slug != slug {
                return Err(format!(
                    "the source {} answered with the {} {}, not the {} {slug}",
                    source.name,
                    archive.kind.as_str(),
                    archive.slug,
                    self.kind_name()
                ));
            }
            if let Some(asked) = version
                && archive.version != asked
            {
                return Err(format!(
                    "the source {} answered with version {} of {slug}, not {asked}",
                    source.name, archive.version
                ));
            }
            return Ok(Some(archive));
        }
        Ok(None)
    }

    fn resolve_or_say(
        &self,
        slug: &str,
        version: Option<&str>,
    ) -> Result<ExtensionArchive, String> {
        self.resolve(slug, version)?.ok_or_else(|| {
            let at = version.map_or_else(String::new, |v| format!(" at version {v}"));
            format!("no source has the {} {slug}{at}", self.kind_name())
        })
    }

    /// Downloads and unpacks `archive` into a fresh work directory beside the others. Gives back the work directory
    /// (removed when dropped) and the one top-level directory inside it.
    fn unpack(&self, archive: &ExtensionArchive) -> Result<(tempfile::TempDir, PathBuf), String> {
        fs::create_dir_all(&self.dir)
            .map_err(|e| format!("cannot create {}: {e}", self.dir.display()))?;
        let work = tempfile::Builder::new()
            .prefix(WORK_PREFIX)
            .tempdir_in(&self.dir)
            .map_err(|e| format!("cannot write in {}: {e}", self.dir.display()))?;
        fetch_and_extract(
            &Archive {
                url: &archive.url,
                sha256: archive.sha256.as_deref(),
                strip_components: 0,
            },
            work.path(),
            &HttpFetch::new(),
        )
        .map_err(|e| e.to_string())?;
        let entries: Vec<PathBuf> = fs::read_dir(work.path())
            .map_err(|e| format!("cannot read {}: {e}", work.path().display()))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect();
        let top = match entries.as_slice() {
            [only]
                if fs::symlink_metadata(only)
                    .map(|m| m.is_dir())
                    .unwrap_or(false) =>
            {
                only.clone()
            }
            _ => {
                return Err(format!(
                    "{}: the archive of the {} {} must hold exactly one top-level directory",
                    archive.url,
                    self.kind_name(),
                    archive.slug
                ));
            }
        };
        Ok((work, top))
    }

    /// Installs `slug` (the latest, or `version`) and gives back the version installed.
    pub fn install(&self, slug: &str, version: Option<&str>) -> Result<String, String> {
        if self.installed_path(slug).is_some() {
            return Err(format!(
                "the {} {slug} is already installed; use update",
                self.kind_name()
            ));
        }
        let archive = self.resolve_or_say(slug, version)?;
        let (_work, top) = self.unpack(&archive)?;
        let target = self.dir.join(slug);
        fs::rename(&top, &target)
            .map_err(|e| format!("cannot move it to {}: {e}", target.display()))?;
        Ok(archive.version)
    }

    /// Where `slug` is installed and what the app's plugin says it is; an entry the plugin does not recognize (say
    /// WordPress's own `index.php` in the plugins directory) is not one.
    fn recognized(&self, slug: &str) -> Result<(PathBuf, Extension), String> {
        let missing = || {
            format!(
                "no {} {slug} in install {}",
                self.kind_name(),
                self.install.name
            )
        };
        let path = self.installed_path(slug).ok_or_else(missing)?;
        let found = self.inspect(&path, slug)?.ok_or_else(missing)?;
        Ok((path, found))
    }

    /// Updates `slug` to the latest version, leaving the old one in place if anything fails.
    pub fn update(&self, slug: &str) -> Result<Updated, String> {
        self.recognized(slug)?;
        let archive = self.resolve_or_say(slug, None)?;
        match self.update_to(slug, archive)? {
            Updated::Newer { installed, latest } => Err(format!(
                "the source's latest {slug} is {latest}, older than the {installed} installed; it is left as it is \
                 (install a version with {slug}@<version> after deleting it, to go back)"
            )),
            updated => Ok(updated),
        }
    }

    /// Updates `slug` to `archive`, which a source already gave. Never goes back to an older version.
    pub fn update_to(&self, slug: &str, archive: ExtensionArchive) -> Result<Updated, String> {
        let (old_path, found) = self.recognized(slug)?;
        let current = found.version;
        if let Some(current) = &current {
            if same(current, &archive.version) {
                return Ok(Updated::AlreadyAt(current.clone()));
            }
            if older(&archive.version, current) {
                return Ok(Updated::Newer {
                    installed: current.clone(),
                    latest: archive.version,
                });
            }
        }
        let (work, top) = self.unpack(&archive)?;
        let aside = work.path().join("old");
        fs::rename(&old_path, &aside)
            .map_err(|e| format!("cannot move {} aside: {e}", old_path.display()))?;
        let target = self.dir.join(slug);
        if let Err(e) = fs::rename(&top, &target) {
            // Put the old one back where it was.
            let _ = fs::rename(&aside, &old_path);
            return Err(format!("cannot move it to {}: {e}", target.display()));
        }
        // The old one goes with the work directory.
        Ok(Updated::From {
            old: current,
            new: archive.version,
        })
    }

    /// Removes `slug`'s files.
    pub fn delete(&self, slug: &str) -> Result<(), String> {
        let (path, _) = self.recognized(slug)?;
        let removed = if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        removed.map_err(|e| format!("cannot remove {}: {e}", path.display()))
    }

    pub fn install_name(&self) -> &str {
        &self.install.name
    }
}

#[cfg(test)]
mod tests {
    use super::{older, same};

    #[test]
    fn versions() {
        assert!(older("1.5", "2.0"));
        assert!(older("5.3.9", "5.3.10"));
        assert!(!older("5.3.10", "5.3.9"));
        assert!(!older("2.0", "2.0.0"));
        assert!(!older("trunk", "2.0"));
        assert!(same("2.0", "2.0.0") && same("trunk", "trunk") && !same("trunk", "2.0"));
    }
}
