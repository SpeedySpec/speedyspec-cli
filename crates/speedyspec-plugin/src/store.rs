//! The plugins installed in the user plugin directory: where one lives, finding it, removing it.

use std::fs;
use std::path::{Path, PathBuf};

use crate::manifest::{PluginError, read_plugin};

/// The directory an installed plugin gets: `vendor/name` becomes `vendor.name`. A dot cannot
/// appear in a plugin name, so two names never share a directory.
pub fn directory_name(plugin_name: &str) -> String {
    plugin_name.replace('/', ".")
}

/// The directory of the installed plugin called `plugin_name`: the one whose manifest carries
/// that name, or, when no manifest matches, the directory that name would be installed to (so a
/// plugin with a broken manifest can still be replaced or removed).
pub fn find_installed(plugins_dir: &Path, plugin_name: &str) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = fs::read_dir(plugins_dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    candidates.sort();
    if let Some(found) = candidates
        .iter()
        .find(|dir| read_plugin(dir).is_ok_and(|plugin| plugin.manifest.name == plugin_name))
    {
        return Some(found.clone());
    }
    let by_directory = plugins_dir.join(directory_name(plugin_name));
    fs::symlink_metadata(&by_directory)
        .is_ok()
        .then_some(by_directory)
}

/// Deletes the installed plugin called `plugin_name` and returns the directory it was in. A
/// plugin directory that is a symbolic link is unlinked; what it points at is left alone.
pub fn remove_installed(plugins_dir: &Path, plugin_name: &str) -> Result<PathBuf, PluginError> {
    let Some(dir) = find_installed(plugins_dir, plugin_name) else {
        return Err(PluginError::NotInstalled {
            name: plugin_name.to_owned(),
            dir: plugins_dir.to_path_buf(),
        });
    };
    let is_link = fs::symlink_metadata(&dir).is_ok_and(|found| found.file_type().is_symlink());
    let removed = if is_link {
        fs::remove_file(&dir)
    } else {
        fs::remove_dir_all(&dir)
    };
    removed.map_err(|error| PluginError::Directory {
        dir: dir.clone(),
        problem: format!("could not be removed: {error}"),
    })?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::tests::write_plugin;

    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn directory_name_replaces_the_slash() {
        assert_eq!(directory_name("acme/install-wp"), "acme.install-wp");
    }

    #[test]
    fn finds_an_installed_plugin_by_manifest_name() {
        let plugins = tempfile::tempdir().unwrap();
        write_plugin(&plugins.path().join("whatever"), "acme/one", "plugin.php");
        write_plugin(&plugins.path().join("acme.two"), "acme/two", "plugin.php");
        assert_eq!(
            find_installed(plugins.path(), "acme/one"),
            Some(plugins.path().join("whatever"))
        );
        assert_eq!(find_installed(plugins.path(), "acme/three"), None);
        assert_eq!(
            find_installed(&plugins.path().join("missing"), "acme/one"),
            None
        );
    }

    #[test]
    fn finds_a_broken_plugin_by_its_directory() {
        let plugins = tempfile::tempdir().unwrap();
        let broken = plugins.path().join("acme.broken");
        fs::create_dir(&broken).unwrap();
        fs::write(broken.join("speedyspec-plugin.json"), "not json").unwrap();
        assert_eq!(find_installed(plugins.path(), "acme/broken"), Some(broken));
    }

    #[test]
    fn remove_deletes_only_that_plugin() {
        let plugins = tempfile::tempdir().unwrap();
        write_plugin(&plugins.path().join("acme.one"), "acme/one", "plugin.php");
        write_plugin(&plugins.path().join("acme.two"), "acme/two", "plugin.php");
        let removed = remove_installed(plugins.path(), "acme/one").unwrap();
        assert_eq!(removed, plugins.path().join("acme.one"));
        assert_eq!(listing(plugins.path()), vec!["acme.two"]);
    }

    #[test]
    fn remove_unknown_is_an_error() {
        let plugins = tempfile::tempdir().unwrap();
        write_plugin(&plugins.path().join("acme.one"), "acme/one", "plugin.php");
        let error = remove_installed(plugins.path(), "acme/nope").unwrap_err();
        assert!(
            error.to_string().contains("acme/nope is not installed in"),
            "{error}"
        );
        assert_eq!(listing(plugins.path()), vec!["acme.one"]);
    }

    #[cfg(unix)]
    #[test]
    fn remove_never_follows_a_symlinked_plugin_directory() {
        let plugins = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        write_plugin(&elsewhere.path().join("real"), "acme/linked", "plugin.php");
        std::os::unix::fs::symlink(
            elsewhere.path().join("real"),
            plugins.path().join("acme.linked"),
        )
        .unwrap();
        remove_installed(plugins.path(), "acme/linked").unwrap();
        assert_eq!(listing(plugins.path()), Vec::<String>::new());
        assert!(
            elsewhere.path().join("real/plugin.php").exists(),
            "the target must survive"
        );
    }
}
