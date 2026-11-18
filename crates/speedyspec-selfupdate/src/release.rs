//! The latest release, as GitHub describes it.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Write};

pub use semver::Version;
use speedyspec_fetch::{Fetch, FetchError};

/// Where speedyspec is released.
pub const REPOSITORY: &str = "SpeedySpec/speedyspec-cli";
/// The release asset listing the SHA-256 of every binary.
pub const SUMS: &str = "SHA256SUMS";
/// The largest release description read.
const MAX_DESCRIPTION: u64 = 1024 * 1024;

/// Why an update could not be done, as one message for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateError(pub String);

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UpdateError {}

/// The name of the release binary for an operating system and architecture, as Rust names them
/// (`std::env::consts::OS` and `ARCH`), if there is one.
pub fn asset_name(os: &str, arch: &str) -> Option<String> {
    let os_known = matches!(os, "linux" | "macos");
    let arch_known = matches!(arch, "x86_64" | "aarch64");
    (os_known && arch_known).then(|| format!("speedyspec-{os}-{arch}"))
}

/// The release binary for the machine this runs on.
pub fn current_asset() -> Option<String> {
    asset_name(std::env::consts::OS, std::env::consts::ARCH)
}

/// One file attached to a release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    /// In bytes, as GitHub reports it.
    pub size: u64,
}

/// A published release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub version: Version,
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|asset| asset.name == name)
    }
}

/// Where the running version stands against the latest release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    UpToDate(Version),
    /// The running version is newer than any release (a development build).
    Ahead {
        latest: Version,
    },
    Newer(Release),
}

/// A writer that refuses to take more than `limit` bytes.
pub(crate) struct Limited<W> {
    pub(crate) inner: W,
    pub(crate) limit: u64,
    pub(crate) written: u64,
    /// Set once a write was refused for going past `limit`.
    pub(crate) refused: bool,
}

impl<W> Limited<W> {
    pub(crate) fn new(inner: W, limit: u64) -> Self {
        Limited {
            inner,
            limit,
            written: 0,
            refused: false,
        }
    }
}

impl<W: Write> Write for Limited<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written + buf.len() as u64 > self.limit {
            self.refused = true;
            return Err(io::Error::other(format!("more than {} bytes", self.limit)));
        }
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The latest release (GitHub leaves out drafts and pre-releases).
pub fn latest_release(fetch: &dyn Fetch, api_base: &str) -> Result<Release, UpdateError> {
    let url = format!("{api_base}/repos/{REPOSITORY}/releases/latest");
    let mut body = Limited::new(Vec::new(), MAX_DESCRIPTION);
    match fetch.fetch(&url, &mut body) {
        Ok(_) => {}
        Err(FetchError::Status { status: 404, .. }) => {
            return Err(UpdateError(
                "no release of speedyspec has been published yet".to_owned(),
            ));
        }
        Err(FetchError::Status {
            status: status @ (403 | 429),
            ..
        }) => {
            return Err(UpdateError(format!(
                "GitHub refused the request (HTTP {status}); it limits anonymous requests, so \
                 set GITHUB_TOKEN and try again"
            )));
        }
        Err(error) => {
            return Err(UpdateError(format!(
                "could not ask GitHub for the latest release: {error}"
            )));
        }
    }
    let json: serde_json::Value = serde_json::from_slice(&body.inner).map_err(|e| {
        UpdateError(format!(
            "the description of the latest release could not be read: {e}"
        ))
    })?;
    let tag = json["tag_name"].as_str().unwrap_or_default().to_owned();
    let version = Version::parse(tag.strip_prefix('v').unwrap_or(&tag)).map_err(|_| {
        UpdateError(format!(
            "the latest release has the tag \"{tag}\", which is not a version"
        ))
    })?;
    let assets = json["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|asset| {
            Some(Asset {
                name: asset["name"].as_str()?.to_owned(),
                url: asset["browser_download_url"].as_str()?.to_owned(),
                size: asset["size"].as_u64()?,
            })
        })
        .collect();
    Ok(Release {
        tag,
        version,
        assets,
    })
}

/// How `current` compares with the latest release.
pub fn check(fetch: &dyn Fetch, api_base: &str, current: &Version) -> Result<Check, UpdateError> {
    let release = latest_release(fetch, api_base)?;
    Ok(match release.version.cmp(current) {
        std::cmp::Ordering::Greater => Check::Newer(release),
        std::cmp::Ordering::Equal => Check::UpToDate(release.version),
        std::cmp::Ordering::Less => Check::Ahead {
            latest: release.version,
        },
    })
}

/// Reads a `SHA256SUMS` file (`sha256sum` output): file name to lowercase hex digest.
pub fn parse_sums(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut sums = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        let malformed = || format!("line {number} of SHA256SUMS is not \"<sha256>  <name>\"");
        let (digest, rest) = line.split_at_checked(64).ok_or_else(malformed)?;
        if !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(malformed());
        }
        let name = rest
            .strip_prefix("  ")
            .or_else(|| rest.strip_prefix(" *"))
            .filter(|name| !name.is_empty())
            .ok_or_else(malformed)?;
        if sums
            .insert(name.to_owned(), digest.to_ascii_lowercase())
            .is_some()
        {
            return Err(format!("line {number} of SHA256SUMS lists {name} again"));
        }
    }
    Ok(sums)
}

#[cfg(test)]
mod tests {
    use super::*;
    use speedyspec_fetch::{Fetch, FetchError};
    use std::cell::RefCell;
    use std::io::Write;

    const API: &str = "https://api.github.com";

    /// Answers every request with one body or one HTTP status, and remembers what was asked.
    struct FakeApi {
        reply: Result<String, u16>,
        requests: RefCell<Vec<String>>,
    }

    impl FakeApi {
        fn body(body: &str) -> FakeApi {
            FakeApi {
                reply: Ok(body.to_owned()),
                requests: RefCell::default(),
            }
        }

        fn status(status: u16) -> FakeApi {
            FakeApi {
                reply: Err(status),
                requests: RefCell::default(),
            }
        }
    }

    impl Fetch for FakeApi {
        fn fetch(&self, url: &str, to: &mut dyn Write) -> Result<u64, FetchError> {
            self.requests.borrow_mut().push(url.to_owned());
            match &self.reply {
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

    fn release_json(tag: &str) -> String {
        format!(
            r#"{{"tag_name":"{tag}","assets":[
                {{"name":"SHA256SUMS","browser_download_url":"https://x/SHA256SUMS","size":400}},
                {{"name":"speedyspec-linux-x86_64","browser_download_url":"https://x/linux","size":123}}]}}"#
        )
    }

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn asset_names() {
        assert_eq!(
            asset_name("linux", "x86_64").as_deref(),
            Some("speedyspec-linux-x86_64")
        );
        assert_eq!(
            asset_name("macos", "aarch64").as_deref(),
            Some("speedyspec-macos-aarch64")
        );
        assert_eq!(asset_name("windows", "x86_64"), None);
        assert_eq!(asset_name("linux", "riscv64"), None);
    }

    #[test]
    fn reads_the_latest_release() {
        let api = FakeApi::body(&release_json("v0.3.0"));
        let release = latest_release(&api, API).unwrap();
        assert_eq!(release.version, v("0.3.0"));
        assert_eq!(release.tag, "v0.3.0");
        let linux = release.asset("speedyspec-linux-x86_64").unwrap();
        assert_eq!((linux.url.as_str(), linux.size), ("https://x/linux", 123));
        assert!(release.asset(SUMS).is_some());
        assert_eq!(
            *api.requests.borrow(),
            vec!["https://api.github.com/repos/SpeedySpec/speedyspec-cli/releases/latest"]
        );
    }

    #[test]
    fn compares_versions() {
        let current = v("0.2.0");
        let newer = check(&FakeApi::body(&release_json("v0.3.0")), API, &current).unwrap();
        assert!(matches!(newer, Check::Newer(release) if release.version == v("0.3.0")));
        let same = check(&FakeApi::body(&release_json("v0.2.0")), API, &current).unwrap();
        assert!(matches!(same, Check::UpToDate(version) if version == v("0.2.0")));
        let older = check(&FakeApi::body(&release_json("0.1.9")), API, &current).unwrap();
        assert!(matches!(older, Check::Ahead { latest } if latest == v("0.1.9")));
    }

    #[test]
    fn no_release_yet() {
        let error = latest_release(&FakeApi::status(404), API).unwrap_err();
        assert_eq!(error.0, "no release of speedyspec has been published yet");
    }

    #[test]
    fn rate_limited() {
        let error = latest_release(&FakeApi::status(403), API).unwrap_err();
        assert_eq!(
            error.0,
            "GitHub refused the request (HTTP 403); it limits anonymous requests, so set \
             GITHUB_TOKEN and try again"
        );
        let error = latest_release(&FakeApi::status(429), API).unwrap_err();
        assert!(
            error
                .0
                .starts_with("GitHub refused the request (HTTP 429);"),
            "{error}"
        );
    }

    #[test]
    fn unreadable_tag() {
        let error = latest_release(&FakeApi::body(&release_json("nightly")), API).unwrap_err();
        assert_eq!(
            error.0,
            "the latest release has the tag \"nightly\", which is not a version"
        );
    }

    #[test]
    fn sums_parse() {
        let text = format!(
            "{}  speedyspec-linux-x86_64\n{} *speedyspec-macos-aarch64\n\n",
            "A".repeat(64),
            "b".repeat(64)
        );
        let sums = parse_sums(&text).unwrap();
        assert_eq!(sums["speedyspec-linux-x86_64"], "a".repeat(64));
        assert_eq!(sums.len(), 2);
        assert_eq!(
            parse_sums("abc  x\n").unwrap_err(),
            "line 1 of SHA256SUMS is not \"<sha256>  <name>\""
        );
        let twice = format!("{0}  x\n{0}  x\n", "a".repeat(64));
        assert_eq!(
            parse_sums(&twice).unwrap_err(),
            "line 2 of SHA256SUMS lists x again"
        );
    }
}
