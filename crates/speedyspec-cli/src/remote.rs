//! The CLI side of work on a Kubernetes install: the runner pod beside the app's pod, with this release's own Linux
//! binary in it, checked twice (against the release's `SHA256SUMS` here, with `sha256sum` in the pod).

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Arg, ArgMatches};
use sha2::{Digest, Sha256};
use speedyspec_driver::Workload;
use speedyspec_driver_kubernetes::{RunnerPod, RunnerSpec, place_runner};
use speedyspec_fetch::{Fetch, HttpFetch, github_api_base};
use speedyspec_selfupdate::{Version, asset_name, download_checked, release_of};

use crate::app_target::{self, Target};
use crate::boot::Runtime;

/// A small image that only sleeps, pinned by digest so it is the same image everywhere.
pub const DEFAULT_RUNNER_IMAGE: &str =
    "busybox@sha256:a7dbc319b1438115a207e11930dc4bb3e643dcb3ec0bdd694bb36cd8fad289ec";
/// The longest a runner pod lives; a CLI killed outright leaves it at most this long, then a minute more.
const DEADLINE: Duration = Duration::from_secs(60 * 60);
/// How long the runner pod may take to start, its image pulled included.
const START_WAIT: Duration = Duration::from_secs(5 * 60);

/// `--runner-binary` and `--runner-image`.
pub fn runner_args() -> Vec<Arg> {
    vec![
        Arg::new("runner-binary")
            .long("runner-binary")
            .value_name("FILE")
            .help("A static Linux build of this speedyspec for the node, instead of the release's [kubernetes]"),
        Arg::new("runner-image")
            .long("runner-image")
            .value_name("IMAGE")
            .help("The image the runner pod sleeps in (default: a pinned busybox) [kubernetes]"),
    ]
}

/// How the runner pod is made, from the command's flags.
#[derive(Clone, Debug, Default)]
pub struct RunnerOptions {
    pub binary: Option<PathBuf>,
    pub image: Option<String>,
}

impl RunnerOptions {
    pub fn from_matches(matches: &ArgMatches) -> RunnerOptions {
        let get = |name: &str| matches.try_get_one::<String>(name).ok().flatten().cloned();
        RunnerOptions {
            binary: get("runner-binary").map(PathBuf::from),
            image: get("runner-image"),
        }
    }
}

/// The binary the runner pod runs, and its SHA-256. A downloaded one is deleted when this goes.
pub struct RunnerBinary {
    pub path: PathBuf,
    pub sha256: String,
    _download: Option<(tempfile::TempDir, tempfile::TempPath)>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Rust's name for a Kubernetes node architecture.
fn rust_arch(architecture: &str) -> Option<&'static str> {
    match architecture {
        "amd64" => Some("x86_64"),
        "arm64" => Some("aarch64"),
        _ => None,
    }
}

/// The binary for a node of `architecture`: `local` when given (`--runner-binary`), otherwise this version's
/// release asset, checked against the release's `SHA256SUMS`.
pub fn resolve_binary(
    architecture: &str,
    local: Option<&Path>,
    fetch: &dyn Fetch,
    api_base: &str,
    version: &str,
) -> Result<RunnerBinary, String> {
    if let Some(path) = local {
        let bytes =
            std::fs::read(path).map_err(|e| format!("--runner-binary {}: {e}", path.display()))?;
        return Ok(RunnerBinary {
            path: path.to_path_buf(),
            sha256: hex(&Sha256::digest(&bytes)),
            _download: None,
        });
    }
    let asset = rust_arch(architecture)
        .and_then(|arch| asset_name("linux", arch))
        .ok_or_else(|| {
            format!(
                "the node is {architecture}, and speedyspec is released for amd64 and arm64 only"
            )
        })?;
    let hint = format!(
        "; give --runner-binary with a static Linux build of speedyspec {version} for {architecture}"
    );
    let version =
        Version::parse(version).map_err(|e| format!("this speedyspec's version {version}: {e}"))?;
    let release = release_of(fetch, api_base, &version).map_err(|e| format!("{e}{hint}"))?;
    let dir = tempfile::tempdir()
        .map_err(|e| format!("cannot make a directory for the runner binary: {e}"))?;
    let (path, sha256) =
        download_checked(fetch, &release, &asset, dir.path()).map_err(|e| format!("{e}{hint}"))?;
    Ok(RunnerBinary {
        path: path.to_path_buf(),
        sha256,
        _download: Some((dir, path)),
    })
}

/// A runner pod beside a Kubernetes install's app, and the workload it is beside.
pub struct Remote<'t> {
    pub pod: RunnerPod<'t>,
    pub workload: Workload,
    _binary: RunnerBinary,
}

/// Starts the runner pod for `target`, which must be a Kubernetes install.
pub fn open<'t>(
    runtime: &Runtime,
    target: &'t Target,
    options: &RunnerOptions,
) -> Result<Remote<'t>, String> {
    let cluster = target.cluster.as_ref().ok_or_else(|| {
        format!(
            "install {} is not a Kubernetes install",
            target.install.name
        )
    })?;
    let workload = app_target::workload(runtime, target)?;
    let placement =
        place_runner(&cluster.connection, &cluster.namespace, &workload).map_err(|e| e.0)?;
    let binary = resolve_binary(
        &placement.architecture,
        options.binary.as_deref(),
        &HttpFetch::new(),
        &github_api_base()?,
        crate::self_cmd::VERSION,
    )?;
    let spec = RunnerSpec {
        image: options
            .image
            .clone()
            .unwrap_or_else(|| DEFAULT_RUNNER_IMAGE.to_owned()),
        deadline: DEADLINE,
        start_wait: START_WAIT,
        install: target.install.name.clone(),
    };
    let pod = RunnerPod::start(
        &cluster.connection,
        &cluster.namespace,
        &placement,
        &binary.path,
        &binary.sha256,
        &spec,
    )
    .map_err(|e| e.0)?;
    Ok(Remote {
        pod,
        workload,
        _binary: binary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use speedyspec_testkit::{Route, Server};

    fn release(server: &Server, sums: &str) -> String {
        format!(
            r#"{{"tag_name":"v0.9.0","assets":[
                {{"name":"speedyspec-linux-aarch64","browser_download_url":"{0}/bin","size":10}},
                {{"name":"SHA256SUMS","browser_download_url":"{0}/sums","size":{1}}}]}}"#,
            server.url(),
            sums.len()
        )
    }

    #[test]
    fn a_local_binary_is_hashed_here() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"binary").unwrap();
        let found = resolve_binary(
            "arm64",
            Some(file.path()),
            &HttpFetch::new(),
            "https://unused",
            "0.9.0",
        )
        .unwrap();
        assert_eq!(found.sha256, hex(&Sha256::digest(b"binary")));
        assert_eq!(found.path, file.path());
    }

    #[test]
    fn the_release_binary_is_checked_against_its_sums() {
        let good = format!(
            "{}  speedyspec-linux-aarch64\n",
            hex(&Sha256::digest(b"0123456789"))
        );
        let server = Server::start(vec![
            ("/bin", Route::ok("0123456789")),
            ("/sums", Route::ok(good.clone())),
        ]);
        let body = release(&server, &good);
        let api = Server::start(vec![(
            "/repos/SpeedySpec/speedyspec-cli/releases/tags/v0.9.0",
            Route::ok(body),
        )]);
        let found = resolve_binary("arm64", None, &HttpFetch::new(), &api.url(), "0.9.0").unwrap();
        assert_eq!(std::fs::read(&found.path).unwrap(), b"0123456789");
        assert_eq!(found.sha256, hex(&Sha256::digest(b"0123456789")));
    }

    #[test]
    fn a_release_binary_that_does_not_match_is_refused() {
        let wrong = format!("{}  speedyspec-linux-aarch64\n", "0".repeat(64));
        let server = Server::start(vec![
            ("/bin", Route::ok("0123456789")),
            ("/sums", Route::ok(wrong.clone())),
        ]);
        let body = release(&server, &wrong);
        let api = Server::start(vec![(
            "/repos/SpeedySpec/speedyspec-cli/releases/tags/v0.9.0",
            Route::ok(body),
        )]);
        let error = resolve_binary("arm64", None, &HttpFetch::new(), &api.url(), "0.9.0")
            .err()
            .unwrap();
        assert!(
            error.contains("does not match") && error.contains("--runner-binary"),
            "{error}"
        );
    }

    #[test]
    fn an_unreleased_version_says_to_give_a_binary() {
        let api = Server::start(vec![]);
        let error = resolve_binary("amd64", None, &HttpFetch::new(), &api.url(), "0.9.1")
            .err()
            .unwrap();
        assert!(
            error.contains("no release v0.9.1") && error.contains("--runner-binary"),
            "{error}"
        );
        let error = resolve_binary("s390x", None, &HttpFetch::new(), &api.url(), "0.9.1")
            .err()
            .unwrap();
        assert!(error.contains("s390x"), "{error}");
    }
}
