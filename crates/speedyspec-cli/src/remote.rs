//! The CLI side of work on a Kubernetes install: the runner pod beside the app's pod, with this release's own Linux
//! binary in it, checked twice (against the release's `SHA256SUMS` here, with `sha256sum` in the pod).

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Arg, ArgMatches};
use serde_json::json;
use sha2::{Digest, Sha256};
use speedyspec_driver::{AppRuntime, AppTask, Workload};
use speedyspec_driver_kubernetes::{RUNNER_DIR, RunnerPod, RunnerSpec, place_runner};
use speedyspec_fetch::{Fetch, HttpFetch, github_api_base};
use speedyspec_selfupdate::{Version, asset_name, download_checked, release_of};

use crate::app_target::{self, Target};
use crate::boot::Runtime;
use crate::runner::{RunnerError, TaskOutcome};

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
    /// Plugin directories already copied into the pod, and where they are there.
    plugins: RefCell<Vec<(PathBuf, String)>>,
    /// For unique file names in the pod.
    files: Cell<u32>,
}

/// The plugin directory a task file belongs to (the nearest one up that has `speedyspec-plugin.json`, or the task's
/// own directory), and the task's path inside it.
fn plugin_of(file: &Path) -> (PathBuf, PathBuf) {
    let parent = file.parent().unwrap_or(Path::new("/"));
    let root = parent
        .ancestors()
        .find(|dir| dir.join("speedyspec-plugin.json").is_file())
        .unwrap_or(parent);
    let relative = file.strip_prefix(root).unwrap_or(file).to_path_buf();
    (root.to_path_buf(), relative)
}

impl Remote<'_> {
    /// A new private path under the pod's runner directory.
    pub fn private_path(&self, what: &str) -> String {
        let n = self.files.get() + 1;
        self.files.set(n);
        format!("{RUNNER_DIR}/{what}-{n}")
    }

    /// Where the plugin directory `root` is in the pod, copying it there the first time.
    fn plugin_in_pod(&self, root: &Path) -> Result<String, String> {
        if let Some((_, there)) = self.plugins.borrow().iter().find(|(here, _)| here == root) {
            return Ok(there.clone());
        }
        let there = self.private_path("plugin");
        let mut archive =
            tempfile::tempfile().map_err(|e| format!("cannot pack {}: {e}", root.display()))?;
        {
            let mut builder = tar::Builder::new(&mut archive);
            builder.follow_symlinks(false);
            builder
                .append_dir_all(".", root)
                .and_then(|()| builder.finish())
                .map_err(|e| format!("cannot pack {}: {e}", root.display()))?;
        }
        use std::io::Seek;
        archive
            .rewind()
            .map_err(|e| format!("cannot pack {}: {e}", root.display()))?;
        let mut err = Vec::new();
        let code = self
            .pod
            .command(
                &[
                    "sh",
                    "-c",
                    "mkdir -p \"$1\" && tar -x -C \"$1\"",
                    "sh",
                    &there,
                ],
                Some(&mut archive),
                &mut std::io::sink(),
                &mut err,
            )
            .map_err(|e| e.0)?;
        if code != 0 {
            return Err(format!(
                "could not copy {} into the runner pod: {}",
                root.display(),
                String::from_utf8_lossy(&err).trim()
            ));
        }
        self.plugins
            .borrow_mut()
            .push((root.to_path_buf(), there.clone()));
        Ok(there)
    }

    /// The request a runner pod's `self db` reads the connection from: the connection as `DatabaseConnection` has it,
    /// password included (the request goes in as a private file the pod deletes on reading). A CA file to verify the
    /// server with is copied into the pod and named there.
    pub fn connection_request(
        &self,
        settings: &speedyspec_driver::DatabaseConnection,
    ) -> Result<serde_json::Value, String> {
        let (tls, ca_file) = match &settings.tls {
            speedyspec_driver::Tls::Off => ("off", None),
            speedyspec_driver::Tls::Required => ("required", None),
            speedyspec_driver::Tls::Verify(ca) => {
                let there = self.private_path("ca.pem");
                let mut file = std::fs::File::open(ca)
                    .map_err(|e| format!("the CA certificate {}: {e}", ca.display()))?;
                self.pod.put(&there, &mut file, 0o600).map_err(|e| e.0)?;
                ("verify", Some(there))
            }
        };
        Ok(json!({
            "host": settings.host,
            "database": settings.database,
            "user": settings.user,
            "password": settings.password,
            "port": settings.port,
            "socket": settings.socket,
            "tablePrefix": settings.table_prefix,
            "charset": settings.charset,
            "tls": tls,
            "caFile": ca_file,
        }))
    }

    /// Runs `task` in the app at the workload's root, in the runner pod, as `runner::run` does on this machine. The
    /// request (with the task's input) goes in as a private file the pod deletes on reading.
    pub fn run_task(&self, app: &AppRuntime, task: &AppTask) -> Result<TaskOutcome, RunnerError> {
        let name = task.file.display().to_string();
        let fail = |message: String| RunnerError(format!("the task {name}: {message}"));
        let (plugin, relative) = plugin_of(&task.file);
        let dir = self.plugin_in_pod(&plugin).map_err(fail)?;
        let map = |pairs: &[(String, String)]| -> serde_json::Value {
            serde_json::Value::Object(
                pairs
                    .iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                    .collect(),
            )
        };
        let request = json!({
            "root": self.workload.root,
            "entry": app.entry,
            "ini": map(&app.ini),
            "server": map(&app.server),
            "file": format!("{dir}/{}", relative.display()),
            "input": task.input,
            "loads_app": task.loads_app,
            "timeout": task.timeout.as_secs(),
        })
        .to_string();
        let path = self.private_path("task.json");
        self.pod
            .put(&path, &mut request.as_bytes(), 0o600)
            .map_err(|e| fail(e.0))?;
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let ran = self.pod.run(
            &["self", "task-request", "--request", &path],
            None,
            &mut out,
            &mut err,
        );
        // `self task-request` deletes it on reading; this is for when it never ran.
        let _ = self.pod.remove(&path);
        let code = ran.map_err(|e| fail(e.0))?;
        let reply: serde_json::Value = serde_json::from_slice(&out).map_err(|_| {
            fail(format!(
                "the runner pod gave no answer (exit {code}): {}",
                String::from_utf8_lossy(&err).trim()
            ))
        })?;
        if reply["ok"] == serde_json::Value::Bool(true) {
            return Ok(TaskOutcome::from_json(&reply["outcome"]));
        }
        Err(RunnerError(
            reply["error"]
                .as_str()
                .unwrap_or("the task failed")
                .to_owned(),
        ))
    }
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
        plugins: RefCell::new(Vec::new()),
        files: Cell::new(0),
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
