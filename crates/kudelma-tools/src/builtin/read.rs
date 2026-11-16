use async_trait::async_trait;
use kudelma_permissions::Scope;
use serde_json::{Value, json};

use crate::{
    Tool, ToolContext, ToolError, ToolOutput, ToolPermission, clip_line, display_path,
    object_schema, optional_usize, required_str, truncate_output,
};

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "read"
    }

    fn description(&self) -> &'static str {
        "Read a text file with 1-based line numbers. Use offset and limit for large files. Read before editing."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "path": { "type": "string", "description": "File path, relative to the workspace or absolute" },
                "offset": { "type": "integer", "minimum": 1, "description": "First line to return (1-based)" },
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum number of lines" }
            }),
            &["path"],
        )
    }

    fn read_only(&self) -> bool {
        true
    }

    fn permission(&self, args: &Value, ctx: &ToolContext) -> Result<ToolPermission, ToolError> {
        let path = ctx.env.resolve(&required_str(args, "path")?);
        Ok(ToolPermission::new(vec![Scope::path(path)]))
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let raw = required_str(&args, "path")?;
        let path = ctx.env.resolve(&raw);
        let meta =
            ctx.env.metadata(&path).await.map_err(|error| {
                ToolError::Io(format!("cannot read {}: {error}", path.display()))
            })?;
        if meta.is_dir {
            return Err(ToolError::Other(format!(
                "{} is a directory; use ls",
                display_path(ctx.cwd(), &path)
            )));
        }
        let bytes =
            ctx.env.read(&path).await.map_err(|error| {
                ToolError::Io(format!("cannot read {}: {error}", path.display()))
            })?;
        if bytes.iter().take(8192).any(|byte| *byte == 0) {
            return Err(ToolError::Other(format!(
                "{} looks binary; read is for text files",
                display_path(ctx.cwd(), &path)
            )));
        }
        let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
        let total_lines = text.lines().count();
        let offset = optional_usize(&args, "offset").unwrap_or(1).max(1);
        if total_lines == 0 {
            ctx.record_read(path.clone(), meta.modified);
            return Ok(ToolOutput::success(format!(
                "{} is empty (0 lines)",
                display_path(ctx.cwd(), &path)
            )));
        }
        if offset > total_lines {
            return Err(ToolError::InvalidArgs(format!(
                "offset {offset} is past the end of the file ({total_lines} lines)"
            )));
        }
        let limit = optional_usize(&args, "limit")
            .unwrap_or(2000)
            .clamp(1, 5000);
        let end = (offset + limit - 1).min(total_lines);
        let width = total_lines.to_string().len();
        let mut body = String::new();
        for (index, line) in text.lines().enumerate() {
            let number = index + 1;
            if number < offset || number > end {
                continue;
            }
            body.push_str(&format!(
                "{number:>width$}: {}\n",
                clip_line(line, 2000),
                width = width
            ));
        }
        if end < total_lines {
            body.push_str(&format!(
                "[showing lines {offset}-{end} of {total_lines}; continue with offset {}]\n",
                end + 1
            ));
        }
        let (body, _) = truncate_output(
            body.trim_end(),
            ctx.config.agent.max_output_bytes,
            ctx.config.agent.max_output_lines,
        );
        ctx.record_read(path.clone(), meta.modified);
        Ok(ToolOutput::success(body).with_metadata(json!({
            "path": path.display().to_string(),
            "lines": total_lines,
            "offset": offset,
            "end": end,
        })))
    }
}
