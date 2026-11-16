use std::path::PathBuf;

use async_trait::async_trait;
use globset::GlobBuilder;
use kudelma_permissions::Scope;
use serde_json::{Value, json};

use crate::{
    Tool, ToolContext, ToolError, ToolOutput, ToolPermission, display_path, object_schema,
    optional_str, required_str, truncate_output,
};

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &'static str {
        "glob"
    }

    fn description(&self) -> &'static str {
        "Find files by glob pattern (e.g. `**/*.rs` or `src/**/*.ts`). Respects .gitignore."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "pattern": { "type": "string", "description": "Glob pattern relative to the search root" },
                "path": { "type": "string", "description": "Search root, defaults to the workspace" }
            }),
            &["pattern"],
        )
    }

    fn read_only(&self) -> bool {
        true
    }

    fn permission(&self, args: &Value, ctx: &ToolContext) -> Result<ToolPermission, ToolError> {
        let base = match optional_str(args, "path") {
            Some(path) => ctx.env.resolve(&path),
            None => ctx.cwd().to_path_buf(),
        };
        Ok(ToolPermission::new(vec![Scope::path(base)]))
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let pattern = required_str(&args, "pattern")?;
        let base = match optional_str(&args, "path") {
            Some(path) => ctx.env.resolve(&path),
            None => ctx.cwd().to_path_buf(),
        };
        let matcher = GlobBuilder::new(&pattern)
            .literal_separator(false)
            .build()
            .map_err(|error| ToolError::InvalidArgs(format!("invalid glob: {error}")))?
            .compile_matcher();
        let base_clone = base.clone();
        let files = tokio::task::spawn_blocking(move || {
            let mut files: Vec<PathBuf> = Vec::new();
            let walker = ignore::WalkBuilder::new(&base_clone)
                .standard_filters(true)
                .follow_links(false)
                .build();
            for entry in walker.flatten() {
                let Some(file_type) = entry.file_type() else {
                    continue;
                };
                if !file_type.is_file() {
                    continue;
                }
                let path = entry.into_path();
                let relative = path.strip_prefix(&base_clone).unwrap_or(&path);
                if matcher.is_match(relative) {
                    files.push(path);
                }
                if files.len() >= 1000 {
                    break;
                }
            }
            files
        })
        .await
        .map_err(|error| ToolError::Other(error.to_string()))?;

        if files.is_empty() {
            return Ok(ToolOutput::success(format!(
                "No files matched `{pattern}` under {}",
                display_path(ctx.cwd(), &base)
            )));
        }
        let total = files.len();
        let mut files = files;
        files.sort();
        let mut lines: Vec<String> = files
            .iter()
            .take(500)
            .map(|path| display_path(ctx.cwd(), path))
            .collect();
        if total > 500 {
            lines.push(format!("[... {} more files ...]", total - 500));
        }
        let (text, _) = truncate_output(
            &lines.join("\n"),
            ctx.config.agent.max_output_bytes,
            ctx.config.agent.max_output_lines,
        );
        Ok(ToolOutput::success(text).with_metadata(json!({ "files": total })))
    }
}
