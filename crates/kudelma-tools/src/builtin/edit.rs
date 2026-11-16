use async_trait::async_trait;
use kudelma_permissions::Scope;
use serde_json::{Value, json};

use super::diff_preview;
use crate::{
    Tool, ToolContext, ToolError, ToolOutput, ToolPermission, display_path, object_schema,
    optional_bool, required_str,
};

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn description(&self) -> &'static str {
        "Replace an exact string in a file. By default old_string must occur exactly once; set replace_all to change every occurrence. Read the file first. Returns a unified diff."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "path": { "type": "string" },
                "old_string": { "type": "string", "description": "Exact text to replace, including indentation" },
                "new_string": { "type": "string", "description": "Replacement text" },
                "replace_all": { "type": "boolean", "description": "Replace every occurrence" }
            }),
            &["path", "old_string", "new_string"],
        )
    }

    fn permission(&self, args: &Value, ctx: &ToolContext) -> Result<ToolPermission, ToolError> {
        let path = ctx.env.resolve(&required_str(args, "path")?);
        Ok(ToolPermission::new(vec![Scope::path(path)]))
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let raw = required_str(&args, "path")?;
        let old_string = required_str(&args, "old_string")?;
        let new_string = required_str(&args, "new_string")?;
        let replace_all = optional_bool(&args, "replace_all").unwrap_or(false);
        if old_string == new_string {
            return Err(ToolError::InvalidArgs(
                "old_string and new_string are identical".into(),
            ));
        }
        if old_string.is_empty() {
            return Err(ToolError::InvalidArgs(
                "old_string must not be empty".into(),
            ));
        }
        let path = ctx.env.resolve(&raw);
        let meta =
            ctx.env.metadata(&path).await.map_err(|error| {
                ToolError::Io(format!("cannot read {}: {error}", path.display()))
            })?;
        if meta.is_dir {
            return Err(ToolError::Other(format!(
                "{} is a directory",
                display_path(ctx.cwd(), &path)
            )));
        }
        if let Some(recorded) = ctx.read_time(&path) {
            if meta.modified != Some(recorded) {
                return Err(ToolError::Other(format!(
                    "{} changed on disk since it was last read; read it again before editing",
                    display_path(ctx.cwd(), &path)
                )));
            }
        }
        let bytes =
            ctx.env.read(&path).await.map_err(|error| {
                ToolError::Io(format!("cannot read {}: {error}", path.display()))
            })?;
        let source = String::from_utf8_lossy(&bytes).to_string();
        let crlf = source.contains("\r\n");
        let normalized = source.replace("\r\n", "\n");
        let old = old_string.replace("\r\n", "\n");
        let new = new_string.replace("\r\n", "\n");
        let count = normalized.matches(&old).count();
        if count == 0 {
            return Err(ToolError::Other(format!(
                "old_string was not found in {}",
                display_path(ctx.cwd(), &path)
            )));
        }
        if count > 1 && !replace_all {
            return Err(ToolError::Other(format!(
                "old_string occurs {count} times in {}; include more context or set replace_all",
                display_path(ctx.cwd(), &path)
            )));
        }
        let updated = if replace_all {
            normalized.replace(&old, &new)
        } else {
            normalized.replacen(&old, &new, 1)
        };
        let diff = diff_preview(&normalized, &updated, 200);
        let output_text = if crlf {
            updated.replace('\n', "\r\n")
        } else {
            updated
        };
        ctx.env
            .write(&path, output_text.as_bytes())
            .await
            .map_err(|error| ToolError::Io(format!("cannot write {}: {error}", path.display())))?;
        let new_meta = ctx.env.metadata(&path).await.ok();
        ctx.record_read(path.clone(), new_meta.and_then(|meta| meta.modified));

        Ok(ToolOutput::success(format!(
            "Edited {} ({} replacement{}):\n\n{}",
            display_path(ctx.cwd(), &path),
            if replace_all { count } else { 1 },
            if (!replace_all && count == 1) || count == 1 {
                ""
            } else {
                "s"
            },
            diff
        ))
        .with_metadata(json!({
            "path": path.display().to_string(),
            "replacements": if replace_all { count } else { 1 },
        })))
    }
}
