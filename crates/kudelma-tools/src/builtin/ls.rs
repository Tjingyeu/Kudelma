use async_trait::async_trait;
use kudelma_permissions::Scope;
use serde_json::{Value, json};

use crate::{
    Tool, ToolContext, ToolError, ToolOutput, ToolPermission, display_path, object_schema,
    optional_str, truncate_output,
};

pub struct LsTool;

#[async_trait]
impl Tool for LsTool {
    fn name(&self) -> &'static str {
        "ls"
    }

    fn description(&self) -> &'static str {
        "List a directory. Directories end with `/`."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "path": { "type": "string", "description": "Directory, defaults to the workspace" }
            }),
            &[],
        )
    }

    fn read_only(&self) -> bool {
        true
    }

    fn permission(&self, args: &Value, ctx: &ToolContext) -> Result<ToolPermission, ToolError> {
        let path = match optional_str(args, "path") {
            Some(path) => ctx.env.resolve(&path),
            None => ctx.cwd().to_path_buf(),
        };
        Ok(ToolPermission::new(vec![Scope::path(path)]))
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = match optional_str(&args, "path") {
            Some(path) => ctx.env.resolve(&path),
            None => ctx.cwd().to_path_buf(),
        };
        let mut entries =
            ctx.env.list(&path).await.map_err(|error| {
                ToolError::Io(format!("cannot list {}: {error}", path.display()))
            })?;
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        if entries.is_empty() {
            return Ok(ToolOutput::success(format!(
                "{} is empty",
                display_path(ctx.cwd(), &path)
            )));
        }
        let total = entries.len();
        let mut lines: Vec<String> = entries
            .iter()
            .take(400)
            .map(|entry| {
                if entry.is_dir {
                    format!("{}/", entry.name)
                } else {
                    format!("{}  ({})", entry.name, human_size(entry.size))
                }
            })
            .collect();
        if total > 400 {
            lines.push(format!("[... {} more entries ...]", total - 400));
        }
        let (text, _) = truncate_output(
            &lines.join("\n"),
            ctx.config.agent.max_output_bytes,
            ctx.config.agent.max_output_lines,
        );
        Ok(ToolOutput::success(format!(
            "{}\n{}",
            display_path(ctx.cwd(), &path),
            text
        )))
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
