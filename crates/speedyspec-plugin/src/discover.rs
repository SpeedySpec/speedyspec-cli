use std::fs;
use std::path::{Path, PathBuf};

use crate::manifest::{MANIFEST_FILE, Plugin, PluginError, parse_manifest};

/// Per-user plugin directory: `$SPEEDYSPEC_HOME/plugins`, or `<data dir>/speedyspec/plugins`.
/// `None` when no home is known; never a relative fallback, so nothing is loaded from the
/// working directory on its own.
pub fn user_plugin_dir() -> Option<PathBuf> {
    user_plugin_dir_from(
        std::env::var("SPEEDYSPEC_HOME").ok().as_deref(),
        dirs::data_dir(),
    )
}

fn user_plugin_dir_from(home: Option<&str>, data_dir: Option<PathBuf>) -> Option<PathBuf> {
    match home {
        Some(h) if !h.is_empty() => Some(PathBuf::from(h).join("plugins")),
        _ => data_dir.map(|d| d.join("speedyspec").join("plugins")),
    }
}

fn has_manifest(dir: &Path) -> bool {
    dir.join(MANIFEST_FILE).is_file()
}

/// Finds plugins in each directory, in order. A directory holding a manifest is itself one
/// plugin; otherwise each immediate subdirectory with a manifest is one (symlinks followed).
/// Within a directory plugins are ordered by name; for duplicate names the later one wins and
/// takes the later position. Missing directories and manifest-less subdirectories are skipped.
pub fn discover(dirs: &[PathBuf]) -> (Vec<Plugin>, Vec<PluginError>) {
    let mut plugins: Vec<Plugin> = Vec::new();
    let mut errors = Vec::new();

    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        let candidates: Vec<PathBuf> = if has_manifest(dir) {
            vec![dir.clone()]
        } else {
            let Ok(read) = fs::read_dir(dir) else {
                continue;
            };
            let mut subs: Vec<PathBuf> = read
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_dir() && has_manifest(p))
                .collect();
            subs.sort();
            subs
        };

        let mut found = Vec::new();
        for candidate in candidates {
            match parse_manifest(&candidate) {
                Ok(manifest) => found.push(Plugin {
                    manifest,
                    dir: candidate,
                }),
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
        let (plugins, errors) = discover(&[tmp.path().to_path_buf()]);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(names(&plugins), ["acme/alpha", "acme/zed"]);
    }

    #[test]
    fn invalid_manifest_is_reported_and_discovery_continues() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(&tmp.path().join("good"), "acme/good", "main.php");
        fs::create_dir(tmp.path().join("bad")).unwrap();
        fs::write(tmp.path().join("bad").join(crate::MANIFEST_FILE), "{").unwrap();
        let (plugins, errors) = discover(&[tmp.path().to_path_buf()]);
        assert_eq!(names(&plugins), ["acme/good"]);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn directory_with_manifest_is_one_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "acme/solo", "main.php");
        // A nested plugin must not be picked up as well.
        write_plugin(&tmp.path().join("nested"), "acme/nested", "main.php");
        let (plugins, errors) = discover(&[tmp.path().to_path_buf()]);
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
        let (plugins, errors) = discover(&[one, two.clone()]);
        assert!(errors.is_empty());
        assert_eq!(names(&plugins), ["acme/other", "acme/dup"]);
        assert_eq!(plugins[1].dir, two.join("p"));
    }

    #[test]
    fn missing_directory_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (plugins, errors) = discover(&[tmp.path().join("nope")]);
        assert!(plugins.is_empty());
        assert!(errors.is_empty());
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
        let (plugins, errors) = discover(&[root]);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(names(&plugins), ["acme/linked"]);
    }

    #[test]
    fn user_plugin_dir_prefers_speedyspec_home() {
        assert_eq!(
            user_plugin_dir_from(Some("/h"), Some("/data".into())),
            Some(std::path::PathBuf::from("/h/plugins"))
        );
    }

    #[test]
    fn user_plugin_dir_falls_back_to_data_dir() {
        let expected = Some(std::path::PathBuf::from("/data/speedyspec/plugins"));
        assert_eq!(user_plugin_dir_from(None, Some("/data".into())), expected);
        assert_eq!(
            user_plugin_dir_from(Some(""), Some("/data".into())),
            expected
        );
    }

    #[test]
    fn user_plugin_dir_is_none_without_any_home() {
        assert_eq!(user_plugin_dir_from(None, None), None);
        assert_eq!(user_plugin_dir_from(Some(""), None), None);
    }
}
