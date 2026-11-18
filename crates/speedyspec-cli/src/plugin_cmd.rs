//! `speedyspec plugin install` and `plugin remove`.
//!
//! Neither runs any PHP, so both work when an installed plugin is broken: they are how a broken
//! plugin gets replaced or removed.

use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use clap::ArgMatches;
use semver::Version;
use speedyspec_driver::is_fetchable_url;
use speedyspec_driver_zip::{Archive, Fetch, FetchError, HttpFetch, fetch_and_extract};
use speedyspec_plugin::{
    MANIFEST_FILE, directory_name, find_installed, parse_manifest, remove_installed,
    user_plugin_dir, valid_name,
};

use crate::boot::CONTRACTS_VERSION;
use crate::prompt::{Prompter, TerminalPrompter, interactive};

/// The release asset a plugin author publishes for the CLI to install.
const RELEASE_ASSET: &str = "speedyspec-plugin.zip";
const GITHUB_API: &str = "https://api.github.com";

/// Where a plugin comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Url(String),
    GitHub {
        owner: String,
        repo: String,
        tag: Option<String>,
    },
}

/// A source turned into the archive to download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub url: String,
    pub strip_components: u32,
    /// What the user is shown before confirming.
    pub describe: String,
}

/// Reads `<zip URL>` or `owner/repo[@tag]`.
pub fn parse_source(text: &str) -> Result<Source, String> {
    let wrong = || {
        format!(
            "\"{text}\" is not a plugin source; give an https:// URL of a zip archive, or \
             owner/repo or owner/repo@tag for a GitHub release"
        )
    };
    if text.contains("://") {
        return if is_fetchable_url(text) {
            Ok(Source::Url(text.to_owned()))
        } else {
            Err(wrong())
        };
    }
    let (path, tag) = match text.split_once('@') {
        Some((path, tag)) => (path, Some(tag)),
        None => (text, None),
    };
    let name = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    };
    let tag_ok = tag.is_none_or(|tag| {
        !tag.is_empty()
            && tag
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
    });
    match path.split('/').collect::<Vec<_>>()[..] {
        [owner, repo] if name(owner) && name(repo) && tag_ok => Ok(Source::GitHub {
            owner: owner.to_owned(),
            repo: repo.to_owned(),
            tag: tag.map(str::to_owned),
        }),
        _ => Err(wrong()),
    }
}

/// Finds the archive of a GitHub release: the asset named `speedyspec-plugin.zip` when the
/// release has one, otherwise its source archive (which wraps everything in one directory).
pub fn resolve_github(
    owner: &str,
    repo: &str,
    tag: Option<&str>,
    api_base: &str,
    fetch: &dyn Fetch,
) -> Result<Resolved, String> {
    let wanted = match tag {
        Some(tag) => format!("{owner}/{repo}@{tag}"),
        None => format!("{owner}/{repo}"),
    };
    let endpoint = match tag {
        Some(tag) => format!("{api_base}/repos/{owner}/{repo}/releases/tags/{tag}"),
        None => format!("{api_base}/repos/{owner}/{repo}/releases/latest"),
    };
    let mut body = Vec::new();
    match fetch.fetch(&endpoint, &mut body) {
        Ok(_) => {}
        Err(FetchError::Status { status: 404, .. }) => {
            return Err(format!("no release found for {wanted}"));
        }
        Err(error) => return Err(format!("{wanted}: {error}")),
    }
    let release: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| format!("{wanted}: the release description could not be read: {e}"))?;
    let asset = release["assets"].as_array().and_then(|assets| {
        assets
            .iter()
            .find(|asset| asset["name"] == RELEASE_ASSET)
            .and_then(|asset| asset["browser_download_url"].as_str())
    });
    let (url, strip_components) = match (asset, release["zipball_url"].as_str()) {
        (Some(url), _) => (url, 0),
        // GitHub wraps a source archive in one directory named after the commit.
        (None, Some(url)) => (url, 1),
        (None, None) => return Err(format!("{wanted}: the release has nothing to download")),
    };
    if !is_fetchable_url(url) {
        return Err(format!("{wanted}: refusing to download from {url}"));
    }
    // Shown to the user before they confirm, and written by whoever runs the server: nothing
    // in it may move the cursor or recolour the terminal.
    let tag_name: String = release["tag_name"]
        .as_str()
        .or(tag)
        .unwrap_or("latest")
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    Ok(Resolved {
        url: url.to_owned(),
        strip_components,
        describe: format!("{owner}/{repo} {tag_name} ({url})"),
    })
}

/// The user plugin directory, or the exit code after saying why there is none.
fn plugins_dir() -> Result<PathBuf, i32> {
    user_plugin_dir().dir.ok_or_else(|| {
        eprintln!(
            "speedyspec: there is no user plugin directory to install into; set \
             SPEEDYSPEC_HOME to an absolute path"
        );
        1
    })
}

/// Asks the user to confirm unless `--yes` was given. `Err` is the exit code.
fn confirm(matches: &ArgMatches, question: &str) -> Result<(), i32> {
    if matches.get_flag("yes") {
        return Ok(());
    }
    if !interactive(
        matches.get_flag("no-interaction"),
        std::io::stdin().is_terminal(),
    ) {
        eprintln!("speedyspec: confirm with --yes");
        return Err(2);
    }
    match TerminalPrompter.text(question, None, false) {
        Ok(answer) if matches!(answer.to_lowercase().as_str(), "y" | "yes") => Ok(()),
        Ok(_) => {
            eprintln!("speedyspec: cancelled");
            Err(1)
        }
        Err(error) => {
            eprintln!("speedyspec: {error}");
            Err(1)
        }
    }
}

/// `plugin install <source> [--yes]`.
pub fn install(matches: &ArgMatches) -> i32 {
    match try_install(matches) {
        Ok(()) => 0,
        Err(code) => code,
    }
}

fn try_install(matches: &ArgMatches) -> Result<(), i32> {
    let fail = |message: String| {
        eprintln!("speedyspec: {message}");
        1
    };
    let plugins = plugins_dir()?;
    let text = matches
        .get_one::<String>("source")
        .expect("clap requires the source");
    let source = parse_source(text).map_err(|message| {
        eprintln!("speedyspec: {message}");
        2
    })?;
    let fetch = HttpFetch::new();
    let resolved = match source {
        Source::Url(url) => Resolved {
            describe: url.clone(),
            url,
            strip_components: 0,
        },
        Source::GitHub { owner, repo, tag } => {
            let api_base = match std::env::var("SPEEDYSPEC_GITHUB_API") {
                Ok(base) if is_fetchable_url(&base) => base.trim_end_matches('/').to_owned(),
                Ok(base) if !base.is_empty() => {
                    return Err(fail(format!(
                        "SPEEDYSPEC_GITHUB_API must be an https:// URL; got \"{base}\""
                    )));
                }
                _ => GITHUB_API.to_owned(),
            };
            resolve_github(&owner, &repo, tag.as_deref(), &api_base, &fetch).map_err(fail)?
        }
    };

    eprintln!("speedyspec: this will download {}", resolved.describe);
    eprintln!("This plugin will run as you, with full access to your files and network.");
    confirm(matches, "Install it? [y/N]")?;

    fs::create_dir_all(&plugins).map_err(|e| fail(format!("{}: {e}", plugins.display())))?;
    // Removed when dropped, on every path out of this function.
    let staging = tempfile::Builder::new()
        .prefix(".speedyspec-staging-")
        .tempdir_in(&plugins)
        .map_err(|e| fail(format!("{}: {e}", plugins.display())))?;
    let unpacked = staging.path().join("new");
    fs::create_dir(&unpacked).map_err(|e| fail(format!("{}: {e}", unpacked.display())))?;
    let archive = Archive {
        url: &resolved.url,
        sha256: None,
        strip_components: resolved.strip_components,
    };
    fetch_and_extract(&archive, &unpacked, &fetch).map_err(|e| fail(e.to_string()))?;

    let root = plugin_root(&unpacked)
        .ok_or_else(|| fail(format!("the archive has no {MANIFEST_FILE}")))?;
    let manifest = parse_manifest(&root).map_err(|error| {
        // The staging path means nothing to the user; the problem does.
        fail(match error {
            speedyspec_plugin::PluginError::Manifest { problem, .. } => problem,
            other => other.to_string(),
        })
    })?;
    let bundled = Version::parse(CONTRACTS_VERSION)
        .map_err(|e| fail(format!("bundled contracts version is invalid: {e}")))?;
    if !manifest.contracts.matches(&bundled) {
        return Err(fail(format!(
            "{} needs contracts {}, this CLI bundles {bundled}",
            manifest.name, manifest.contracts_raw
        )));
    }

    // Only now, with a valid plugin in hand, is anything already installed touched.
    let target = plugins.join(directory_name(&manifest.name));
    let aside = staging.path().join("old");
    let previous = find_installed(&plugins, &manifest.name);
    if let Some(previous) = &previous {
        fs::rename(previous, &aside).map_err(|e| fail(format!("{}: {e}", previous.display())))?;
    }
    if let Err(error) = fs::rename(&root, &target) {
        if let Some(previous) = &previous {
            let _ = fs::rename(&aside, previous);
        }
        return Err(fail(format!("{}: {error}", target.display())));
    }

    let _ = writeln!(
        std::io::stdout().lock(),
        "{}\t{}\t{}",
        manifest.name,
        manifest.version,
        target.display()
    );
    Ok(())
}

/// The directory holding the manifest: what was unpacked, or its only subdirectory.
fn plugin_root(unpacked: &Path) -> Option<PathBuf> {
    if unpacked.join(MANIFEST_FILE).is_file() {
        return Some(unpacked.to_path_buf());
    }
    let mut entries = fs::read_dir(unpacked).ok()?.flatten();
    let only = entries.next()?.path();
    if entries.next().is_some() {
        return None;
    }
    let is_real_directory = fs::symlink_metadata(&only).is_ok_and(|found| found.is_dir());
    (is_real_directory && only.join(MANIFEST_FILE).is_file()).then_some(only)
}

/// `plugin remove <name> [--yes]`.
pub fn remove(matches: &ArgMatches) -> i32 {
    match try_remove(matches) {
        Ok(()) => 0,
        Err(code) => code,
    }
}

fn try_remove(matches: &ArgMatches) -> Result<(), i32> {
    let plugins = plugins_dir()?;
    let name = matches
        .get_one::<String>("name")
        .expect("clap requires the name");
    let fail = |error: speedyspec_plugin::PluginError| {
        eprintln!("speedyspec: {error}");
        1
    };
    if !valid_name(name) {
        eprintln!(
            "speedyspec: {}",
            speedyspec_plugin::PluginError::InvalidName { name: name.clone() }
        );
        return Err(2);
    }
    if find_installed(&plugins, name).is_none() {
        return Err(fail(speedyspec_plugin::PluginError::NotInstalled {
            name: name.clone(),
            dir: plugins,
        }));
    }
    confirm(matches, &format!("Remove {name}? [y/N]"))?;
    let removed = remove_installed(&plugins, name).map_err(fail)?;
    let _ = writeln!(std::io::stdout().lock(), "{name}\t{}", removed.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use speedyspec_driver_zip::FetchError;
    use std::cell::RefCell;
    use std::io::Write;

    /// Answers every request with one body (or a status) and records the URLs asked for.
    struct FakeApi {
        answer: Result<String, u16>,
        asked: RefCell<Vec<String>>,
    }

    impl FakeApi {
        fn json(body: &str) -> FakeApi {
            FakeApi {
                answer: Ok(body.to_owned()),
                asked: RefCell::default(),
            }
        }

        fn status(status: u16) -> FakeApi {
            FakeApi {
                answer: Err(status),
                asked: RefCell::default(),
            }
        }
    }

    impl Fetch for FakeApi {
        fn fetch(&self, url: &str, to: &mut dyn Write) -> Result<u64, FetchError> {
            self.asked.borrow_mut().push(url.to_owned());
            match &self.answer {
                Ok(body) => {
                    to.write_all(body.as_bytes()).unwrap();
                    Ok(body.len() as u64)
                }
                Err(status) => Err(FetchError::Status {
                    url: url.to_owned(),
                    status: *status,
                }),
            }
        }
    }

    const API: &str = "https://api.github.com";

    #[test]
    fn parses_sources() {
        assert_eq!(
            parse_source("https://x.test/p.zip"),
            Ok(Source::Url("https://x.test/p.zip".to_owned()))
        );
        assert_eq!(
            parse_source("http://127.0.0.1:8080/p.zip"),
            Ok(Source::Url("http://127.0.0.1:8080/p.zip".to_owned()))
        );
        assert_eq!(
            parse_source("acme/plugin"),
            Ok(Source::GitHub {
                owner: "acme".to_owned(),
                repo: "plugin".to_owned(),
                tag: None,
            })
        );
        assert_eq!(
            parse_source("Acme-Co/my.plugin_2@v1.2.0"),
            Ok(Source::GitHub {
                owner: "Acme-Co".to_owned(),
                repo: "my.plugin_2".to_owned(),
                tag: Some("v1.2.0".to_owned()),
            })
        );
        for bad in [
            "http://example.com/p.zip",
            "ftp://x.test/p.zip",
            "acme",
            "acme/plugin/extra",
            "../x/y",
            "acme/..",
            "acme/plugin@",
            "acme/plug in",
            "/abs/path.zip",
            "",
        ] {
            let error = parse_source(bad).unwrap_err();
            assert!(
                error.contains("owner/repo") && error.contains("https://"),
                "{bad:?}: {error}"
            );
        }
    }

    #[test]
    fn github_prefers_the_named_asset() {
        let api = FakeApi::json(
            r#"{"tag_name":"v1.2.0","zipball_url":"https://api.github.com/repos/acme/plugin/zipball/v1.2.0",
                "assets":[{"name":"other.zip","browser_download_url":"https://x.test/other.zip"},
                          {"name":"speedyspec-plugin.zip","browser_download_url":"https://x.test/asset.zip"}]}"#,
        );
        let resolved = resolve_github("acme", "plugin", None, API, &api).unwrap();
        assert_eq!(resolved.url, "https://x.test/asset.zip");
        assert_eq!(resolved.strip_components, 0);
        assert!(
            resolved.describe.contains("acme/plugin") && resolved.describe.contains("v1.2.0"),
            "{}",
            resolved.describe
        );
        assert_eq!(
            *api.asked.borrow(),
            vec!["https://api.github.com/repos/acme/plugin/releases/latest"]
        );
    }

    #[test]
    fn github_falls_back_to_the_source_archive() {
        let api = FakeApi::json(
            r#"{"tag_name":"v2","zipball_url":"https://api.github.com/repos/acme/plugin/zipball/v2","assets":[]}"#,
        );
        let resolved = resolve_github("acme", "plugin", None, API, &api).unwrap();
        assert_eq!(
            resolved.url,
            "https://api.github.com/repos/acme/plugin/zipball/v2"
        );
        assert_eq!(resolved.strip_components, 1);
    }

    #[test]
    fn github_tag_uses_the_tags_endpoint() {
        let api =
            FakeApi::json(r#"{"tag_name":"v1.2.0","zipball_url":"https://x.test/z","assets":[]}"#);
        resolve_github("acme", "plugin", Some("v1.2.0"), API, &api).unwrap();
        assert_eq!(
            *api.asked.borrow(),
            vec!["https://api.github.com/repos/acme/plugin/releases/tags/v1.2.0"]
        );
    }

    #[test]
    fn github_missing_release_is_reported() {
        let error = resolve_github("acme", "plugin", None, API, &FakeApi::status(404)).unwrap_err();
        assert_eq!(error, "no release found for acme/plugin");
        let error =
            resolve_github("acme", "plugin", Some("v9"), API, &FakeApi::status(404)).unwrap_err();
        assert_eq!(error, "no release found for acme/plugin@v9");
    }

    #[test]
    fn github_refuses_an_unusable_download_url() {
        let api =
            FakeApi::json(r#"{"tag_name":"v1","zipball_url":"http://evil.test/z","assets":[]}"#);
        let error = resolve_github("acme", "plugin", None, API, &api).unwrap_err();
        assert!(error.contains("http://evil.test/z"), "{error}");
    }

    #[test]
    fn github_tag_name_cannot_carry_control_characters() {
        let api = FakeApi::json(
            r#"{"tag_name":"v1\u001b[2Jevil\u0007","zipball_url":"https://x.test/z","assets":[]}"#,
        );
        let resolved = resolve_github("acme", "plugin", None, API, &api).unwrap();
        assert!(
            resolved.describe.contains("v1[2Jevil"),
            "{}",
            resolved.describe
        );
        assert!(
            !resolved.describe.chars().any(char::is_control),
            "{:?}",
            resolved.describe
        );
    }

    #[test]
    fn github_garbage_is_reported() {
        let error =
            resolve_github("acme", "plugin", None, API, &FakeApi::json("<html>")).unwrap_err();
        assert!(error.contains("acme/plugin"), "{error}");
    }
}
