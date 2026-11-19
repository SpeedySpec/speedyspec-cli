//! Reading `composer.json` and `composer.lock`. Nothing is resolved: the lock says exactly which packages to install,
//! and only its `packages` are read (never `packages-dev`). Everything that later becomes a path is checked here, so
//! no package can reach outside `vendor/` and no autoload path outside its package.

use std::fs;
use std::path::Path;

use serde_json::Value as Json;

use crate::ComposerError;

/// What a package (or the root) autoloads, as relative paths.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Autoload {
    /// Namespace prefix (ending in `\`, or empty) to directories.
    pub psr4: Vec<(String, Vec<String>)>,
    pub psr0: Vec<(String, Vec<String>)>,
    pub classmap: Vec<String>,
    pub files: Vec<String>,
}

/// Where a package's files come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dist {
    pub url: String,
    /// The sha1 of the archive, when the lock has one.
    pub shasum: Option<String>,
}

/// One locked package.
#[derive(Clone, Debug, PartialEq)]
pub struct Package {
    pub name: String,
    pub version: String,
    /// `library` when the lock does not say.
    pub kind: String,
    pub dist: Dist,
    pub autoload: Autoload,
    pub require: Vec<(String, String)>,
    pub extra: Json,
}

/// A plugin's `composer.json` and `composer.lock`, read and checked.
#[derive(Clone, Debug, PartialEq)]
pub struct Lock {
    /// The root package's own autoload, from `composer.json`.
    pub root: Autoload,
    /// The root's `scripts`, event name to commands.
    pub scripts: Vec<(String, Vec<String>)>,
    pub packages: Vec<Package>,
}

/// composer's rule for a package name: `vendor/name`, lowercase.
pub fn is_package_name(name: &str) -> bool {
    fn part(text: &str, double_dash: bool) -> bool {
        let bytes = text.as_bytes();
        if bytes.is_empty() || !is_alnum(bytes[0]) || !is_alnum(bytes[bytes.len() - 1]) {
            return false;
        }
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if is_alnum(b) {
                i += 1;
                continue;
            }
            if !matches!(b, b'_' | b'.' | b'-') {
                return false;
            }
            // One separator between alphanumerics; `--` only in the name part.
            let run = if double_dash && b == b'-' && bytes.get(i + 1) == Some(&b'-') {
                2
            } else {
                1
            };
            if !bytes.get(i + run).is_some_and(|n| is_alnum(*n)) {
                return false;
            }
            i += run;
        }
        true
    }
    fn is_alnum(b: u8) -> bool {
        b.is_ascii_lowercase() || b.is_ascii_digit()
    }
    match name.split_once('/') {
        Some((vendor, package)) => part(vendor, false) && part(package, true),
        None => false,
    }
}

/// Why `path` cannot be an autoload path, if it cannot: it must be relative and stay inside its package.
fn path_problem(path: &str) -> Option<&'static str> {
    if path.contains('\0') || path.contains('\\') {
        return Some("must not contain a backslash or a NUL character");
    }
    if path.starts_with('/') || path.as_bytes().get(1) == Some(&b':') {
        return Some("must be relative to the package");
    }
    if path.split('/').any(|segment| segment == "..") {
        return Some("must stay inside the package");
    }
    None
}

fn read_json(path: &Path) -> Result<Option<Json>, ComposerError> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| ComposerError::new(format!("{}: not valid JSON: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ComposerError::new(format!(
            "cannot read {}: {e}",
            path.display()
        ))),
    }
}

fn paths(owner: &str, key: &str, value: &Json) -> Result<Vec<String>, ComposerError> {
    let list = match value {
        Json::String(one) => vec![one.clone()],
        Json::Array(many) => many
            .iter()
            .map(|item| {
                item.as_str().map(str::to_owned).ok_or_else(|| {
                    ComposerError::new(format!("{owner}: autoload {key} holds a non-string path"))
                })
            })
            .collect::<Result<_, _>>()?,
        _ => {
            return Err(ComposerError::new(format!(
                "{owner}: autoload {key} must be a path or a list of paths"
            )));
        }
    };
    list.into_iter()
        .map(|path| {
            let trimmed = path.trim_end_matches('/').to_owned();
            match path_problem(&trimmed) {
                Some(problem) => Err(ComposerError::new(format!(
                    "{owner}: autoload path {path:?} {problem}"
                ))),
                None => Ok(trimmed),
            }
        })
        .collect()
}

fn prefixed(
    owner: &str,
    key: &str,
    value: Option<&Json>,
) -> Result<Vec<(String, Vec<String>)>, ComposerError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Json::Object(map) = value else {
        return Err(ComposerError::new(format!(
            "{owner}: autoload {key} must map namespaces to paths"
        )));
    };
    map.iter()
        .map(|(prefix, value)| Ok((prefix.clone(), paths(owner, key, value)?)))
        .collect()
}

fn listed(owner: &str, key: &str, value: Option<&Json>) -> Result<Vec<String>, ComposerError> {
    match value {
        None => Ok(Vec::new()),
        Some(value @ Json::Array(_)) => paths(owner, key, value),
        Some(_) => Err(ComposerError::new(format!(
            "{owner}: autoload {key} must be a list of paths"
        ))),
    }
}

/// An `autoload` object (or its absence).
pub(crate) fn autoload(owner: &str, value: Option<&Json>) -> Result<Autoload, ComposerError> {
    let Some(value) = value else {
        return Ok(Autoload::default());
    };
    let Json::Object(map) = value else {
        return Err(ComposerError::new(format!(
            "{owner}: autoload must be an object"
        )));
    };
    Ok(Autoload {
        psr4: prefixed(owner, "psr-4", map.get("psr-4"))?,
        psr0: prefixed(owner, "psr-0", map.get("psr-0"))?,
        classmap: listed(owner, "classmap", map.get("classmap"))?,
        files: listed(owner, "files", map.get("files"))?,
    })
}

fn string_map(value: Option<&Json>) -> Vec<(String, String)> {
    value
        .and_then(Json::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

fn package(index: usize, value: &Json) -> Result<Package, ComposerError> {
    let at = format!("composer.lock packages[{index}]");
    let name = value["name"]
        .as_str()
        .ok_or_else(|| ComposerError::new(format!("{at}: no name")))?;
    if !is_package_name(name) {
        return Err(ComposerError::new(format!(
            "{at}: {name:?} is not a package name (vendor/name, lowercase)"
        )));
    }
    let version = value["version"]
        .as_str()
        .ok_or_else(|| ComposerError::new(format!("{name}: no version")))?
        .to_owned();
    let dist = &value["dist"];
    let kind = dist["type"].as_str().unwrap_or("");
    if kind != "zip" {
        return Err(ComposerError::new(format!(
            "{name}: only zip dists can be installed, not {}",
            if kind.is_empty() {
                "a package without one"
            } else {
                kind
            }
        )));
    }
    let url = dist["url"]
        .as_str()
        .ok_or_else(|| ComposerError::new(format!("{name}: its dist has no url")))?
        .to_owned();
    let shasum = dist["shasum"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase);
    if let Some(sha) = &shasum
        && (sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(ComposerError::new(format!(
            "{name}: its dist shasum is not a sha1"
        )));
    }
    Ok(Package {
        name: name.to_owned(),
        version,
        kind: value["type"].as_str().unwrap_or("library").to_owned(),
        dist: Dist { url, shasum },
        autoload: autoload(name, value.get("autoload"))?,
        require: string_map(value.get("require")),
        extra: value.get("extra").cloned().unwrap_or(Json::Null),
    })
}

/// Whether a requirement is a platform one (`php`, `ext-*`, `lib-*`, composer's own), which no package satisfies.
pub fn is_platform(name: &str) -> bool {
    name == "php"
        || name.starts_with("php-")
        || name.starts_with("ext-")
        || name.starts_with("lib-")
        || name == "composer"
        || name == "composer-plugin-api"
        || name == "composer-runtime-api"
}

impl Lock {
    /// Every problem with `dir`'s `composer.json` and `composer.lock` as this replica reads them, each package checked
    /// on its own rather than stopping at the first.
    pub fn problems(dir: &Path) -> Vec<String> {
        let mut found = Vec::new();
        let json = match read_json(&dir.join("composer.json")) {
            Ok(Some(json)) => json,
            Ok(None) => return vec![format!("there is no composer.json in {}", dir.display())],
            Err(e) => return vec![e.to_string()],
        };
        if let Err(e) = autoload("composer.json", json.get("autoload")) {
            found.push(e.to_string());
        }
        match read_json(&dir.join("composer.lock")) {
            Ok(Some(lock)) => match lock.get("packages") {
                None => {}
                Some(Json::Array(items)) => {
                    for (i, item) in items.iter().enumerate() {
                        if let Err(e) = package(i, item) {
                            found.push(e.to_string());
                        }
                    }
                }
                Some(_) => found.push("composer.lock: packages must be a list".to_owned()),
            },
            Ok(None) => {
                if let Err(e) = Lock::read(dir) {
                    found.push(e.to_string());
                }
            }
            Err(e) => found.push(e.to_string()),
        }
        found
    }

    /// The lock in `dir`; `Ok(None)` when there is nothing to install (no `composer.json`, or one that requires only
    /// platform packages and has no lock).
    pub fn read(dir: &Path) -> Result<Option<Lock>, ComposerError> {
        let Some(json) = read_json(&dir.join("composer.json"))? else {
            return Ok(None);
        };
        let root = autoload("composer.json", json.get("autoload"))?;
        let scripts = json
            .get("scripts")
            .and_then(Json::as_object)
            .map(|map| {
                map.iter()
                    .map(|(event, value)| {
                        let commands = match value {
                            Json::String(one) => vec![one.clone()],
                            Json::Array(many) => many
                                .iter()
                                .filter_map(|c| c.as_str().map(str::to_owned))
                                .collect(),
                            _ => Vec::new(),
                        };
                        (event.clone(), commands)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let needs_packages = string_map(json.get("require"))
            .iter()
            .any(|(name, _)| !is_platform(name));
        let Some(lock) = read_json(&dir.join("composer.lock"))? else {
            if needs_packages {
                return Err(ComposerError::new(
                    "it needs composer packages but has no composer.lock; run composer update in it".to_owned(),
                ));
            }
            return Ok(Some(Lock {
                root,
                scripts,
                packages: Vec::new(),
            }));
        };
        let packages = match lock.get("packages") {
            None => Vec::new(),
            Some(Json::Array(items)) => items
                .iter()
                .enumerate()
                .map(|(i, item)| package(i, item))
                .collect::<Result<_, _>>()?,
            Some(_) => {
                return Err(ComposerError::new(
                    "composer.lock: packages must be a list".to_owned(),
                ));
            }
        };
        Ok(Some(Lock {
            root,
            scripts,
            packages,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(dir: &Path, composer: Json, lock: Option<Json>) {
        fs::write(dir.join("composer.json"), composer.to_string()).unwrap();
        if let Some(lock) = lock {
            fs::write(dir.join("composer.lock"), lock.to_string()).unwrap();
        }
    }

    fn locked(name: &str) -> Json {
        json!({
            "name": name, "version": "7.9.2", "type": "library",
            "dist": {"type": "zip", "url": "https://example.com/a.zip", "shasum": ""},
            "require": {"php": "^8.0", "ext-json": "*", "psr/log": "^3"},
            "autoload": {"psr-4": {"GuzzleHttp\\": "src/"}, "files": ["src/functions_include.php"]},
        })
    }

    #[test]
    fn reads_a_lock() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            json!({"require": {"guzzlehttp/guzzle": "^7"}, "autoload": {"psr-4": {"Acme\\": ["src", "lib/"]}},
                   "scripts": {"post-install-cmd": "Acme\\Hooks::installed"}}),
            Some(
                json!({"packages": [locked("guzzlehttp/guzzle")], "packages-dev": [locked("phpunit/phpunit")]}),
            ),
        );
        let lock = Lock::read(dir.path()).unwrap().unwrap();
        assert_eq!(lock.packages.len(), 1, "dev packages are never installed");
        let p = &lock.packages[0];
        assert_eq!(p.name, "guzzlehttp/guzzle");
        assert_eq!(p.dist.shasum, None);
        assert_eq!(
            p.autoload.psr4,
            vec![("GuzzleHttp\\".to_owned(), vec!["src".to_owned()])]
        );
        assert_eq!(
            p.autoload.files,
            vec!["src/functions_include.php".to_owned()]
        );
        assert_eq!(
            lock.root.psr4[0].1,
            vec!["src".to_owned(), "lib".to_owned()]
        );
        assert_eq!(
            lock.scripts[0],
            (
                "post-install-cmd".into(),
                vec!["Acme\\Hooks::installed".into()]
            )
        );
    }

    #[test]
    fn nothing_to_read() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Lock::read(dir.path()).unwrap().is_none());
        write(
            dir.path(),
            json!({"require": {"php": ">=8.4", "ext-curl": "*"}}),
            None,
        );
        assert!(Lock::read(dir.path()).unwrap().unwrap().packages.is_empty());
    }

    #[test]
    fn packages_without_a_lock_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            json!({"require": {"guzzlehttp/guzzle": "^7"}}),
            None,
        );
        let error = Lock::read(dir.path()).unwrap_err();
        assert!(error.to_string().contains("no composer.lock"), "{error}");
    }

    #[test]
    fn lock_rules() {
        let cases: Vec<(Json, &str)> = vec![
            (json!({"name": "Bad/Name"}), "not a package name"),
            (json!({"name": "../evil"}), "not a package name"),
            (
                json!({"dist": {"type": "path", "url": "../x"}}),
                "only zip dists",
            ),
            (
                json!({"dist": {"type": "zip", "url": "https://e/a.zip", "shasum": "xyz"}}),
                "not a sha1",
            ),
            (
                json!({"autoload": {"psr-4": {"A\\": "../out"}}}),
                "stay inside",
            ),
            (json!({"autoload": {"files": ["/etc/passwd"]}}), "relative"),
            (json!({"autoload": {"classmap": "src"}}), "list of paths"),
        ];
        for (overrides, problem) in cases {
            let dir = tempfile::tempdir().unwrap();
            let mut package = locked("acme/thing");
            for (k, v) in overrides.as_object().unwrap() {
                package[k] = v.clone();
            }
            write(dir.path(), json!({}), Some(json!({"packages": [package]})));
            let error = Lock::read(dir.path()).unwrap_err().to_string();
            assert!(error.contains(problem), "{problem}: {error}");
        }
    }

    #[test]
    fn package_names() {
        for good in [
            "guzzlehttp/guzzle",
            "psr/http-message",
            "symfony/polyfill-php80",
            "a/b--c",
            "a.b/c_d",
        ] {
            assert!(is_package_name(good), "{good}");
        }
        for bad in [
            "guzzle",
            "Guzzle/guzzle",
            "a//b",
            "../x",
            "a/../b",
            "a/b/c",
            "-a/b",
            "a/b-",
            "a--b/c",
            "",
        ] {
            assert!(!is_package_name(bad), "{bad}");
        }
    }
}
