use async_trait::async_trait;
use kudelma_permissions::Scope;
use serde_json::{Value, json};

use super::diff_preview;
use crate::{
    Tool, ToolContext, ToolError, ToolOutput, ToolPermission, display_path, object_schema,
    required_str,
};

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &'static str {
        "write"
    }

    fn description(&self) -> &'static str {
        "Create a file or replace its entire contents. Prefer edit for existing files. Reads the old contents first and refuses to overwrite a file changed since it was last read."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "path": { "type": "string", "description": "File path, relative to the workspace or absolute" },
                "content": { "type": "string", "description": "Full new file contents" }
            }),
            &["path", "content"],
        )
    }

    fn permission(&self, args: &Value, ctx: &ToolContext) -> Result<ToolPermission, ToolError> {
        let path = ctx.env.resolve(&required_str(args, "path")?);
        Ok(ToolPermission::new(vec![Scope::path(path)]))
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let raw = required_str(&args, "path")?;
        let content = required_str(&args, "content")?;
        let path = ctx.env.resolve(&raw);
        let existing = ctx.env.metadata(&path).await.ok();
        let old = if existing.is_some() {
            if let Some(recorded) = ctx.read_time(&path) {
                if existing.as_ref().and_then(|meta| meta.modified) != Some(recorded) {
                    return Err(ToolError::Other(format!(
                        "{} changed on disk since it was last read; read it again before writing",
                        display_path(ctx.cwd(), &path)
                    )));
                }
            }
            ctx.env
                .read(&path)
                .await
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).replace("\r\n", "\n"))
        } else {
            None
        };
        ctx.env
            .write(&path, content.as_bytes())
            .await
            .map_err(|error| ToolError::Io(format!("cannot write {}: {error}", path.display())))?;
        let meta = ctx.env.metadata(&path).await.ok();
        ctx.record_read(path.clone(), meta.and_then(|meta| meta.modified));

        let relative = display_path(ctx.cwd(), &path);
        let mut output = format!("Wrote {} bytes to {relative}", content.len());
        if let Some(old) = old {
            let diff = diff_preview(&old, &content, 120);
            if !diff.trim().is_empty() {
                output.push_str("\n\n");
                output.push_str(&diff);
            }
        }
        Ok(ToolOutput::success(output).with_metadata(json!({
            "path": path.display().to_string(),
            "bytes": content.len(),
            "created": existing.is_none(),
        })))
    }
}
