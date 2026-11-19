//! Autoload maps: what composer's generated autoloader knows (PSR-4 and PSR-0 prefixes, a classmap, the files to
//! include), kept by speedyspec as data in `vendor/speedyspec-autoload.json` instead of generated PHP, and resolved in
//! Rust. Every path is relative to the plugin's directory, so the directory can be moved after installing.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value as Json, json};

use crate::ComposerError;
use crate::classmap;
use crate::lock::{Autoload, Lock};

/// The file the maps are kept in, under the plugin's directory.
pub const MAPS_FILE: &str = "vendor/speedyspec-autoload.json";
const SCHEMA: u64 = 1;

/// Where each class is, relative to the plugin's directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Maps {
    /// Longest prefix first.
    pub psr4: Vec<(String, Vec<String>)>,
    pub psr0: Vec<(String, Vec<String>)>,
    pub classmap: BTreeMap<String, String>,
    /// Included in this order: packages in lock order, the root last.
    pub files: Vec<String>,
}

fn join(base: &str, path: &str) -> String {
    match (base.is_empty(), path.is_empty()) {
        (true, _) => path.to_owned(),
        (_, true) => base.to_owned(),
        _ => format!("{base}/{path}"),
    }
}

impl Maps {
    fn add(&mut self, base: &Path, owner_dir: &str, autoload: &Autoload) {
        for (prefix, dirs) in &autoload.psr4 {
            let dirs = dirs.iter().map(|d| join(owner_dir, d)).collect();
            self.psr4.push((prefix.clone(), dirs));
        }
        for (prefix, dirs) in &autoload.psr0 {
            let dirs = dirs.iter().map(|d| join(owner_dir, d)).collect();
            self.psr0.push((prefix.clone(), dirs));
        }
        let scanned: Vec<PathBuf> = autoload
            .classmap
            .iter()
            .map(|p| PathBuf::from(join(owner_dir, p)))
            .collect();
        for (class, file) in classmap::scan(base, &scanned) {
            self.classmap
                .entry(class)
                .or_insert_with(|| file.to_string_lossy().into_owned());
        }
        self.files
            .extend(autoload.files.iter().map(|f| join(owner_dir, f)));
    }

    /// The maps for `lock`'s packages (installed under `vendor/<name>`) and the root, in `base`.
    pub fn build(base: &Path, lock: &Lock) -> Maps {
        Maps::build_in(base, "vendor", lock)
    }

    /// As [`Maps::build`], with the packages read from `vendor_dir` (a staging directory, say) while the maps still
    /// point into `vendor/`, where they will be.
    pub fn build_in(base: &Path, vendor_dir: &str, lock: &Lock) -> Maps {
        let mut maps = Maps::build_from(base, vendor_dir, lock);
        if vendor_dir != "vendor" {
            let from = format!("{vendor_dir}/");
            let fix = |path: &mut String| {
                if let Some(rest) = path.strip_prefix(&from) {
                    *path = format!("vendor/{rest}");
                }
            };
            for (_, dirs) in maps.psr4.iter_mut().chain(maps.psr0.iter_mut()) {
                dirs.iter_mut().for_each(fix);
            }
            maps.files.iter_mut().for_each(fix);
            for file in maps.classmap.values_mut() {
                fix(file);
            }
        }
        maps
    }

    fn build_from(base: &Path, vendor_dir: &str, lock: &Lock) -> Maps {
        let mut maps = Maps::default();
        for package in &lock.packages {
            maps.add(
                base,
                &format!("{vendor_dir}/{}", package.name),
                &package.autoload,
            );
        }
        // The root's own paths into vendor/ (a classmap of a package without autoload) are read where the packages
        // are now; `build_in` maps them back.
        let mut root = lock.root.clone();
        if vendor_dir != "vendor" {
            let into = |path: &mut String| {
                if let Some(rest) = path.strip_prefix("vendor/") {
                    *path = format!("{vendor_dir}/{rest}");
                }
            };
            for (_, dirs) in root.psr4.iter_mut().chain(root.psr0.iter_mut()) {
                dirs.iter_mut().for_each(into);
            }
            root.classmap.iter_mut().for_each(into);
            root.files.iter_mut().for_each(into);
        }
        maps.add(base, "", &root);
        // composer tries longer prefixes first; the sort is stable, so lock order breaks ties.
        maps.psr4.sort_by_key(|p| std::cmp::Reverse(p.0.len()));
        maps.psr0.sort_by_key(|p| std::cmp::Reverse(p.0.len()));
        maps
    }

    fn to_json(&self) -> Json {
        json!({
            "schema": SCHEMA,
            "psr-4": self.psr4,
            "psr-0": self.psr0,
            "classmap": self.classmap,
            "files": self.files,
        })
    }

    /// Writes the maps under `base`.
    pub fn write(&self, base: &Path) -> Result<(), ComposerError> {
        self.write_to(&base.join(MAPS_FILE))
    }

    /// Writes the maps to `path`.
    pub fn write_to(&self, path: &Path) -> Result<(), ComposerError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                ComposerError::new(format!("cannot create {}: {e}", parent.display()))
            })?;
        }
        let text = serde_json::to_string_pretty(&self.to_json()).expect("maps are JSON");
        fs::write(path, text + "\n")
            .map_err(|e| ComposerError::new(format!("cannot write {}: {e}", path.display())))
    }

    /// The maps under `base`; `Ok(None)` when it has none.
    pub fn read(base: &Path) -> Result<Option<Maps>, ComposerError> {
        let path = base.join(MAPS_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(ComposerError::new(format!(
                    "cannot read {}: {e}",
                    path.display()
                )));
            }
        };
        let bad = || {
            ComposerError::new(format!(
                "{}: not autoload maps speedyspec wrote",
                path.display()
            ))
        };
        let json: Json = serde_json::from_str(&text).map_err(|_| bad())?;
        if json["schema"].as_u64() != Some(SCHEMA) {
            return Err(bad());
        }
        let prefixes = |key: &str| -> Option<Vec<(String, Vec<String>)>> {
            json[key]
                .as_array()?
                .iter()
                .map(|pair| {
                    let prefix = pair.get(0)?.as_str()?.to_owned();
                    let dirs = pair
                        .get(1)?
                        .as_array()?
                        .iter()
                        .map(|d| d.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()?;
                    Some((prefix, dirs))
                })
                .collect()
        };
        let classmap = json["classmap"]
            .as_object()
            .ok_or_else(bad)?
            .iter()
            .map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
            .collect::<Option<BTreeMap<_, _>>>()
            .ok_or_else(bad)?;
        let files = json["files"]
            .as_array()
            .ok_or_else(bad)?
            .iter()
            .map(|f| f.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(bad)?;
        Ok(Some(Maps {
            psr4: prefixes("psr-4").ok_or_else(bad)?,
            psr0: prefixes("psr-0").ok_or_else(bad)?,
            classmap,
            files,
        }))
    }

    /// The file that declares `class`, as composer's autoloader would find it: the classmap, then PSR-4, then PSR-0.
    pub fn find(&self, base: &Path, class: &str) -> Option<PathBuf> {
        let class = class.trim_start_matches('\\');
        if let Some(file) = self.classmap.get(class) {
            return Some(base.join(file));
        }
        for (prefix, dirs) in &self.psr4 {
            let Some(rest) = class.strip_prefix(prefix.as_str()) else {
                continue;
            };
            let rest = format!("{}.php", rest.replace('\\', "/"));
            for dir in dirs {
                let path = base.join(join(dir, &rest));
                if path.is_file() {
                    return Some(path);
                }
            }
        }
        // PSR-0: underscores in the class name (not the namespace) are directory separators too.
        let psr0 = match class.rfind('\\') {
            Some(at) => format!(
                "{}/{}.php",
                class[..at].replace('\\', "/"),
                class[at + 1..].replace('_', "/")
            ),
            None => format!("{}.php", class.replace('_', "/")),
        };
        for (prefix, dirs) in &self.psr0 {
            if !class.starts_with(prefix.as_str()) {
                continue;
            }
            for dir in dirs {
                let path = base.join(join(dir, &psr0));
                if path.is_file() {
                    return Some(path);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::{Dist, Package};

    fn package(name: &str, autoload: Autoload) -> Package {
        Package {
            name: name.into(),
            version: "1.0.0".into(),
            kind: "library".into(),
            dist: Dist {
                url: "https://example.com/a.zip".into(),
                shasum: None,
            },
            autoload,
            require: Vec::new(),
            extra: Json::Null,
        }
    }

    fn file(base: &Path, path: &str, body: &str) {
        let path = base.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn builds_writes_reads_and_finds() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        file(
            base,
            "vendor/acme/http/src/Client.php",
            "<?php namespace Acme\\Http; class Client {}",
        );
        file(
            base,
            "vendor/acme/http/src/Sub/Thing.php",
            "<?php namespace Acme\\Http\\Sub; class Thing {}",
        );
        file(
            base,
            "vendor/acme/http/src/functions.php",
            "<?php function acme_http() {}",
        );
        file(
            base,
            "vendor/old/lib/lib/Old/Legacy_Thing.php",
            "<?php class Old_Legacy_Thing {}",
        );
        file(
            base,
            "vendor/maps/it/classes/Mapped.php",
            "<?php class SomethingMapped {}",
        );
        file(
            base,
            "src/Plugin.php",
            "<?php namespace Mine; class Plugin {}",
        );
        let lock = Lock {
            root: Autoload {
                psr4: vec![("Mine\\".into(), vec!["src".into()])],
                ..Default::default()
            },
            scripts: Vec::new(),
            packages: vec![
                package(
                    "acme/http",
                    Autoload {
                        psr4: vec![("Acme\\Http\\".into(), vec!["src".into()])],
                        files: vec!["src/functions.php".into()],
                        ..Default::default()
                    },
                ),
                package(
                    "old/lib",
                    Autoload {
                        psr0: vec![("Old_".into(), vec!["lib".into()])],
                        ..Default::default()
                    },
                ),
                package(
                    "maps/it",
                    Autoload {
                        classmap: vec!["classes".into()],
                        ..Default::default()
                    },
                ),
            ],
        };
        let maps = Maps::build(base, &lock);
        assert_eq!(maps.files, ["vendor/acme/http/src/functions.php"]);
        assert_eq!(maps.psr4[0].0, "Acme\\Http\\", "longest prefix first");
        maps.write(base).unwrap();
        let read = Maps::read(base).unwrap().unwrap();
        assert_eq!(read, maps);
        let found = |class: &str| {
            read.find(base, class)
                .map(|p| p.strip_prefix(base).unwrap().to_path_buf())
        };
        assert_eq!(
            found("Acme\\Http\\Client"),
            Some("vendor/acme/http/src/Client.php".into())
        );
        assert_eq!(
            found("\\Acme\\Http\\Sub\\Thing"),
            Some("vendor/acme/http/src/Sub/Thing.php".into())
        );
        // PSR-0 reads the underscore as a directory: Old/Legacy/Thing.php, which is not there.
        assert_eq!(found("Old_Legacy_Thing"), None);
        assert_eq!(
            found("SomethingMapped"),
            Some("vendor/maps/it/classes/Mapped.php".into())
        );
        assert_eq!(found("Mine\\Plugin"), Some("src/Plugin.php".into()));
        assert_eq!(found("Acme\\Http\\Missing"), None);
    }

    #[test]
    fn psr0_underscores_are_directories() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        file(
            base,
            "vendor/old/lib/lib/Old/Legacy/Thing.php",
            "<?php class Old_Legacy_Thing {}",
        );
        let maps = Maps {
            psr0: vec![("Old_".into(), vec!["vendor/old/lib/lib".into()])],
            ..Default::default()
        };
        assert_eq!(
            maps.find(base, "Old_Legacy_Thing"),
            Some(base.join("vendor/old/lib/lib/Old/Legacy/Thing.php"))
        );
    }

    #[test]
    fn foreign_maps_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        file(dir.path(), MAPS_FILE, "{\"schema\": 9}");
        assert!(Maps::read(dir.path()).is_err());
        assert!(
            Maps::read(tempfile::tempdir().unwrap().path())
                .unwrap()
                .is_none()
        );
    }
}
