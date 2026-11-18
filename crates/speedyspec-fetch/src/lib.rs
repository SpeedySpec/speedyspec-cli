//! Downloading over HTTPS, shared by the zip driver, `plugin install` and `self update`.

use std::fmt;
use std::io::{self, Write};
use std::time::Duration;

use speedyspec_driver::is_fetchable_url;

/// The GitHub API, unless `SPEEDYSPEC_GITHUB_API` names another (tests point it at a local server).
const GITHUB_API: &str = "https://api.github.com";

/// The base URL of the GitHub API to use, without a trailing slash.
pub fn github_api_base() -> Result<String, String> {
    match std::env::var("SPEEDYSPEC_GITHUB_API") {
        Ok(base) if is_fetchable_url(&base) => Ok(base.trim_end_matches('/').to_owned()),
        Ok(base) if !base.is_empty() => Err(format!(
            "SPEEDYSPEC_GITHUB_API must be an https:// URL; got \"{base}\""
        )),
        _ => Ok(GITHUB_API.to_owned()),
    }
}

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
    /// Waits a minute for a server to start answering and half an hour for a body.
    pub fn new() -> HttpFetch {
        Self::with_timeouts(Duration::from_secs(60), Duration::from_secs(30 * 60))
    }

    /// As `new`, giving up after `response` without an answer and after `body` receiving one
    /// body, so a server that stalls cannot hang the command for ever.
    pub fn with_timeouts(response: Duration, body: Duration) -> HttpFetch {
        let agent = ureq::Agent::config_builder()
            // Redirects are followed here, not by the client, so each hop is checked.
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .user_agent(concat!("speedyspec/", env!("CARGO_PKG_VERSION")))
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(response))
            .timeout_recv_body(Some(body))
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
        let mut was_secure = false;
        for _ in 0..=MAX_REDIRECTS {
            if !hop_allowed(was_secure, &current) {
                return Err(FetchError::Insecure {
                    url: url.to_owned(),
                    to: current,
                });
            }
            was_secure |= current.starts_with("https://");
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

/// Whether the next hop of a download may be fetched: it must pass the URL rule, and once any
/// hop was `https` no later one may be plain `http`, not even to this machine (a remote server
/// must not be able to bounce the CLI onto a local service).
fn hop_allowed(was_secure: bool, url: &str) -> bool {
    is_fetchable_url(url) && !(was_secure && url.starts_with("http://"))
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
    fn github_api_base_defaults_and_overrides() {
        // SAFETY: the only test in this crate that touches the variable.
        unsafe { std::env::remove_var("SPEEDYSPEC_GITHUB_API") };
        assert_eq!(github_api_base().unwrap(), "https://api.github.com");
        unsafe { std::env::set_var("SPEEDYSPEC_GITHUB_API", "http://127.0.0.1:9") };
        assert_eq!(github_api_base().unwrap(), "http://127.0.0.1:9");
        unsafe { std::env::set_var("SPEEDYSPEC_GITHUB_API", "http://example.com") };
        assert!(
            github_api_base()
                .unwrap_err()
                .contains("SPEEDYSPEC_GITHUB_API must be an https:// URL")
        );
        unsafe { std::env::remove_var("SPEEDYSPEC_GITHUB_API") };
    }

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
    fn plain_http_is_refused_once_a_hop_was_https() {
        assert!(hop_allowed(false, "https://x.test/a.zip"));
        assert!(hop_allowed(true, "https://y.test/a.zip"));
        assert!(hop_allowed(false, "http://127.0.0.1:8080/a.zip"));
        assert!(!hop_allowed(true, "http://127.0.0.1:8080/a.zip"));
        assert!(!hop_allowed(true, "http://localhost/a.zip"));
        assert!(!hop_allowed(false, "http://example.com/a.zip"));
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
