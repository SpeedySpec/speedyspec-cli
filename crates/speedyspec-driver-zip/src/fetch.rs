//! Downloading over HTTPS.

use std::fmt;
use std::io::{self, Write};
use std::time::Duration;

use speedyspec_driver::is_fetchable_url;

/// How many redirects one download follows.
const MAX_REDIRECTS: usize = 10;

/// Why a download failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchError {
    /// The server answered with something other than success.
    Status {
        url: String,
        status: u16,
    },
    /// `url` would have been fetched from `to`, which is not `https` (nor this machine).
    Insecure {
        url: String,
        to: String,
    },
    TooManyRedirects {
        url: String,
    },
    /// The connection failed or the body was cut short.
    Io {
        url: String,
        message: String,
    },
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Status { url, status } => write!(f, "{url}: the server answered HTTP {status}"),
            Self::Insecure { url, to } if url == to => {
                write!(f, "{url}: only https:// URLs are downloaded")
            }
            Self::Insecure { url, to } => {
                write!(f, "{url}: redirected to {to}, which is not an https:// URL")
            }
            Self::TooManyRedirects { url } => {
                write!(f, "{url}: more than {MAX_REDIRECTS} redirects")
            }
            Self::Io { url, message } => write!(f, "{url}: {message}"),
        }
    }
}

impl std::error::Error for FetchError {}

/// Fetches the body at a URL into a writer. A trait so installs can be tested without a network.
pub trait Fetch {
    /// Writes the body to `to` and returns how many bytes that was.
    fn fetch(&self, url: &str, to: &mut dyn Write) -> Result<u64, FetchError>;
}

/// The real thing: HTTPS with the root certificates compiled in, redirects followed by hand so
/// every hop is checked.
pub struct HttpFetch {
    agent: ureq::Agent,
    github_token: Option<String>,
}

impl HttpFetch {
    pub fn new() -> HttpFetch {
        let agent = ureq::Agent::config_builder()
            // Redirects are followed here, not by the client, so each hop is checked.
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .user_agent(concat!("speedyspec/", env!("CARGO_PKG_VERSION")))
            .timeout_connect(Some(Duration::from_secs(30)))
            .build()
            .into();
        HttpFetch {
            agent,
            github_token: std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty()),
        }
    }
}

impl Default for HttpFetch {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetch for HttpFetch {
    fn fetch(&self, url: &str, to: &mut dyn Write) -> Result<u64, FetchError> {
        let io_error = |message: String| FetchError::Io {
            url: url.to_owned(),
            message,
        };
        let mut current = url.to_owned();
        for _ in 0..=MAX_REDIRECTS {
            if !is_fetchable_url(&current) {
                return Err(FetchError::Insecure {
                    url: url.to_owned(),
                    to: current,
                });
            }
            let mut request = self.agent.get(&current);
            if let Some(header) = authorization(&current, self.github_token.as_deref()) {
                request = request.header("Authorization", header);
            }
            let mut response = request.call().map_err(|e| io_error(e.to_string()))?;
            let status = response.status().as_u16();
            if (300..400).contains(&status) {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| io_error(format!("HTTP {status} without a Location")))?;
                current = resolve(&current, location);
                continue;
            }
            if !(200..300).contains(&status) {
                return Err(FetchError::Status {
                    url: url.to_owned(),
                    status,
                });
            }
            let mut body = response.body_mut().as_reader();
            return io::copy(&mut body, to).map_err(|e| io_error(format!("download failed: {e}")));
        }
        Err(FetchError::TooManyRedirects {
            url: url.to_owned(),
        })
    }
}

/// The `Authorization` header for `url`, if it gets one: the GitHub token goes to the GitHub
/// API (which rate-limits anonymous callers) and to no other host, redirects included.
fn authorization(url: &str, github_token: Option<&str>) -> Option<String> {
    let token = github_token?;
    let rest = url.strip_prefix("https://api.github.com")?;
    (rest.is_empty() || rest.starts_with(['/', '?'])).then(|| format!("Bearer {token}"))
}

/// The URL a `Location` header points at, seen from `base`.
fn resolve(base: &str, location: &str) -> String {
    if location.contains("://") {
        return location.to_owned();
    }
    let scheme_end = base.find("://").map_or(0, |i| i + 3);
    let authority_end = base[scheme_end..]
        .find(['/', '?', '#'])
        .map_or(base.len(), |i| scheme_end + i);
    let origin = &base[..authority_end];
    if location.starts_with('/') {
        return format!("{origin}{location}");
    }
    // Relative to the directory of the base path.
    let path = base[authority_end..].split(['?', '#']).next().unwrap_or("");
    let directory = path.rfind('/').map_or("/", |i| &path[..=i]);
    format!("{origin}{directory}{location}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_goes_only_to_the_github_api() {
        let token = Some("t0ken");
        assert_eq!(
            authorization("https://api.github.com/repos/a/b/releases/latest", token).as_deref(),
            Some("Bearer t0ken")
        );
        for elsewhere in [
            "https://github.com/a/b/archive/x.zip",
            "https://objects.githubusercontent.com/x",
            "http://127.0.0.1:9/x",
            "https://api.github.com.evil.test/x",
            "https://api.github.com@evil.test/x",
            "http://api.github.com/x",
        ] {
            assert_eq!(authorization(elsewhere, token), None, "{elsewhere}");
        }
        assert_eq!(authorization("https://api.github.com/x", None), None);
    }

    #[test]
    fn resolves_locations() {
        let base = "https://h.test/a/b.zip?x=1";
        assert_eq!(resolve(base, "https://o.test/z"), "https://o.test/z");
        assert_eq!(resolve(base, "/z"), "https://h.test/z");
        assert_eq!(resolve(base, "c.zip"), "https://h.test/a/c.zip");
        assert_eq!(resolve("https://h.test", "c.zip"), "https://h.test/c.zip");
    }
}
