//! The runner pod: a Job beside an app's pod, on its node and with its app container's volumes, that the CLI drives
//! through exec. It holds a small image that only sleeps; the CLI streams its own binary in, checks it, and runs it
//! there. The Job never outlives the command: it is deleted when the `RunnerPod` goes, on Ctrl-C, and, should the CLI
//! be killed outright, by its own deadline and time-to-live.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use k8s_openapi::api::batch::v1::Job;
use kube::api::{Api, DeleteParams, PropagationPolicy};
use serde_json::{Value as Json, json};
use speedyspec_driver::Workload;

use crate::cluster::ClusterError;
use crate::connect::Connection;
use crate::manifest::ResourceId;

/// Where the binary goes in the pod: an in-memory `emptyDir` only the runner sees.
pub const RUNNER_DIR: &str = "/speedyspec";
const BINARY: &str = "/speedyspec/speedyspec";
const CONTAINER: &str = "runner";
/// The service account token Kubernetes mounts itself: never copied, the runner needs no API access.
const SERVICE_ACCOUNT_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

/// Why the runner pod could not be placed, started or used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunnerError(pub String);

impl From<ClusterError> for RunnerError {
    fn from(error: ClusterError) -> RunnerError {
        RunnerError(error.message("the runner pod"))
    }
}

/// How the runner pod is made.
#[derive(Clone, Debug)]
pub struct RunnerSpec {
    /// The image that only sleeps.
    pub image: String,
    /// How long the Job may run in all; its pod sleeps that long.
    pub deadline: Duration,
    /// How long the pod may take to start.
    pub start_wait: Duration,
    /// The install it works on, for its labels and name.
    pub install: String,
}

/// Where the runner pod goes: beside a ready pod of the workload.
#[derive(Clone, Debug)]
pub struct Placement {
    /// The app's pod it goes beside.
    pub pod: String,
    pub node: String,
    /// The node's architecture as Kubernetes names it: `amd64` or `arm64`.
    pub architecture: String,
    /// The app container's mounts and the pod volumes they use, as the pod has them.
    mounts: Vec<Json>,
    volumes: Vec<Json>,
    pod_security: Json,
    container_security: Json,
}

fn other(message: String) -> RunnerError {
    RunnerError(message)
}

fn ready(pod: &Json) -> bool {
    pod["status"]["phase"] == "Running"
        && pod["status"]["conditions"]
            .as_array()
            .is_some_and(|conditions| {
                conditions
                    .iter()
                    .any(|c| c["type"] == "Ready" && c["status"] == "True")
            })
}

impl Connection {
    /// The objects of `kind` in `namespace` whose labels match `selector` (`key=value,...`).
    pub(crate) fn list(
        &self,
        api_version: &str,
        kind: &str,
        namespace: &str,
        selector: &str,
    ) -> Result<Vec<Json>, ClusterError> {
        let id = ResourceId {
            api_version: api_version.to_owned(),
            kind: kind.to_owned(),
            namespace: Some(namespace.to_owned()),
            name: String::new(),
        };
        self.list_of(&id, selector)
    }
}

/// Finds a ready pod of `workload` in `namespace`, its node, and the app container's volumes. Nothing is created.
pub fn place_runner(
    connection: &Connection,
    namespace: &str,
    workload: &Workload,
) -> Result<Placement, RunnerError> {
    let named = format!("{} {}", workload.kind.as_str(), workload.name);
    let id = ResourceId {
        api_version: "apps/v1".to_owned(),
        kind: workload.kind.as_str().to_owned(),
        namespace: Some(namespace.to_owned()),
        name: workload.name.clone(),
    };
    let found = match connection.get(&id) {
        Ok(found) => found,
        Err(ClusterError::NotFound) => {
            return Err(other(format!(
                "{named} is not in namespace {namespace}; the app's plugin names it as the workload that serves the \
                 install"
            )));
        }
        Err(error) => return Err(error.into()),
    };
    let labels = found["spec"]["selector"]["matchLabels"]
        .as_object()
        .filter(|labels| !labels.is_empty())
        .ok_or_else(|| {
            other(format!(
                "{named} has no selector labels to find its pods by"
            ))
        })?;
    let selector = labels
        .iter()
        .map(|(key, value)| format!("{key}={}", value.as_str().unwrap_or_default()))
        .collect::<Vec<_>>()
        .join(",");
    let pods = connection.list("v1", "Pod", namespace, &selector)?;
    let pod = pods.iter().find(|pod| ready(pod)).ok_or_else(|| {
        other(format!(
            "{named} has no ready pod in namespace {namespace}; the runner works beside one, so start the app first"
        ))
    })?;
    let pod_name = pod["metadata"]["name"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let node = pod["spec"]["nodeName"]
        .as_str()
        .ok_or_else(|| other(format!("pod {pod_name} is on no node yet")))?
        .to_owned();
    let containers = pod["spec"]["containers"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let names: Vec<&str> = containers
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    let container = match &workload.container {
        Some(wanted) => containers
            .iter()
            .find(|c| c["name"].as_str() == Some(wanted))
            .ok_or_else(|| {
                other(format!(
                    "pod {pod_name} has no container {wanted} (it has {})",
                    names.join(", ")
                ))
            })?,
        None => match containers.as_slice() {
            [only] => only,
            _ => {
                return Err(other(format!(
                    "pod {pod_name} has several containers ({}); the app's plugin must name the app's",
                    names.join(", ")
                )));
            }
        },
    };
    let mounts: Vec<Json> = container["volumeMounts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|mount| mount["mountPath"].as_str() != Some(SERVICE_ACCOUNT_DIR))
        .filter(|mount| mount["mountPath"].as_str() != Some(RUNNER_DIR))
        .collect();
    let used: Vec<&str> = mounts.iter().filter_map(|m| m["name"].as_str()).collect();
    let volumes: Vec<Json> = pod["spec"]["volumes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|volume| {
            volume["name"]
                .as_str()
                .is_some_and(|name| used.contains(&name))
        })
        .collect();
    let node_id = ResourceId {
        api_version: "v1".to_owned(),
        kind: "Node".to_owned(),
        namespace: None,
        name: node.clone(),
    };
    let architecture = connection.get(&node_id)?["status"]["nodeInfo"]["architecture"]
        .as_str()
        .ok_or_else(|| other(format!("node {node} does not say its architecture")))?
        .to_owned();
    Ok(Placement {
        pod: pod_name,
        node,
        architecture,
        mounts,
        volumes,
        pod_security: pod["spec"]["securityContext"].clone(),
        container_security: container["securityContext"].clone(),
    })
}

/// The runner's Job, named `name`: on the placement's node with its volumes, its one container sleeping for the
/// deadline, with no environment and no service account token.
pub fn runner_job(namespace: &str, placement: &Placement, spec: &RunnerSpec, name: &str) -> Json {
    let seconds = spec.deadline.as_secs().max(1);
    let labels = json!({"speedyspec.runner": "1", "speedyspec.install": spec.install});
    let mut mounts = placement.mounts.clone();
    mounts.push(json!({"name": "speedyspec-runner", "mountPath": RUNNER_DIR}));
    let mut volumes = placement.volumes.clone();
    volumes.push(json!({"name": "speedyspec-runner", "emptyDir": {"medium": "Memory"}}));
    let mut container = json!({
        "name": CONTAINER,
        "image": spec.image,
        "command": ["sleep", seconds.to_string()],
        "volumeMounts": mounts,
    });
    if !placement.container_security.is_null() {
        container["securityContext"] = placement.container_security.clone();
    }
    let mut pod = json!({
        "nodeName": placement.node,
        "restartPolicy": "Never",
        "automountServiceAccountToken": false,
        "enableServiceLinks": false,
        "terminationGracePeriodSeconds": 0,
        "containers": [container],
        "volumes": volumes,
    });
    if !placement.pod_security.is_null() {
        pod["securityContext"] = placement.pod_security.clone();
    }
    json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {"name": name, "namespace": namespace, "labels": labels},
        "spec": {
            "activeDeadlineSeconds": seconds,
            "ttlSecondsAfterFinished": 60,
            "backoffLimit": 0,
            "template": {"metadata": {"labels": labels}, "spec": pod},
        },
    })
}

/// A random suffix for the Job's name, so two commands never share one.
fn suffix() -> String {
    let mut bytes = [0u8; 3];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_err()
    {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
            ^ std::process::id();
        bytes = [(nanos >> 16) as u8, (nanos >> 8) as u8, nanos as u8];
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The signal a stopping signal handler saw; 0 for none.
static STOPPED_BY: AtomicI32 = AtomicI32::new(0);
const STOPPING: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

extern "C" fn note_signal(signal: libc::c_int) {
    STOPPED_BY.store(signal, Ordering::SeqCst);
}

/// The stopping signal received while a runner pod is in use, if one was. A command streaming through exec, or
/// waiting for the pod, gives up when it is set.
pub(crate) fn stopping_signal() -> Option<libc::c_int> {
    match STOPPED_BY.load(Ordering::SeqCst) {
        0 => None,
        signal => Some(signal),
    }
}

/// While it lives, Ctrl-C and the other stopping signals are caught: the command in the pod gives up, the
/// `RunnerPod` deletes its Job as it goes, and then this guard ends the CLI as the signal would have.
struct SignalGuard {
    previous: Vec<(libc::c_int, libc::sighandler_t)>,
}

impl SignalGuard {
    fn install() -> SignalGuard {
        STOPPED_BY.store(0, Ordering::SeqCst);
        let handler = note_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        let mut previous = Vec::new();
        for signal in STOPPING {
            // SAFETY: the handler only stores into an atomic, which is async-signal-safe.
            let before = unsafe { libc::signal(signal, handler) };
            if before == libc::SIG_IGN {
                // SAFETY: puts back the disposition just replaced: an ignored signal stays ignored.
                unsafe { libc::signal(signal, libc::SIG_IGN) };
            } else {
                previous.push((signal, before));
            }
        }
        SignalGuard { previous }
    }
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        for &(signal, previous) in &self.previous {
            // SAFETY: puts back the disposition `install` replaced.
            unsafe { libc::signal(signal, previous) };
        }
        if let Some(signal) = stopping_signal() {
            // The Job is gone by now (the RunnerPod deletes it before this guard drops): end as the signal would.
            // SAFETY: raises the signal with its earlier disposition back in place.
            unsafe { libc::raise(signal) };
            std::process::exit(128 + signal);
        }
    }
}

fn foreground() -> DeleteParams {
    DeleteParams {
        propagation_policy: Some(PropagationPolicy::Foreground),
        grace_period_seconds: Some(0),
        ..DeleteParams::default()
    }
}

/// A started runner pod. Dropping it deletes its Job.
pub struct RunnerPod<'c> {
    connection: &'c Connection,
    namespace: String,
    job: String,
    pod: String,
    _signals: SignalGuard,
}

impl<'c> RunnerPod<'c> {
    /// Creates the Job, waits for its pod, streams `binary` in and checks it against `sha256` there. Anything that
    /// fails deletes the Job again, and says which step failed.
    pub fn start(
        connection: &'c Connection,
        namespace: &str,
        placement: &Placement,
        binary: &Path,
        sha256: &str,
        spec: &RunnerSpec,
    ) -> Result<RunnerPod<'c>, RunnerError> {
        let job = format!("speedyspec-run-{}-{}", spec.install, suffix());
        let manifest = runner_job(namespace, placement, spec, &job);
        // Catching Ctrl-C before the Job exists, so there is never a moment it could be left.
        let signals = SignalGuard::install();
        let mut runner = RunnerPod {
            connection,
            namespace: namespace.to_owned(),
            job: job.clone(),
            pod: String::new(),
            _signals: signals,
        };
        connection.apply(&manifest, true).map_err(|e| {
            other(format!(
                "could not create the runner pod's Job {job}: {}",
                e.message(&job)
            ))
        })?;
        runner.pod = runner.wait_for_pod(spec)?;
        runner.command(
            &["chmod", "0700", RUNNER_DIR],
            None,
            &mut std::io::sink(),
            &mut std::io::sink(),
        )?;
        let mut file = std::fs::File::open(binary).map_err(|e| {
            other(format!(
                "cannot read the runner binary {}: {e}",
                binary.display()
            ))
        })?;
        runner.put(BINARY, &mut file, 0o700).map_err(|e| {
            other(format!(
                "could not copy the runner binary into the pod: {}",
                e.0
            ))
        })?;
        let mut sums = Vec::new();
        let code = runner.command(
            &["sha256sum", BINARY],
            None,
            &mut sums,
            &mut std::io::sink(),
        )?;
        let found = String::from_utf8_lossy(&sums);
        let found = found.split_whitespace().next().unwrap_or_default();
        if code != 0 || !found.eq_ignore_ascii_case(sha256) {
            return Err(other(format!(
                "the runner binary in the pod does not match its checksum (expected {sha256}, the pod has {})",
                if found.is_empty() { "nothing" } else { found }
            )));
        }
        Ok(runner)
    }

    /// The Job's pod once it runs. An image that cannot be pulled, or a pod that does not start in time, is an
    /// error naming the step.
    fn wait_for_pod(&self, spec: &RunnerSpec) -> Result<String, RunnerError> {
        let started = Instant::now();
        let mut last = "no pod yet".to_owned();
        loop {
            let pods = self.connection.list(
                "v1",
                "Pod",
                &self.namespace,
                &format!("job-name={}", self.job),
            )?;
            if let Some(pod) = pods.first() {
                let name = pod["metadata"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                let phase = pod["status"]["phase"].as_str().unwrap_or("Pending");
                let waiting = pod["status"]["containerStatuses"]
                    .as_array()
                    .and_then(|statuses| statuses.first())
                    .map(|status| status["state"]["waiting"].clone())
                    .unwrap_or(Json::Null);
                let reason = waiting["reason"].as_str().unwrap_or_default();
                if matches!(
                    reason,
                    "ErrImagePull" | "ImagePullBackOff" | "InvalidImageName"
                ) {
                    return Err(other(format!(
                        "the runner image {} could not be pulled: {}",
                        spec.image,
                        waiting["message"].as_str().unwrap_or(reason)
                    )));
                }
                match phase {
                    "Running" => return Ok(name),
                    "Failed" | "Succeeded" => {
                        return Err(other(format!(
                            "the runner pod {name} ended before it was used ({phase})"
                        )));
                    }
                    _ => {
                        last = if reason.is_empty() {
                            phase.to_owned()
                        } else {
                            format!("{phase}, {reason}")
                        }
                    }
                }
            }
            if started.elapsed() >= spec.start_wait {
                return Err(other(format!(
                    "the runner pod did not start within {}s ({last})",
                    spec.start_wait.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(250).min(spec.start_wait));
        }
    }

    /// The runner pod's name.
    pub fn pod(&self) -> &str {
        &self.pod
    }

    /// Runs `command` in the runner container.
    pub fn command(
        &self,
        command: &[&str],
        stdin: Option<&mut (dyn Read + Send)>,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
    ) -> Result<i32, RunnerError> {
        Ok(self.connection.exec(
            &self.namespace,
            &self.pod,
            Some(CONTAINER),
            command,
            stdin,
            stdout,
            stderr,
        )?)
    }

    /// Runs the CLI's binary in the pod with `args`.
    pub fn run(
        &self,
        args: &[&str],
        stdin: Option<&mut (dyn Read + Send)>,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
    ) -> Result<i32, RunnerError> {
        let mut command = vec![BINARY];
        command.extend_from_slice(args);
        self.command(&command, stdin, stdout, stderr)
    }

    /// Writes `input` to `path` in the pod with `mode`, made private (umask 077) from the start.
    pub fn put(
        &self,
        path: &str,
        input: &mut (dyn Read + Send),
        mode: u32,
    ) -> Result<(), RunnerError> {
        let mode = format!("{mode:o}");
        let mut err = Vec::new();
        let code = self.command(
            &[
                "sh",
                "-c",
                "umask 077 && cat > \"$1\" && chmod \"$2\" \"$1\"",
                "sh",
                path,
                &mode,
            ],
            Some(input),
            &mut std::io::sink(),
            &mut err,
        )?;
        if code != 0 {
            return Err(other(format!(
                "could not write {path} in the runner pod: {}",
                String::from_utf8_lossy(&err).trim()
            )));
        }
        Ok(())
    }

    /// Streams `path` in the pod to `output`.
    pub fn get(&self, path: &str, output: &mut dyn Write) -> Result<(), RunnerError> {
        let mut err = Vec::new();
        let code = self.command(&["cat", path], None, output, &mut err)?;
        if code != 0 {
            return Err(other(format!(
                "could not read {path} in the runner pod: {}",
                String::from_utf8_lossy(&err).trim()
            )));
        }
        Ok(())
    }

    /// Deletes `path` in the pod; one that is not there is no error.
    pub fn remove(&self, path: &str) -> Result<(), RunnerError> {
        self.command(
            &["rm", "-rf", path],
            None,
            &mut std::io::sink(),
            &mut std::io::sink(),
        )
        .map(drop)
    }
}

impl Drop for RunnerPod<'_> {
    fn drop(&mut self) {
        let jobs: Api<Job> = Api::namespaced(self.connection.client.clone(), &self.namespace);
        let deleted = self.connection.runtime.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(30),
                jobs.delete(&self.job, &foreground()),
            )
            .await
        });
        let failed = match deleted {
            Ok(Ok(_)) => None,
            Ok(Err(kube::Error::Api(status))) if status.code == 404 => None,
            Ok(Err(error)) => Some(error.to_string()),
            Err(_) => Some("no answer within 30s".to_owned()),
        };
        if let Some(problem) = failed {
            eprintln!(
                "speedyspec: warning: could not delete the runner pod's Job {} in namespace {} ({problem}); it \
                 ends by itself at its deadline",
                self.job, self.namespace
            );
        }
    }
}
