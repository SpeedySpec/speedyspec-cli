//! Turning the user's flags into a connection to one cluster.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Read;
use std::time::Duration;

use kube::Client;
use kube::api::ApiResource;
use kube::config::{Config, KubeConfigOptions, Kubeconfig};
use speedyspec_driver::is_fetchable_url;
use tokio::runtime::Runtime;

/// What the user gave to say which cluster, and as whom.
pub struct ConnectFlags<'a> {
    pub server: Option<&'a str>,
    pub token_file: Option<&'a str>,
    pub ca_cert: Option<&'a str>,
    pub insecure_skip_tls_verify: bool,
    pub context: Option<&'a str>,
}

/// Why there is no connection.
#[derive(Debug)]
pub enum ConnectError {
    /// The flags are wrong or incomplete.
    Usage(String),
    Failure(String),
}

/// A connection to one cluster. Everything it does blocks: the client is asynchronous, and the
/// runtime that drives it lives here and nowhere else.
pub struct Connection {
    pub(crate) runtime: Runtime,
    pub(crate) client: Client,
    /// What discovery has said about each kind asked for so far.
    pub(crate) kinds: RefCell<HashMap<(String, String), (ApiResource, bool)>>,
    /// How long an unknown kind is retried before it is reported.
    pub(crate) discovery_wait: Duration,
}

/// Builds a connection from the flags, the token in the environment, and stdin (for
/// `--token-file -`). Nothing is sent to the cluster yet.
pub fn connect(
    flags: &ConnectFlags<'_>,
    env_token: Option<&str>,
    stdin: &mut dyn Read,
) -> Result<Connection, ConnectError> {
    if flags.ca_cert.is_some() && flags.insecure_skip_tls_verify {
        return Err(ConnectError::Usage(
            "give --ca-cert or --insecure-skip-tls-verify, not both".to_owned(),
        ));
    }
    let token = token(flags.token_file, env_token, stdin)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ConnectError::Failure(format!("could not start the cluster client: {e}")))?;

    let mut config = match (flags.server, token) {
        (Some(server), Some(token)) => {
            if !is_fetchable_url(server) {
                return Err(ConnectError::Usage(format!(
                    "--server must be an https:// URL (http:// is accepted only for this \
                     machine); got \"{server}\""
                )));
            }
            let url = server.parse().map_err(|e| {
                ConnectError::Usage(format!("--server \"{server}\" is not a URL: {e}"))
            })?;
            let mut config = Config::new(url);
            config.auth_info.token = Some(token.into());
            config
        }
        (Some(_), None) => {
            return Err(ConnectError::Usage(
                "--server needs a token: set SPEEDYSPEC_KUBE_TOKEN, or give --token-file <path> \
                 (- reads it from standard input)"
                    .to_owned(),
            ));
        }
        (None, Some(_)) => {
            return Err(ConnectError::Usage(
                "a token was given without --server; say which cluster it is for".to_owned(),
            ));
        }
        (None, None) => from_kubeconfig(&runtime, flags.context)?,
    };
    if let Some(ca_cert) = flags.ca_cert {
        config.root_cert = None;
        config.root_cert_file = Some(ca_cert.into());
    }
    if flags.insecure_skip_tls_verify {
        config.accept_invalid_certs = true;
    }

    let client = runtime
        .block_on(async { Client::try_from(config) })
        .map_err(|e| ConnectError::Failure(format!("could not set up the cluster client: {e}")))?;
    Ok(Connection {
        runtime,
        client,
        kinds: RefCell::default(),
        discovery_wait: Duration::from_secs(30),
    })
}

/// The token to use, if one was given: the file (or stdin) wins over the environment.
fn token(
    token_file: Option<&str>,
    env_token: Option<&str>,
    stdin: &mut dyn Read,
) -> Result<Option<String>, ConnectError> {
    let (text, source) = match token_file {
        Some("-") => {
            let mut text = String::new();
            stdin.read_to_string(&mut text).map_err(|e| {
                ConnectError::Usage(format!("could not read the token from standard input: {e}"))
            })?;
            (text, "standard input".to_owned())
        }
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| {
                ConnectError::Usage(format!("could not read --token-file {path}: {e}"))
            })?;
            (text, format!("--token-file {path}"))
        }
        None => match env_token {
            Some(text) => (text.to_owned(), "SPEEDYSPEC_KUBE_TOKEN".to_owned()),
            None => return Ok(None),
        },
    };
    let text = text.trim();
    if text.is_empty() {
        return Err(ConnectError::Usage(format!(
            "the token from {source} is empty"
        )));
    }
    Ok(Some(text.to_owned()))
}

/// The cluster the user's kubeconfig points at, when no server and token were given.
fn from_kubeconfig(runtime: &Runtime, context: Option<&str>) -> Result<Config, ConnectError> {
    let kubeconfig = Kubeconfig::read().map_err(|_| {
        ConnectError::Usage(
            "no cluster to connect to: give --server and a token (SPEEDYSPEC_KUBE_TOKEN or \
             --token-file), or have a kubeconfig"
                .to_owned(),
        )
    })?;
    let options = KubeConfigOptions {
        context: context.map(str::to_owned),
        ..KubeConfigOptions::default()
    };
    runtime
        .block_on(Config::from_custom_kubeconfig(kubeconfig, &options))
        .map_err(|e| match context {
            Some(context) => ConnectError::Usage(format!("kubeconfig context \"{context}\": {e}")),
            None => ConnectError::Usage(format!("the kubeconfig cannot be used: {e}")),
        })
}

/// A client error as one line for the user. Never carries a request or response body.
pub(crate) fn explain(error: &kube::Error) -> String {
    match error {
        kube::Error::Api(status) if status.code == 401 => {
            "unauthorized: the cluster rejected the credentials".to_owned()
        }
        kube::Error::Api(status) if status.code == 403 => {
            format!("forbidden: {}", status.message)
        }
        kube::Error::Api(status) => status.message.clone(),
        other => format!("could not reach the cluster: {other}"),
    }
}

impl Connection {
    /// How long a kind the cluster does not know is retried before it is reported. A resource
    /// definition applied a moment ago takes a little while to be served.
    pub fn set_discovery_wait(&mut self, wait: Duration) {
        self.discovery_wait = wait;
    }

    /// The cluster's version, for example `v1.34.1`.
    pub fn server_version(&self) -> Result<String, String> {
        self.runtime
            .block_on(self.client.apiserver_version())
            .map(|info| info.git_version)
            .map_err(|e| explain(&e))
    }
}
