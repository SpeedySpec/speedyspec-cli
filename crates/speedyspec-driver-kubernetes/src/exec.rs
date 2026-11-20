//! Runs a command in a pod through the API's exec, with its input and output streamed in 64 KiB chunks, so a dump or
//! an archive of any size goes through in flat memory.

use std::io::{Read, Write};

use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, AttachParams};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::cluster::ClusterError;
use crate::connect::Connection;

const CHUNK: usize = 64 * 1024;

/// The exit status in the status the API sends when the command ends: 0 for success, the command's own code for a
/// non-zero exit; `None` when the status says neither (the command could not start).
fn exit_code(status: &k8s_openapi::apimachinery::pkg::apis::meta::v1::Status) -> Option<i32> {
    if status.status.as_deref() == Some("Success") {
        return Some(0);
    }
    if status.reason.as_deref() != Some("NonZeroExitCode") {
        return None;
    }
    status
        .details
        .as_ref()?
        .causes
        .as_ref()?
        .iter()
        .find(|cause| cause.reason.as_deref() == Some("ExitCode"))?
        .message
        .as_deref()?
        .parse()
        .ok()
}

impl Connection {
    /// Runs `command` in `container` (the pod's only one when `None`) of `pod`, streaming `stdin` to it and its
    /// output to `stdout` and `stderr`, and gives back its exit status. A stream that ends before the command does,
    /// such as when the pod goes, is an error naming the pod, never a truncated success.
    #[allow(clippy::too_many_arguments)]
    pub fn exec(
        &self,
        namespace: &str,
        pod: &str,
        container: Option<&str>,
        command: &[&str],
        stdin: Option<&mut (dyn Read + Send)>,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
    ) -> Result<i32, ClusterError> {
        self.exec_with(
            namespace, pod, container, command, stdin, stdout, stderr, true,
        )
    }

    /// As `exec`, but not given up on Ctrl-C: for the clean-up that has to happen after a stop.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn exec_to_the_end(
        &self,
        namespace: &str,
        pod: &str,
        container: Option<&str>,
        command: &[&str],
        stdin: Option<&mut (dyn Read + Send)>,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
    ) -> Result<i32, ClusterError> {
        self.exec_with(
            namespace, pod, container, command, stdin, stdout, stderr, false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn exec_with(
        &self,
        namespace: &str,
        pod: &str,
        container: Option<&str>,
        command: &[&str],
        stdin: Option<&mut (dyn Read + Send)>,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
        stoppable: bool,
    ) -> Result<i32, ClusterError> {
        let broke =
            |what: &str| ClusterError::Other(format!("the stream to {pod} broke while {what}"));
        let api: Api<Pod> = Api::namespaced(self.client.clone(), namespace);
        let mut params = AttachParams::default()
            .stdin(stdin.is_some())
            .stdout(true)
            .stderr(true)
            .max_stdin_buf_size(CHUNK)
            .max_stdout_buf_size(CHUNK)
            .max_stderr_buf_size(CHUNK);
        if let Some(container) = container {
            params = params.container(container);
        }
        let command: Vec<String> = command.iter().map(|part| (*part).to_owned()).collect();
        std::thread::scope(|scope| {
            // The caller's input is read on a thread of its own, so a slow reader never stops the output draining.
            let (chunks, mut incoming) = tokio::sync::mpsc::channel::<std::io::Result<Vec<u8>>>(4);
            let reader = stdin.map(|stdin| {
                scope.spawn(move || {
                    let mut buffer = vec![0u8; CHUNK];
                    loop {
                        let read = match stdin.read(&mut buffer) {
                            Ok(0) => return,
                            Ok(read) => Ok(buffer[..read].to_vec()),
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                            Err(e) => Err(e),
                        };
                        let failed = read.is_err();
                        if chunks.blocking_send(read).is_err() || failed {
                            return;
                        }
                    }
                })
            });
            let result = self.runtime.block_on(async {
                let mut process = api
                    .exec(pod, command, &params)
                    .await
                    .map_err(|e| ClusterError::Other(format!("could not run a command in {pod}: {e}")))?;
                let status = process.take_status().expect("taken once");
                let to_pod = process.stdin();
                let mut out = process.stdout().expect("asked for");
                let mut err = process.stderr().expect("asked for");
                // Input goes in and output comes out at the same time: a command such as cat stops reading once its
                // output is not drained.
                let input = async {
                    if let Some(mut writer) = to_pod {
                        while let Some(chunk) = incoming.recv().await {
                            let bytes = chunk.map_err(|e| {
                                ClusterError::Other(format!("could not read the input for {pod}: {e}"))
                            })?;
                            writer.write_all(&bytes).await.map_err(|_| broke("sending input"))?;
                        }
                        // End of input: the command sees end of file.
                        let _ = writer.shutdown().await;
                    }
                    Ok::<(), ClusterError>(())
                };
                let output = async {
                    let (mut out_buffer, mut err_buffer) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
                    let (mut out_open, mut err_open) = (true, true);
                    while out_open || err_open {
                        tokio::select! {
                            read = out.read(&mut out_buffer), if out_open => match read {
                                Ok(0) => out_open = false,
                                Ok(read) => stdout.write_all(&out_buffer[..read]).map_err(|e| {
                                    ClusterError::Other(format!("could not write the output of {pod}: {e}"))
                                })?,
                                Err(_) => return Err(broke("receiving output")),
                            },
                            read = err.read(&mut err_buffer), if err_open => match read {
                                Ok(0) => err_open = false,
                                Ok(read) => {
                                    let _ = stderr.write_all(&err_buffer[..read]);
                                }
                                Err(_) => return Err(broke("receiving output")),
                            },
                        }
                    }
                    Ok::<(), ClusterError>(())
                };
                // A stopping signal (Ctrl-C) ends the stream, so the runner pod can be deleted before the CLI goes.
                let stopped = async {
                    while !stoppable || crate::runner::stopping_signal().is_none() {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                };
                tokio::pin!(input, output, stopped);
                let mut input_done = false;
                loop {
                    tokio::select! {
                        () = &mut stopped => {
                            return Err(ClusterError::Other(format!("stopped while running a command in {pod}")));
                        }
                        sent = &mut input, if !input_done => {
                            sent?;
                            input_done = true;
                        }
                        // Once the output ends the command has, whether or not it read all its input.
                        received = &mut output => {
                            received?;
                            break;
                        }
                    }
                }
                let code = match status.await {
                    Some(status) => exit_code(&status).ok_or_else(|| {
                        ClusterError::Other(format!(
                            "the command in {pod} did not run: {}",
                            status.message.unwrap_or_default()
                        ))
                    })?,
                    None => return Err(broke("running the command")),
                };
                // A command killed by a signal may have been killed because its pod is going: then the run did not
                // end, it was cut off.
                if code >= 128 {
                    let going = match api.get_opt(pod).await {
                        Ok(Some(found)) => found.metadata.deletion_timestamp.is_some(),
                        Ok(None) => true,
                        Err(_) => false,
                    };
                    if going {
                        return Err(ClusterError::Other(format!(
                            "the stream to {pod} broke while running the command: the pod was deleted"
                        )));
                    }
                }
                Ok(code)
            });
            // The reader stops once its channel is gone.
            drop(incoming);
            if let Some(reader) = reader {
                let _ = reader.join();
            }
            result
        })
    }
}
