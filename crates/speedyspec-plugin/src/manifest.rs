use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use semver::{Version, VersionReq};
use serde_json::Value;

/// File name every plugin directory must contain.
pub const MANIFEST_FILE: &str = "speedyspec-plugin.json";

/// A validated plugin manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub name: String,
    pub version: Version,
    pub contracts: VersionReq,
    /// The contracts requirement exactly as written, kept for error messages.
    pub contracts_raw: String,
    /// Entry file as written in the manifest, relative to the plugin directory.
    pub entry: PathBuf,
}

/// A plugin found on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct Plugin {
    pub manifest: Manifest,
    pub dir: PathBuf,
    /// The entry file's absolute, canonical path, as validated at discovery to be a regular
    /// file inside `dir`.
    pub entry: PathBuf,
}

impl Plugin {
    /// The entry file validated at discovery. It is never resolved again, so what is loaded
    /// is exactly what was checked; if the file has gone since, loading it fails.
    pub fn entry_path(&self) -> &Path {
        &self.entry
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PluginError {
    Manifest {
        dir: PathBuf,
        problem: String,
    },
    Contracts {
        name: String,
        needs: String,
        bundled: String,
    },
    Load {
        name: String,
        problem: String,
    },
    /// Two plugins with the same name in one directory; the first by path order is kept.
    Duplicate {
        name: String,
        kept: PathBuf,
        ignored: PathBuf,
    },
    /// A directory given explicitly (`--plugin-dir`) that cannot be searched.
    Directory {
        dir: PathBuf,
        problem: String,
    },
    /// Not a plugin name at all, so it is never looked for on disk.
    InvalidName {
        name: String,
    },
    /// No plugin of this name is installed in the user plugin directory.
    NotInstalled {
        name: String,
        dir: PathBuf,
    },
}

impl PluginError {
    /// The plugin name where known, otherwise the directory.
    pub fn plugin_label(&self) -> String {
        match self {
            PluginError::Manifest { dir, .. } | PluginError::Directory { dir, .. } => {
                dir.display().to_string()
            }
            PluginError::Contracts { name, .. }
            | PluginError::Load { name, .. }
            | PluginError::Duplicate { name, .. }
            | PluginError::NotInstalled { name, .. }
            | PluginError::InvalidName { name } => name.clone(),
        }
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PluginError::Manifest { dir, problem } => write!(f, "{}: {problem}", dir.display()),
            PluginError::Contracts { needs, bundled, .. } => {
                write!(f, "needs contracts {needs}, this CLI bundles {bundled}")
            }
            PluginError::Load { problem, .. } => write!(f, "{problem}"),
            PluginError::Duplicate { kept, ignored, .. } => write!(
                f,
                "the plugin in {} has the same name as the one in {}, which is loaded instead",
                ignored.display(),
                kept.display()
            ),
            PluginError::Directory { dir, problem } => {
                write!(f, "plugin directory {} {problem}", dir.display())
            }
            PluginError::InvalidName { name } => write!(
                f,
                "\"{name}\" is not a plugin name (vendor/package in lowercase letters, digits and hyphens)"
            ),
            PluginError::NotInstalled { name, dir } => {
                write!(f, "{name} is not installed in {}", dir.display())
            }
        }
    }
}

impl std::error::Error for PluginError {}

/// Whether `name` is a plugin name: `vendor/package` in lowercase letters, digits and hyphens.
/// Nothing else may ever be turned into a path under the plugins directory.
pub fn valid_name(name: &str) -> bool {
    let segment = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    match name.split_once('/') {
        Some((vendor, package)) => segment(vendor) && segment(package),
        None => false,
    }
}

fn string_field<'a>(obj: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a str, String> {
    match obj.get(key) {
        None => Err(format!("missing field \"{key}\"")),
        Some(Value::String(s)) => Ok(s),
        Some(_) => Err(format!("field \"{key}\" must be a string")),
    }
}

/// Resolves `entry` and proves it is a regular file inside `dir`, returning the entry as written
/// and its canonical path. Canonicalising both sides resolves `..` and symlinks (of the file or
/// of any directory on the way), so none of them can be used to point outside the plugin.
fn check_entry(dir: &Path, entry: &str) -> Result<(PathBuf, PathBuf), String> {
    if entry.is_empty() {
        return Err("field \"entry\" must not be empty".into());
    }
    let rel = Path::new(entry);
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
    {
        return Err("field \"entry\" must be a relative path".into());
    }
    let base = dir
        .canonicalize()
        .map_err(|e| format!("cannot resolve plugin directory: {e}"))?;
    let resolved = base
        .join(rel)
        .canonicalize()
        .map_err(|_| format!("entry file \"{entry}\" does not exist"))?;
    if !resolved.starts_with(&base) {
        return Err(format!(
            "field \"entry\" resolves outside the plugin directory: \"{entry}\""
        ));
    }
    if !resolved.is_file() {
        return Err(format!("entry \"{entry}\" is not a regular file"));
    }
    Ok((rel.to_path_buf(), resolved))
}

/// Reads and validates `<dir>/speedyspec-plugin.json`.
pub fn parse_manifest(dir: &Path) -> Result<Manifest, PluginError> {
    read_plugin(dir).map(|plugin| plugin.manifest)
}

/// Reads and validates the plugin in `dir`, keeping the entry path it validated.
pub(crate) fn read_plugin(dir: &Path) -> Result<Plugin, PluginError> {
    let fail = |problem: String| PluginError::Manifest {
        dir: dir.to_path_buf(),
        problem,
    };
    let text = fs::read_to_string(dir.join(MANIFEST_FILE))
        .map_err(|e| fail(format!("cannot read {MANIFEST_FILE}: {e}")))?;
    let value: Value =
        serde_json::from_str(&text).map_err(|e| fail(format!("invalid JSON: {e}")))?;
    let obj = value
        .as_object()
        .ok_or_else(|| fail("manifest must be a JSON object".into()))?;

    let name = string_field(obj, "name").map_err(fail)?;
    if !valid_name(name) {
        return Err(fail(format!(
            "field \"name\" must match ^[a-z0-9-]+/[a-z0-9-]+$, got \"{name}\""
        )));
    }
    let version_raw = string_field(obj, "version").map_err(fail)?;
    let version = Version::parse(version_raw).map_err(|e| {
        fail(format!(
            "field \"version\" is not a valid semver version: {e}"
        ))
    })?;
    let contracts_raw = string_field(obj, "contracts").map_err(fail)?;
    let contracts = VersionReq::parse(contracts_raw).map_err(|e| {
        fail(format!(
            "field \"contracts\" is not a valid version requirement: {e}"
        ))
    })?;
    let entry_raw = string_field(obj, "entry").map_err(fail)?;
    let (entry, resolved) = check_entry(dir, entry_raw).map_err(fail)?;

    Ok(Plugin {
        manifest: Manifest {
            name: name.to_string(),
            version,
            contracts,
            contracts_raw: contracts_raw.to_string(),
            entry,
        },
        dir: dir.to_path_buf(),
        entry: resolved,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    pub(crate) fn write_plugin(dir: &Path, name: &str, entry: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join(MANIFEST_FILE),
            format!(
                r#"{{"name":"{name}","version":"1.2.3","contracts":"^1.0","entry":"{entry}"}}"#
            ),
        )
        .unwrap();
        let entry_file = dir.join(entry);
        if let Some(parent) = entry_file.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(entry_file, "<?php\n").unwrap();
    }

    fn problem(err: PluginError) -> String {
        match err {
            PluginError::Manifest { problem, .. } => problem,
            other => panic!("expected Manifest error, got {other:?}"),
        }
    }

    fn write_raw(dir: &Path, json: &str) {
        fs::write(dir.join(MANIFEST_FILE), json).unwrap();
    }

    #[test]
    fn parses_a_valid_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "acme/hello", "src/main.php");
        let m = parse_manifest(tmp.path()).unwrap();
        assert_eq!(m.name, "acme/hello");
        assert_eq!(m.version, semver::Version::new(1, 2, 3));
        assert_eq!(m.contracts, semver::VersionReq::parse("^1.0").unwrap());
        assert_eq!(m.contracts_raw, "^1.0");
        assert_eq!(m.entry, Path::new("src/main.php"));
        let plugin = read_plugin(tmp.path()).unwrap();
        assert_eq!(
            plugin.entry_path(),
            tmp.path().join("src/main.php").canonicalize().unwrap()
        );
        // The path checked is the path kept: it is not resolved again later.
        fs::remove_file(tmp.path().join("src/main.php")).unwrap();
        assert_eq!(
            plugin.entry_path(),
            tmp.path()
                .canonicalize()
                .unwrap()
                .join("src")
                .join("main.php")
        );
    }

    #[test]
    fn ignores_unknown_fields() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "acme/hello", "main.php");
        write_raw(
            tmp.path(),
            r#"{"name":"acme/hello","version":"1.0.0","contracts":"^1","entry":"main.php","extra":[1]}"#,
        );
        assert!(parse_manifest(tmp.path()).is_ok());
    }

    #[test]
    fn rejects_missing_field() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "acme/hello", "main.php");
        write_raw(
            tmp.path(),
            r#"{"name":"acme/hello","version":"1.0.0","contracts":"^1"}"#,
        );
        assert!(problem(parse_manifest(tmp.path()).unwrap_err()).contains("entry"));
    }

    #[test]
    fn rejects_bad_name() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "WordPress", "main.php");
        assert!(problem(parse_manifest(tmp.path()).unwrap_err()).contains("name"));
        for bad in ["a/b/c", "/b", "a/", "A/b", "a b/c", ""] {
            write_plugin(tmp.path(), bad, "main.php");
            assert!(
                problem(parse_manifest(tmp.path()).unwrap_err()).contains("name"),
                "{bad}"
            );
        }
    }

    #[test]
    fn rejects_wrong_type_invalid_json_and_bad_versions() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "acme/hello", "main.php");
        write_raw(
            tmp.path(),
            r#"{"name":5,"version":"1.0.0","contracts":"^1","entry":"main.php"}"#,
        );
        assert!(problem(parse_manifest(tmp.path()).unwrap_err()).contains("name"));
        write_raw(tmp.path(), "{not json");
        assert!(matches!(
            parse_manifest(tmp.path()),
            Err(PluginError::Manifest { .. })
        ));
        write_raw(
            tmp.path(),
            r#"{"name":"a/b","version":"nope","contracts":"^1","entry":"main.php"}"#,
        );
        assert!(problem(parse_manifest(tmp.path()).unwrap_err()).contains("version"));
        write_raw(
            tmp.path(),
            r#"{"name":"a/b","version":"1.0.0","contracts":"??","entry":"main.php"}"#,
        );
        assert!(problem(parse_manifest(tmp.path()).unwrap_err()).contains("contracts"));
    }

    #[test]
    fn rejects_entry_outside_plugin_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plug");
        write_plugin(&dir, "acme/hello", "main.php");
        fs::write(tmp.path().join("evil.php"), "<?php\n").unwrap();
        write_raw(
            &dir,
            r#"{"name":"acme/hello","version":"1.0.0","contracts":"^1","entry":"../evil.php"}"#,
        );
        assert!(problem(parse_manifest(&dir).unwrap_err()).contains("outside"));
        let abs = tmp.path().join("evil.php");
        write_raw(
            &dir,
            &format!(
                r#"{{"name":"acme/hello","version":"1.0.0","contracts":"^1","entry":{:?}}}"#,
                abs.to_str().unwrap()
            ),
        );
        assert!(problem(parse_manifest(&dir).unwrap_err()).contains("must be a relative path"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_entry_through_symlinked_directory_escaping_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plug");
        write_plugin(&dir, "acme/hello", "main.php");
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("x.php"), "<?php\n").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("sub")).unwrap();
        write_raw(
            &dir,
            r#"{"name":"acme/hello","version":"1.0.0","contracts":"^1","entry":"sub/x.php"}"#,
        );
        assert!(problem(parse_manifest(&dir).unwrap_err()).contains("outside"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_entry_symlink_escaping_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plug");
        write_plugin(&dir, "acme/hello", "main.php");
        let outside = tmp.path().join("secret.php");
        fs::write(&outside, "<?php\n").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("link.php")).unwrap();
        write_raw(
            &dir,
            r#"{"name":"acme/hello","version":"1.0.0","contracts":"^1","entry":"link.php"}"#,
        );
        assert!(problem(parse_manifest(&dir).unwrap_err()).contains("outside"));
    }

    #[test]
    fn rejects_missing_entry_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "acme/hello", "main.php");
        fs::remove_file(tmp.path().join("main.php")).unwrap();
        assert!(problem(parse_manifest(tmp.path()).unwrap_err()).contains("entry"));
    }

    #[test]
    fn rejects_entry_that_is_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "acme/hello", "main.php");
        fs::create_dir(tmp.path().join("sub")).unwrap();
        write_raw(
            tmp.path(),
            r#"{"name":"a/b","version":"1.0.0","contracts":"^1","entry":"sub"}"#,
        );
        assert!(problem(parse_manifest(tmp.path()).unwrap_err()).contains("file"));
    }

    #[test]
    fn error_messages_and_labels() {
        let e = PluginError::Manifest {
            dir: "/p".into(),
            problem: "bad".into(),
        };
        assert_eq!(e.to_string(), "/p: bad");
        assert_eq!(e.plugin_label(), "/p");
        let e = PluginError::Contracts {
            name: "a/b".into(),
            needs: "^2.0".into(),
            bundled: "1.0.0".into(),
        };
        assert_eq!(
            e.to_string(),
            "needs contracts ^2.0, this CLI bundles 1.0.0"
        );
        assert_eq!(e.plugin_label(), "a/b");
        let e = PluginError::Load {
            name: "a/b".into(),
            problem: "boom".into(),
        };
        assert_eq!(e.to_string(), "boom");
        assert_eq!(e.plugin_label(), "a/b");
        let e = PluginError::Duplicate {
            name: "a/b".into(),
            kept: "/d/one".into(),
            ignored: "/d/two".into(),
        };
        assert_eq!(
            e.to_string(),
            "the plugin in /d/two has the same name as the one in /d/one, which is loaded instead"
        );
        assert_eq!(e.plugin_label(), "a/b");
        let e = PluginError::Directory {
            dir: "/nope".into(),
            problem: "not found".into(),
        };
        assert_eq!(e.to_string(), "plugin directory /nope not found");
    }
}
