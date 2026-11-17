use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use kudelma_permissions::analyze_bash;
use kudelma_storage::atomic_write;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Child;
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    BackgroundNotice, Tool, ToolContext, ToolError, ToolOutput, ToolPermission, display_path,
    object_schema, optional_bool, optional_str, optional_usize, required_str, truncate_output,
};

const MAX_CAPTURE_BYTES: u64 = 5 * 1024 * 1024;
const BACKGROUND_PREVIEW_BYTES: usize = 20_000;

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &'static str {
        "Run a shell command in the workspace. Output keeps both the beginning and end of long results. Set background=true for long builds or test runs; a receipt is returned immediately and the result arrives later."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "command": { "type": "string", "description": "Shell command to run" },
                "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 3600, "description": "Default 120" },
                "cwd": { "type": "string", "description": "Working directory, defaults to the workspace root" },
                "background": { "type": "boolean", "description": "Detach long-running work and return a receipt" }
            }),
            &["command"],
        )
    }

    fn permission(&self, args: &Value, ctx: &ToolContext) -> Result<ToolPermission, ToolError> {
        let command = required_str(args, "command")?;
        let analysis = analyze_bash(&command);
        let reason = analysis
            .force_prompt
            .then(|| "command contains redirections or shell metacharacters".to_string());
        let cwd = optional_str(args, "cwd")
            .map(|cwd| ctx.env.resolve(&cwd))
            .unwrap_or_else(|| ctx.cwd().to_path_buf());
        Ok(ToolPermission {
            scopes: analysis.scopes,
            force_prompt: analysis.force_prompt,
            reason,
            cwd: Some(cwd),
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let command = required_str(&args, "command")?;
        let cwd = match optional_str(&args, "cwd") {
            Some(cwd) => ctx.env.resolve(&cwd),
            None => ctx.cwd().to_path_buf(),
        };
        if !cwd.is_dir() {
            return Err(ToolError::InvalidArgs(format!(
                "working directory does not exist: {}",
                cwd.display()
            )));
        }
        let timeout_secs = optional_usize(&args, "timeout_secs")
            .map(|value| value as u64)
            .unwrap_or(ctx.config.agent.bash_timeout_secs)
            .clamp(1, 3600);
        let background = optional_bool(&args, "background").unwrap_or(false);

        let label = format!("bash: {}", command.chars().take(60).collect::<String>());
        let child = spawn(&command, &cwd, ctx)?;
        if background {
            let Some(sender) = ctx.background.clone() else {
                return Err(ToolError::Other("background tasks are unavailable".into()));
            };
            spawn_background(child, label.clone(), timeout_secs, sender);
            return Ok(ToolOutput::success(format!(
                "Started in the background: {label}\nA completion notice will arrive after the turn."
            ))
            .with_metadata(json!({ "background": true })));
        }

        let pid = child.id();
        let mut child = child;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ToolError::Other("cannot capture stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ToolError::Other("cannot capture stderr".into()))?;
        let stdout_task = tokio::spawn(read_capped(stdout));
        let stderr_task = tokio::spawn(read_capped(stderr));

        let sleep = tokio::time::sleep(Duration::from_secs(timeout_secs));
        tokio::pin!(sleep);
        enum Outcome {
            Done(std::io::Result<std::process::ExitStatus>),
            Timeout,
            Cancelled,
            Background,
        }
        let outcome = tokio::select! {
            status = child.wait() => Outcome::Done(status),
            _ = &mut sleep => Outcome::Timeout,
            _ = ctx.cancel.cancelled() => {
                if ctx.cancel.take_background() {
                    Outcome::Background
                } else {
                    Outcome::Cancelled
                }
            }
        };

        if matches!(outcome, Outcome::Background) {
            let Some(sender) = ctx.background.clone() else {
                kill_tree(&mut child, pid);
                let _ = child.wait().await;
                return Err(ToolError::Other("background tasks are unavailable".into()));
            };
            let output_task = stdout_task;
            let error_task = stderr_task;
            tokio::spawn(async move {
                let status = child.wait().await;
                let mut output = collect(
                    output_task.await.unwrap_or_default().0,
                    error_task.await.unwrap_or_default().0,
                );
                append_status(&mut output, status.as_ref().ok(), false, false);
                let (preview, _) = truncate_output(&output, BACKGROUND_PREVIEW_BYTES, 200);
                let _ = sender.send(BackgroundNotice {
                    id: uuid::Uuid::new_v4().to_string(),
                    label,
                    content: preview,
                    is_error: !status.map(|s| s.success()).unwrap_or(false),
                });
            });
            return Ok(ToolOutput::success(format!(
                "Moved to the background after {}s. The result will arrive as a notice.",
                timeout_secs
            ))
            .with_metadata(json!({ "background": true })));
        }

        let timed_out = matches!(outcome, Outcome::Timeout);
        let cancelled = matches!(outcome, Outcome::Cancelled);
        if timed_out || cancelled {
            kill_tree(&mut child, pid);
        }
        let status = match outcome {
            Outcome::Done(status) => status.ok(),
            _ => {
                let _ = child.wait().await;
                None
            }
        };
        let stdout_bytes = stdout_task.await.unwrap_or_default().0;
        let stderr_bytes = stderr_task.await.unwrap_or_default().0;
        let mut output = collect(stdout_bytes, stderr_bytes);
        append_status(&mut output, status.as_ref(), timed_out, cancelled);

        let max_bytes = ctx.config.agent.max_output_bytes;
        let max_lines = ctx.config.agent.max_output_lines;
        let (truncated, was_truncated) = truncate_output(&output, max_bytes, max_lines);
        let mut text = truncated;
        if was_truncated {
            if let Ok(path) = save_log(ctx, &output) {
                text.push_str(&format!(
                    "\n[full output: {}]",
                    display_path(&ctx.state_dir, &path)
                ));
            }
        }
        let is_error =
            timed_out || cancelled || !status.map(|status| status.success()).unwrap_or(false);
        Ok(ToolOutput::success(text).with_metadata(json!({
            "exit_code": status.and_then(|status| status.code()),
            "timed_out": timed_out,
            "cancelled": cancelled,
        })))
        .map(|output| ToolOutput {
            is_error: is_error && !cancelled,
            ..output
        })
    }
}

fn spawn(command: &str, cwd: &std::path::Path, ctx: &ToolContext) -> Result<Child, ToolError> {
    ctx.env
        .shell(command, cwd)
        .map_err(|error| ToolError::Io(format!("cannot start shell: {error}")))?
        .spawn()
        .map_err(|error| ToolError::Io(format!("cannot start shell: {error}")))
}

async fn read_capped<R: AsyncRead + Unpin>(reader: R) -> (Vec<u8>, bool) {
    let mut buffer = Vec::new();
    let mut limited = reader.take(MAX_CAPTURE_BYTES);
    let _ = limited.read_to_end(&mut buffer).await;
    let truncated = buffer.len() as u64 >= MAX_CAPTURE_BYTES;
    (buffer, truncated)
}

fn collect(stdout: Vec<u8>, stderr: Vec<u8>) -> String {
    let mut output = String::new();
    if !stdout.is_empty() {
        output.push_str(&String::from_utf8_lossy(&stdout));
    }
    if !stderr.is_empty() {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str("[stderr]\n");
        output.push_str(&String::from_utf8_lossy(&stderr));
    }
    if output.trim().is_empty() {
        output.push_str("(no output)");
    }
    output
}

fn append_status(
    output: &mut String,
    status: Option<&std::process::ExitStatus>,
    timed_out: bool,
    cancelled: bool,
) {
    if !output.ends_with('\n') {
        output.push('\n');
    }
    if cancelled {
        output.push_str("[interrupted by user — partial output]");
    } else if timed_out {
        output.push_str("[command timed out; partial output]");
    } else if let Some(status) = status {
        if !status.success() {
            match status.code() {
                Some(code) => output.push_str(&format!("[exit code: {code}]")),
                None => output.push_str("[terminated by signal]"),
            }
        }
    }
}

fn kill_tree(child: &mut Child, pid: Option<u32>) {
    #[cfg(unix)]
    {
        let _ = child;
        if let Some(pid) = pid {
            // SAFETY: a negative pid signals the process group created by the
            // workspace shell. The child was spawned with process_group(0).
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        let _ = child.start_kill();
    }
}

fn spawn_background(
    child: Child,
    label: String,
    timeout_secs: u64,
    sender: UnboundedSender<BackgroundNotice>,
) {
    tokio::spawn(async move {
        let mut child = child;
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdout_task = stdout.map(|stream| tokio::spawn(read_capped(stream)));
        let stderr_task = stderr.map(|stream| tokio::spawn(read_capped(stream)));
        let status =
            match tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait()).await {
                Ok(result) => result.ok(),
                Err(_) => {
                    kill_tree(&mut child, pid);
                    let _ = child.wait().await;
                    None
                }
            };
        let stdout_bytes = match stdout_task {
            Some(task) => task.await.unwrap_or_default().0,
            None => Vec::new(),
        };
        let stderr_bytes = match stderr_task {
            Some(task) => task.await.unwrap_or_default().0,
            None => Vec::new(),
        };
        let mut output = collect(stdout_bytes, stderr_bytes);
        let timed_out = status.is_none();
        append_status(&mut output, status.as_ref(), timed_out, false);
        let (preview, _) = truncate_output(&output, BACKGROUND_PREVIEW_BYTES, 200);
        let _ = sender.send(BackgroundNotice {
            id: uuid::Uuid::new_v4().to_string(),
            label,
            content: preview,
            is_error: timed_out || !status.map(|s| s.success()).unwrap_or(false),
        });
    });
}

fn save_log(ctx: &ToolContext, output: &str) -> Result<PathBuf, std::io::Error> {
    let dir = ctx
        .state_dir
        .join("sessions")
        .join("data")
        .join(&ctx.session_id)
        .join("bash");
    let path = dir.join(format!("{}.log", uuid::Uuid::new_v4()));
    atomic_write(&path, output.as_bytes()).map_err(std::io::Error::other)?;
    Ok(path)
}
