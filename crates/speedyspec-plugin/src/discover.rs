use std::fs;
use std::path::{Path, PathBuf};

use crate::manifest::{MANIFEST_FILE, Plugin, PluginError, read_plugin};

/// The per-user plugin directory, and warnings to show when the environment named one that
/// was ignored.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct UserPluginDir {
    /// `$SPEEDYSPEC_HOME/plugins`, or `<data dir>/speedyspec/plugins`; `None` when no absolute
    /// home is known. Never relative, so nothing is loaded from the working directory on its own.
    pub dir: Option<PathBuf>,
    /// One line for each relative path that was ignored (`SPEEDYSPEC_HOME`, or the platform data
    /// directory derived from a relative `HOME`).
    pub warnings: Vec<String>,
}

/// Per-user plugin directory: `$SPEEDYSPEC_HOME/plugins` when that is an absolute path,
/// otherwise `<data dir>/speedyspec/plugins` when the platform data directory is absolute.
pub fn user_plugin_dir() -> UserPluginDir {
    user_plugin_dir_from(
        std::env::var("SPEEDYSPEC_HOME").ok().as_deref(),
        dirs::data_dir(),
    )
}

fn user_plugin_dir_from(home: Option<&str>, data_dir: Option<PathBuf>) -> UserPluginDir {
    let mut warnings = Vec::new();
    if let Some(h) = home.filter(|h| !h.is_empty()) {
        if Path::new(h).is_absolute() {
            return UserPluginDir {
                dir: Some(PathBuf::from(h).join("plugins")),
                warnings,
            };
        }
        // A relative home would be resolved against the working directory, which must never
        // be a source of plugins.
        warnings.push(format!(
            "SPEEDYSPEC_HOME must be an absolute path; ignoring \"{h}\" and using the default data directory"
        ));
    }
    // `dirs` builds the data directory from `$HOME` (and on Linux `$XDG_DATA_HOME`) without
    // checking that it is absolute; a relative one would also point into the working directory.
    let dir = match data_dir {
        Some(d) if d.is_absolute() => Some(d.join("speedyspec").join("plugins")),
        Some(d) => {
            warnings.push(format!(
                "the platform data directory \"{}\" is not an absolute path (check HOME); no user plugin directory is used",
                d.display()
            ));
            None
        }
        None => None,
    };
    UserPluginDir { dir, warnings }
}

/// A directory to search for plugins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchDir {
    pub path: PathBuf,
    /// Named by the user (`--plugin-dir`): if it is missing, that is reported. The user plugin
    /// directory is not explicit; it need not exist.
    pub explicit: bool,
}

impl SearchDir {
    pub fn implicit(path: PathBuf) -> Self {
        Self {
            path,
            explicit: false,
        }
    }

    pub fn explicit(path: PathBuf) -> Self {
        Self {
            path,
            explicit: true,
        }
    }
}

fn has_manifest(dir: &Path) -> bool {
    dir.join(MANIFEST_FILE).is_file()
}

/// Finds plugins in each directory, in order. A directory holding a manifest is itself one
/// plugin; otherwise each immediate subdirectory with a manifest is one (symlinks followed).
/// Within a directory plugins are ordered by name, and two with the same name are reported
/// (the first by path order is kept). Across directories, a later directory's plugin replaces
/// an earlier one of the same name and takes the later position. A missing implicit directory
/// and manifest-less subdirectories are skipped; a missing explicit directory is reported.
pub fn discover(dirs: &[SearchDir]) -> (Vec<Plugin>, Vec<PluginError>) {
    let mut plugins: Vec<Plugin> = Vec::new();
    let mut errors = Vec::new();

    for SearchDir {
        path: dir,
        explicit,
    } in dirs
    {
        if !dir.is_dir() {
            if *explicit {
                errors.push(PluginError::Directory {
                    dir: dir.clone(),
                    problem: if dir.exists() {
                        "is not a directory".to_owned()
                    } else {
                        "not found".to_owned()
                    },
                });
            }
            continue;
        }
        let candidates: Vec<PathBuf> = if has_manifest(dir) {
            vec![dir.clone()]
        } else {
            let read = match fs::read_dir(dir) {
                Ok(read) => read,
                Err(e) => {
                    if *explicit {
                        errors.push(PluginError::Directory {
                            dir: dir.clone(),
                            problem: format!("cannot be read: {e}"),
                        });
                    }
                    continue;
                }
            };
            let mut subs: Vec<PathBuf> = read
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_dir() && has_manifest(p))
                .collect();
            subs.sort();
            subs
        };

        let mut found: Vec<Plugin> = Vec::new();
        for candidate in candidates {
            match read_plugin(&candidate) {
                Ok(plugin) => {
                    match found
                        .iter()
                        .find(|p| p.manifest.name == plugin.manifest.name)
                    {
                        Some(kept) => errors.push(PluginError::Duplicate {
                            name: plugin.manifest.name.clone(),
                            kept: kept.dir.clone(),
                            ignored: plugin.dir,
                        }),
                        None => found.push(plugin),
                    }
                }
                Err(e) => errors.push(e),
            }
        }
        found.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));

        for plugin in found {
            plugins.retain(|p| p.manifest.name != plugin.manifest.name);
            plugins.push(plugin);
        }
    }
    (plugins, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::tests::write_plugin;
    use std::fs;

    fn names(plugins: &[crate::Plugin]) -> Vec<String> {
        plugins.iter().map(|p| p.manifest.name.clone()).collect()
    }

    #[test]
    fn discovers_subdirectories_in_name_order() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(&tmp.path().join("z"), "acme/zed", "main.php");
        write_plugin(&tmp.path().join("a"), "acme/alpha", "main.php");
        fs::write(tmp.path().join("file.txt"), "x").unwrap();
        fs::create_dir(tmp.path().join("empty")).unwrap();
        let (plugins, errors) = discover(&[SearchDir::implicit(tmp.path().to_path_buf())]);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(names(&plugins), ["acme/alpha", "acme/zed"]);
    }

    #[test]
    fn invalid_manifest_is_reported_and_discovery_continues() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(&tmp.path().join("good"), "acme/good", "main.php");
        fs::create_dir(tmp.path().join("bad")).unwrap();
        fs::write(tmp.path().join("bad").join(crate::MANIFEST_FILE), "{").unwrap();
        let (plugins, errors) = discover(&[SearchDir::implicit(tmp.path().to_path_buf())]);
        assert_eq!(names(&plugins), ["acme/good"]);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn directory_with_manifest_is_one_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "acme/solo", "main.php");
        // A nested plugin must not be picked up as well.
        write_plugin(&tmp.path().join("nested"), "acme/nested", "main.php");
        let (plugins, errors) = discover(&[SearchDir::implicit(tmp.path().to_path_buf())]);
        assert!(errors.is_empty());
        assert_eq!(names(&plugins), ["acme/solo"]);
        assert_eq!(plugins[0].dir, tmp.path());
    }

    #[test]
    fn later_directory_wins_on_same_name() {
        let tmp = tempfile::tempdir().unwrap();
        let one = tmp.path().join("one");
        let two = tmp.path().join("two");
        write_plugin(&one.join("p"), "acme/dup", "main.php");
        write_plugin(&one.join("q"), "acme/other", "main.php");
        write_plugin(&two.join("p"), "acme/dup", "main.php");
        let (plugins, errors) =
            discover(&[SearchDir::explicit(one), SearchDir::explicit(two.clone())]);
        assert!(errors.is_empty());
        assert_eq!(names(&plugins), ["acme/other", "acme/dup"]);
        assert_eq!(plugins[1].dir, two.join("p"));
    }

    #[test]
    fn missing_implicit_directory_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (plugins, errors) = discover(&[SearchDir::implicit(tmp.path().join("nope"))]);
        assert!(plugins.is_empty());
        assert!(errors.is_empty());
    }

    #[test]
    fn missing_explicit_directory_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("file");
        fs::write(&file, "x").unwrap();
        let (plugins, errors) = discover(&[
            SearchDir::explicit(tmp.path().join("nope")),
            SearchDir::explicit(file.clone()),
        ]);
        assert!(plugins.is_empty());
        assert_eq!(
            errors,
            [
                PluginError::Directory {
                    dir: tmp.path().join("nope"),
                    problem: "not found".into(),
                },
                PluginError::Directory {
                    dir: file,
                    problem: "is not a directory".into(),
                },
            ]
        );
    }

    #[test]
    fn same_name_twice_in_one_directory_is_reported_and_first_kept() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(&tmp.path().join("b"), "acme/dup", "main.php");
        write_plugin(&tmp.path().join("a"), "acme/dup", "main.php");
        write_plugin(&tmp.path().join("c"), "acme/other", "main.php");
        let (plugins, errors) = discover(&[SearchDir::implicit(tmp.path().to_path_buf())]);
        assert_eq!(names(&plugins), ["acme/dup", "acme/other"]);
        assert_eq!(plugins[0].dir, tmp.path().join("a"));
        assert_eq!(
            errors,
            [PluginError::Duplicate {
                name: "acme/dup".into(),
                kept: tmp.path().join("a"),
                ignored: tmp.path().join("b"),
            }]
        );
    }

    #[cfg(unix)]
    #[test]
    fn follows_symlinked_plugin_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        write_plugin(&real, "acme/linked", "main.php");
        let root = tmp.path().join("root");
        fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(&real, root.join("link")).unwrap();
        let (plugins, errors) = discover(&[SearchDir::implicit(root)]);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(names(&plugins), ["acme/linked"]);
    }

    fn dir_of(found: UserPluginDir) -> Option<PathBuf> {
        assert!(found.warnings.is_empty(), "{:?}", found.warnings);
        found.dir
    }

    #[test]
    fn user_plugin_dir_prefers_speedyspec_home() {
        assert_eq!(
            dir_of(user_plugin_dir_from(Some("/h"), Some("/data".into()))),
            Some(PathBuf::from("/h/plugins"))
        );
    }

    #[test]
    fn user_plugin_dir_falls_back_to_data_dir() {
        let expected = Some(PathBuf::from("/data/speedyspec/plugins"));
        assert_eq!(
            dir_of(user_plugin_dir_from(None, Some("/data".into()))),
            expected
        );
        assert_eq!(
            dir_of(user_plugin_dir_from(Some(""), Some("/data".into()))),
            expected
        );
    }

    #[test]
    fn user_plugin_dir_is_none_without_any_home() {
        assert_eq!(dir_of(user_plugin_dir_from(None, None)), None);
        assert_eq!(dir_of(user_plugin_dir_from(Some(""), None)), None);
    }

    #[test]
    fn relative_speedyspec_home_is_ignored_with_a_warning() {
        for relative in ["h", "./h", "../h"] {
            let found = user_plugin_dir_from(Some(relative), Some("/data".into()));
            assert_eq!(found.dir, Some(PathBuf::from("/data/speedyspec/plugins")));
            assert_eq!(found.warnings.len(), 1, "{:?}", found.warnings);
            let warning = &found.warnings[0];
            assert!(
                warning.contains("SPEEDYSPEC_HOME must be an absolute path"),
                "{warning}"
            );
            assert!(warning.contains(relative), "{warning}");
        }
    }

    #[test]
    fn relative_data_dir_yields_no_plugin_directory() {
        for data in [
            "rel/Library/Application Support",
            "rel/.local/share",
            "share",
        ] {
            let found = user_plugin_dir_from(None, Some(data.into()));
            assert_eq!(found.dir, None, "{data}");
            assert_eq!(found.warnings.len(), 1, "{:?}", found.warnings);
            assert!(
                found.warnings[0].contains("not an absolute path"),
                "{:?}",
                found.warnings
            );
            assert!(found.warnings[0].contains(data), "{:?}", found.warnings);
            // Both relative: two warnings, still no directory.
            let found = user_plugin_dir_from(Some("h"), Some(data.into()));
            assert_eq!(found.dir, None);
            assert_eq!(found.warnings.len(), 2, "{:?}", found.warnings);
        }
        // An absolute SPEEDYSPEC_HOME does not need the data directory at all.
        let found = user_plugin_dir_from(Some("/h"), Some("rel".into()));
        assert_eq!(found.dir, Some(PathBuf::from("/h/plugins")));
        assert!(found.warnings.is_empty());
    }
}
