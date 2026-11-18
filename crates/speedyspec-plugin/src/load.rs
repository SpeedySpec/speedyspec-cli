use semver::Version;

use crate::manifest::{Plugin, PluginError};

/// Runs a plugin's entry file. Implemented by the PHP host; tests use a fake.
pub trait Loader {
    fn load(&self, plugin: &Plugin) -> Result<(), String>;
}

#[derive(Debug)]
pub struct LoadReport {
    pub loaded: Vec<Plugin>,
    pub skipped: Vec<PluginError>,
}

/// Checks each plugin's contracts range against the bundled version, then loads it. One
/// plugin failing never stops the others.
pub fn load_all(
    plugins: Vec<Plugin>,
    bundled_contracts: &Version,
    loader: &dyn Loader,
) -> LoadReport {
    let mut report = LoadReport {
        loaded: Vec::new(),
        skipped: Vec::new(),
    };
    for plugin in plugins {
        if !plugin.manifest.contracts.matches(bundled_contracts) {
            report.skipped.push(PluginError::Contracts {
                name: plugin.manifest.name.clone(),
                needs: plugin.manifest.contracts_raw.clone(),
                bundled: bundled_contracts.to_string(),
            });
            continue;
        }
        match loader.load(&plugin) {
            Ok(()) => report.loaded.push(plugin),
            Err(problem) => report.skipped.push(PluginError::Load {
                name: plugin.manifest.name.clone(),
                problem,
            }),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Manifest, Plugin, PluginError};
    use std::cell::RefCell;

    fn plugin(name: &str, contracts: &str) -> Plugin {
        Plugin {
            manifest: Manifest {
                name: name.into(),
                version: semver::Version::new(1, 0, 0),
                contracts: semver::VersionReq::parse(contracts).unwrap(),
                contracts_raw: contracts.into(),
                entry: "main.php".into(),
            },
            dir: "/x".into(),
            entry: "/x/main.php".into(),
        }
    }

    struct Fake {
        fail: &'static str,
        called: RefCell<Vec<String>>,
    }

    impl Loader for Fake {
        fn load(&self, plugin: &Plugin) -> Result<(), String> {
            self.called.borrow_mut().push(plugin.manifest.name.clone());
            if plugin.manifest.name == self.fail {
                Err("boom".into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn skips_plugin_whose_contract_range_is_not_met() {
        let loader = Fake {
            fail: "",
            called: RefCell::new(vec![]),
        };
        let report = load_all(
            vec![plugin("a/a", "^2.0"), plugin("b/b", "^1.0")],
            &semver::Version::new(1, 0, 0),
            &loader,
        );
        assert_eq!(*loader.called.borrow(), ["b/b"]);
        assert_eq!(report.loaded.len(), 1);
        assert_eq!(report.skipped.len(), 1);
        match &report.skipped[0] {
            PluginError::Contracts {
                name,
                needs,
                bundled,
            } => {
                assert_eq!(
                    (name.as_str(), needs.as_str(), bundled.as_str()),
                    ("a/a", "^2.0", "1.0.0")
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn one_failing_plugin_does_not_stop_others() {
        let loader = Fake {
            fail: "a/a",
            called: RefCell::new(vec![]),
        };
        let report = load_all(
            vec![plugin("a/a", "^1.0"), plugin("b/b", "^1.0")],
            &semver::Version::new(1, 0, 0),
            &loader,
        );
        assert_eq!(report.loaded.len(), 1);
        assert_eq!(report.loaded[0].manifest.name, "b/b");
        assert_eq!(report.skipped.len(), 1);
        assert!(
            matches!(&report.skipped[0], PluginError::Load { name, problem } if name == "a/a" && problem == "boom")
        );
    }
}
