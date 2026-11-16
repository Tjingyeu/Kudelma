use kudelma_config::HooksConfig;
use kudelma_providers::CancelToken;
use serde_json::{Value, json};

const HOOK_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone)]
pub struct HookOutcome {
    pub allowed: bool,
    pub message: Option<String>,
}

pub async fn run_pre_tool(
    hooks: &HooksConfig,
    tool: &str,
    input: &Value,
    cwd: &std::path::Path,
    cancel: &CancelToken,
) -> HookOutcome {
    let Some(script) = &hooks.pre_tool_use else {
        return HookOutcome {
            allowed: true,
            message: None,
        };
    };
    match run_hook(
        script,
        "pre_tool_use",
        json!({ "tool": tool, "input": input }),
        cwd,
        cancel,
    )
    .await
    {
        Ok((code, _stdout, stderr)) => {
            if code == 2 {
                HookOutcome {
                    allowed: false,
                    message: Some(if stderr.trim().is_empty() {
                        "blocked by the pre_tool_use hook".into()
                    } else {
                        stderr.trim().to_string()
                    }),
                }
            } else {
                HookOutcome {
                    allowed: true,
                    message: (!stderr.trim().is_empty()).then(|| stderr.trim().to_string()),
                }
            }
        }
        Err(error) => HookOutcome {
            allowed: true,
            message: Some(format!("pre_tool_use hook failed: {error}")),
        },
    }
}

pub async fn run_post_tool(
    hooks: &HooksConfig,
    tool: &str,
    input: &Value,
    output: &str,
    cwd: &std::path::Path,
    cancel: &CancelToken,
) {
    let Some(script) = &hooks.post_tool_use else {
        return;
    };
    let _ = run_hook(
        script,
        "post_tool_use",
        json!({ "tool": tool, "input": input, "output": output }),
        cwd,
        cancel,
    )
    .await;
}

pub async fn run_session_end(hooks: &HooksConfig, cwd: &std::path::Path) {
    let Some(script) = &hooks.session_end else {
        return;
    };
    let _ = run_hook(script, "session_end", json!({}), cwd, &CancelToken::new()).await;
}

async fn run_hook(
    script: &str,
    kind: &str,
    payload: Value,
    cwd: &std::path::Path,
    cancel: &CancelToken,
) -> Result<(i32, String, String), String> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;

    let mut child = tokio::process::Command::new("bash")
        .arg("-lc")
        .arg(script)
        .current_dir(cwd)
        .env("KUDELMA_HOOK", kind)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| error.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.to_string().as_bytes()).await;
    }
    let wait = tokio::time::timeout(
        std::time::Duration::from_secs(HOOK_TIMEOUT_SECS),
        child.wait_with_output(),
    );
    tokio::select! {
        result = wait => {
            let output = result
                .map_err(|_| "hook timed out".to_string())?
                .map_err(|error| error.to_string())?;
            Ok((
                output.status.code().unwrap_or(1),
                String::from_utf8_lossy(&output.stdout).to_string(),
                String::from_utf8_lossy(&output.stderr).to_string(),
            ))
        }
        _ = cancel.cancelled() => Err("hook cancelled".into()),
    }
}
