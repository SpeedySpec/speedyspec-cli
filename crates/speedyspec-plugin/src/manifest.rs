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
}

impl Plugin {
    /// Absolute, canonical path of the entry file. Falls back to the plain join if the file
    /// has vanished since validation, so the loader reports the failure.
    pub fn entry_path(&self) -> PathBuf {
        let joined = self.dir.join(&self.manifest.entry);
        joined.canonicalize().unwrap_or(joined)
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
}

impl PluginError {
    /// The plugin name where known, otherwise the directory.
    pub fn plugin_label(&self) -> String {
        match self {
            PluginError::Manifest { dir, .. } => dir.display().to_string(),
            PluginError::Contracts { name, .. } | PluginError::Load { name, .. } => name.clone(),
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
        }
    }
}

impl std::error::Error for PluginError {}

fn valid_name(name: &str) -> bool {
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

/// Resolves `entry` and proves it is a regular file inside `dir`. Canonicalising both sides
/// resolves `..` and symlinks, so neither can be used to point outside the plugin.
fn check_entry(dir: &Path, entry: &str) -> Result<PathBuf, String> {
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
    Ok(rel.to_path_buf())
}

/// Reads and validates `<dir>/speedyspec-plugin.json`.
pub fn parse_manifest(dir: &Path) -> Result<Manifest, PluginError> {
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
    let entry = check_entry(dir, entry_raw).map_err(fail)?;

    Ok(Manifest {
        name: name.to_string(),
        version,
        contracts,
        contracts_raw: contracts_raw.to_string(),
        entry,
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
        let plugin = Plugin {
            manifest: m,
            dir: tmp.path().to_path_buf(),
        };
        assert_eq!(
            plugin.entry_path(),
            tmp.path().join("src/main.php").canonicalize().unwrap()
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
        assert!(parse_manifest(&dir).is_err());
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
    }
}
