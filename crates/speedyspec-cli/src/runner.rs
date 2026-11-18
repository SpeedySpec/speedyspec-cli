//! Running an app's own PHP: the parent side. The task runs in a child process of this binary
//! (`self run-task`, see `runner_child.rs`), so the app's state, its fatal errors and its
//! `exit()` never reach the CLI's own PHP. The request and the result travel in files of a
//! private temporary directory, never in arguments or the environment, which the child inherits
//! unchanged (an app's config may read it).

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value as Json, json};
use speedyspec_driver::{AppRuntime, AppTask};

/// The most of a task's output that is kept, per stream.
const MAX_OUTPUT: usize = 1024 * 1024;
/// How much of the output an error message quotes.
const QUOTED_OUTPUT: usize = 2000;
/// How long to wait for the output once the task's processes are gone.
const OUTPUT_GRACE: Duration = Duration::from_secs(2);
/// The signals that stop the CLI, which stop a running task first.
const STOPPING: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

/// A stopping signal that arrived while a task ran, or 0.
static STOPPED_BY: AtomicI32 = AtomicI32::new(0);

extern "C" fn note_signal(signal: libc::c_int) {
    STOPPED_BY.store(signal, Ordering::SeqCst);
}

/// The task runs in a process group of its own, so that everything it starts can be stopped
/// with it. That also keeps the terminal's Ctrl-C from reaching it, so while the guard lives the
/// stopping signals are caught here; the caller stops the group and then ends as the signal
/// would have ended it.
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
                // Ignored before (SIGHUP under nohup, say): it stays ignored.
                // SAFETY: puts back the disposition just replaced.
                unsafe { libc::signal(signal, libc::SIG_IGN) };
            } else {
                previous.push((signal, before));
            }
        }
        SignalGuard { previous }
    }

    fn received(&self) -> Option<libc::c_int> {
        match STOPPED_BY.load(Ordering::SeqCst) {
            0 => None,
            signal => Some(signal),
        }
    }
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        for &(signal, previous) in &self.previous {
            // SAFETY: puts back the disposition `install` replaced.
            unsafe { libc::signal(signal, previous) };
        }
    }
}

/// Stops every process left in the task's group: the child, if it still runs, and anything the
/// app started.
fn stop_group(leader: u32) {
    // SAFETY: a negative pid names the process group the child leads; nothing else is in it.
    unsafe { libc::kill(-(leader as libc::pid_t), libc::SIGKILL) };
}

/// The output a reader thread collected, or what it had when `deadline` passed: a process that
/// left the group can still hold the pipe open.
fn finish(reader: JoinHandle<(Vec<u8>, bool)>, deadline: Instant) -> (Vec<u8>, bool) {
    while !reader.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if reader.is_finished() {
        reader.join().unwrap_or_default()
    } else {
        (
            b"[output still held open by a process the task started]\n".to_vec(),
            true,
        )
    }
}

/// The diagnostics the child appended to `path` as they happened, one JSON object a line.
fn diagnostics_so_far(path: &Path) -> Vec<Diagnostic> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Json>(line).ok())
        .map(|json| Diagnostic::from_json(&json))
        .collect()
}

/// A PHP warning, notice, deprecation, error or exception raised while the task ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub level: String,
    pub message: String,
    pub file: String,
    pub line: u64,
}

impl Diagnostic {
    fn from_json(json: &Json) -> Diagnostic {
        Diagnostic {
            level: json["level"].as_str().unwrap_or("warning").to_owned(),
            message: json["message"].as_str().unwrap_or_default().to_owned(),
            file: json["file"].as_str().unwrap_or_default().to_owned(),
            line: json["line"].as_u64().unwrap_or(0),
        }
    }

    fn to_json(&self) -> Json {
        json!({"level": self.level, "message": self.message, "file": self.file, "line": self.line})
    }
}

/// What a task gave back.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskOutcome {
    pub result: Json,
    pub diagnostics: Vec<Diagnostic>,
    /// What the app and the task printed, stdout then stderr.
    pub output: String,
}

impl TaskOutcome {
    pub fn to_json(&self) -> Json {
        json!({
            "result": self.result,
            "diagnostics": self.diagnostics.iter().map(Diagnostic::to_json).collect::<Vec<_>>(),
            "output": self.output,
        })
    }
}

/// Why a task gave nothing back, with what it printed and raised, as one message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunnerError(pub String);

impl std::fmt::Display for RunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RunnerError {}

/// Reads a child's stream, keeping at most `MAX_OUTPUT` bytes and draining the rest so the
/// child never blocks on a full pipe.
fn collect(stream: impl Read + Send + 'static) -> JoinHandle<(Vec<u8>, bool)> {
    std::thread::spawn(move || {
        let mut stream = stream;
        let mut kept = Vec::new();
        let mut truncated = false;
        let mut buffer = [0u8; 8192];
        while let Ok(n) = stream.read(&mut buffer) {
            if n == 0 {
                break;
            }
            let room = MAX_OUTPUT.saturating_sub(kept.len());
            kept.extend_from_slice(&buffer[..n.min(room)]);
            truncated |= n > room;
        }
        (kept, truncated)
    })
}

fn output_text(stdout: (Vec<u8>, bool), stderr: (Vec<u8>, bool)) -> String {
    let mut text = String::from_utf8_lossy(&stdout.0).into_owned();
    if stdout.1 {
        text.push_str("\n[stdout truncated]\n");
    }
    text.push_str(&String::from_utf8_lossy(&stderr.0));
    if stderr.1 {
        text.push_str("\n[stderr truncated]\n");
    }
    text
}

/// The diagnostics and output, as lines to put under an error.
fn details(diagnostics: &[Diagnostic], output: &str) -> String {
    let mut text = String::new();
    for d in diagnostics {
        text.push_str(&format!(
            "\n  {}: {} ({}:{})",
            d.level, d.message, d.file, d.line
        ));
    }
    let output = output.trim();
    if !output.is_empty() {
        let shown: String = output.chars().take(QUOTED_OUTPUT).collect();
        text.push_str(&format!("\n  it printed: {shown}"));
        if shown.len() < output.len() {
            text.push('…');
        }
    }
    text
}

/// Runs `task` in the app at `root`, loaded as `runtime` says, and gives back its result.
pub fn run(root: &Path, runtime: &AppRuntime, task: &AppTask) -> Result<TaskOutcome, RunnerError> {
    let name = task.file.display().to_string();
    let fail = |message: String| RunnerError(message);
    let dir = tempfile::Builder::new()
        .prefix("speedyspec-task-")
        .tempdir()
        .map_err(|e| fail(format!("could not make a directory for the task: {e}")))?;
    let request_path = dir.path().join("request.json");
    let result_path = dir.path().join("result.json");
    let diagnostics_path = dir.path().join("diagnostics.jsonl");
    let map = |pairs: &[(String, String)]| -> Json {
        Json::Object(
            pairs
                .iter()
                .map(|(k, v)| (k.clone(), Json::String(v.clone())))
                .collect(),
        )
    };
    let request = json!({
        "root": root.to_string_lossy(),
        "entry": runtime.entry,
        "ini": map(&runtime.ini),
        "server": map(&runtime.server),
        "file": task.file.to_string_lossy(),
        "input": task.input,
        "loads_app": task.loads_app,
        "diagnostics_file": diagnostics_path.to_string_lossy(),
    });
    std::fs::write(&request_path, request.to_string())
        .map_err(|e| fail(format!("could not write the task's request: {e}")))?;

    let exe = std::env::current_exe().map_err(|e| {
        fail(format!(
            "cannot find the speedyspec executable to run the task: {e}"
        ))
    })?;
    let signals = SignalGuard::install();
    let mut child = Command::new(exe)
        .arg("self")
        .arg("run-task")
        .arg("--request")
        .arg(&request_path)
        .arg("--result")
        .arg(&result_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| fail(format!("could not start the task {name}: {e}")))?;
    let stdout = collect(child.stdout.take().expect("stdout is piped"));
    let stderr = collect(child.stderr.take().expect("stderr is piped"));

    let started = Instant::now();
    let status = loop {
        if let Some(signal) = signals.received() {
            stop_group(child.id());
            let _ = child.wait();
            drop(dir);
            drop(signals);
            // SAFETY: ends this process as the signal would have, its own disposition restored.
            unsafe { libc::raise(signal) };
            std::process::exit(128 + signal);
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= task.timeout => {
                stop_group(child.id());
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                stop_group(child.id());
                return Err(fail(format!("could not wait for the task {name}: {e}")));
            }
        }
    };
    // Whatever the app started in the background goes with the task.
    stop_group(child.id());
    drop(signals);
    let deadline = Instant::now() + OUTPUT_GRACE;
    let output = output_text(finish(stdout, deadline), finish(stderr, deadline));
    let Some(status) = status else {
        return Err(fail(format!(
            "the task {name} did not finish within {}s and was stopped{}",
            task.timeout.as_secs(),
            details(&diagnostics_so_far(&diagnostics_path), &output)
        )));
    };

    let written = std::fs::read_to_string(&result_path).unwrap_or_default();
    let Ok(result) = serde_json::from_str::<Json>(&written) else {
        return Err(fail(format!(
            "the task {name} ended without a result ({status}){}",
            details(&diagnostics_so_far(&diagnostics_path), &output)
        )));
    };
    let diagnostics: Vec<Diagnostic> = result["diagnostics"]
        .as_array()
        .map(|all| all.iter().map(Diagnostic::from_json).collect())
        .unwrap_or_default();
    if result["ok"] == Json::Bool(true) {
        return Ok(TaskOutcome {
            result: result["result"].clone(),
            diagnostics,
            output,
        });
    }
    let error = result["error"].as_str().unwrap_or("it gave no reason");
    Err(fail(format!(
        "the task {name} failed: {error}{}",
        details(&diagnostics, &output)
    )))
}
