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
            if code == 0 {
                HookOutcome {
                    allowed: true,
                    message: (!stderr.trim().is_empty()).then(|| stderr.trim().to_string()),
                }
            } else {
                HookOutcome {
                    allowed: false,
                    message: Some(if stderr.trim().is_empty() {
                        format!("pre_tool_use hook exited with code {code}")
                    } else {
                        stderr.trim().to_string()
                    }),
                }
            }
        }
        Err(error) => HookOutcome {
            allowed: false,
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
) -> Option<String> {
    let script = hooks.post_tool_use.as_ref()?;
    match run_hook(
        script,
        "post_tool_use",
        json!({ "tool": tool, "input": input, "output": output }),
        cwd,
        cancel,
    )
    .await
    {
        Ok((0, _, stderr)) => {
            let stderr = stderr.trim();
            (!stderr.is_empty()).then(|| stderr.to_string())
        }
        Ok((code, _, stderr)) => Some(format!(
            "post_tool_use hook exited with code {code}{}",
            if stderr.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", stderr.trim())
            }
        )),
        Err(error) => Some(format!("post_tool_use hook failed: {error}")),
    }
}

pub async fn run_session_end(hooks: &HooksConfig, cwd: &std::path::Path) -> Option<String> {
    let script = hooks.session_end.as_ref()?;
    match run_hook(script, "session_end", json!({}), cwd, &CancelToken::new()).await {
        Ok((0, _, stderr)) => {
            let stderr = stderr.trim();
            (!stderr.is_empty()).then(|| stderr.to_string())
        }
        Ok((code, _, stderr)) => Some(format!(
            "session_end hook exited with code {code}{}",
            if stderr.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", stderr.trim())
            }
        )),
        Err(error) => Some(format!("session_end hook failed: {error}")),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn hooks(script: &str) -> HooksConfig {
        HooksConfig {
            pre_tool_use: Some(script.into()),
            post_tool_use: None,
            session_end: None,
        }
    }

    #[tokio::test]
    async fn pre_tool_allows_exit_zero() {
        let outcome = run_pre_tool(
            &hooks("exit 0"),
            "read",
            &json!({}),
            std::path::Path::new("."),
            &CancelToken::new(),
        )
        .await;
        assert!(outcome.allowed);
    }

    #[tokio::test]
    async fn pre_tool_denies_exit_two_and_other_failures() {
        for script in ["echo blocked >&2; exit 2", "exit 1"] {
            let outcome = run_pre_tool(
                &hooks(script),
                "read",
                &json!({}),
                std::path::Path::new("."),
                &CancelToken::new(),
            )
            .await;
            assert!(!outcome.allowed);
            assert!(outcome.message.is_some());
        }
    }

    #[tokio::test]
    async fn pre_tool_fails_closed_when_cancelled() {
        let cancel = CancelToken::new();
        cancel.cancel();
        let outcome = run_pre_tool(
            &hooks("sleep 5"),
            "read",
            &json!({}),
            std::path::Path::new("."),
            &cancel,
        )
        .await;
        assert!(!outcome.allowed);
    }
}
