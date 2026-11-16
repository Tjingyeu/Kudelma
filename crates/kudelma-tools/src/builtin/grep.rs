use std::path::Path;

use async_trait::async_trait;
use globset::GlobBuilder;
use kudelma_permissions::Scope;
use regex::RegexBuilder;
use serde_json::{Value, json};

use crate::{
    Tool, ToolContext, ToolError, ToolOutput, ToolPermission, clip_line, display_path,
    object_schema, optional_bool, optional_str, optional_usize, required_str, truncate_output,
};

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn description(&self) -> &'static str {
        "Search file contents with a regular expression. Returns path:line matches and respects .gitignore. Use `fixed` for literal text."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "pattern": { "type": "string", "description": "Regular expression, or literal text with fixed=true" },
                "path": { "type": "string", "description": "File or directory to search, defaults to the workspace" },
                "glob": { "type": "string", "description": "Optional file glob filter, e.g. `**/*.rs`" },
                "case_sensitive": { "type": "boolean", "description": "Default false" },
                "fixed": { "type": "boolean", "description": "Treat pattern as literal text" },
                "max_results": { "type": "integer", "description": "Default 100, maximum 500" }
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
        let case_sensitive = optional_bool(&args, "case_sensitive").unwrap_or(false);
        let fixed = optional_bool(&args, "fixed").unwrap_or(false);
        let max_results = optional_usize(&args, "max_results")
            .unwrap_or(100)
            .clamp(1, 500);
        let raw_pattern = if fixed {
            regex::escape(&pattern)
        } else {
            pattern.clone()
        };
        let regex = RegexBuilder::new(&raw_pattern)
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|error| ToolError::InvalidArgs(format!("invalid regex: {error}")))?;
        let base = match optional_str(&args, "path") {
            Some(path) => ctx.env.resolve(&path),
            None => ctx.cwd().to_path_buf(),
        };
        let glob = optional_str(&args, "glob")
            .map(|pattern| {
                GlobBuilder::new(&pattern)
                    .literal_separator(false)
                    .build()
                    .map(|glob| glob.compile_matcher())
                    .map_err(|error| ToolError::InvalidArgs(format!("invalid glob: {error}")))
            })
            .transpose()?;

        let base_clone = base.clone();
        let results = tokio::task::spawn_blocking(move || {
            let mut matches: Vec<String> = Vec::new();
            let mut files = 0usize;
            let mut visit = |path: &Path| -> bool {
                if let Some(glob) = &glob {
                    let relative = path.strip_prefix(&base_clone).unwrap_or(path);
                    if !glob.is_match(relative) {
                        return true;
                    }
                }
                let Ok(meta) = std::fs::metadata(path) else {
                    return true;
                };
                if meta.len() > MAX_FILE_BYTES {
                    return true;
                }
                let Ok(bytes) = std::fs::read(path) else {
                    return true;
                };
                if bytes.iter().take(4096).any(|byte| *byte == 0) {
                    return true;
                }
                let text = String::from_utf8_lossy(&bytes);
                let mut file_matched = false;
                for (index, line) in text.lines().enumerate() {
                    if regex.is_match(line) {
                        file_matched = true;
                        matches.push(format!(
                            "{}:{}: {}",
                            display_path(&base_clone, path),
                            index + 1,
                            clip_line(line.trim_end(), 300)
                        ));
                        if matches.len() >= max_results {
                            return false;
                        }
                    }
                }
                if file_matched {
                    files += 1;
                }
                true
            };

            if base_clone.is_file() {
                visit(&base_clone);
            } else {
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
                    if !visit(&entry.into_path()) {
                        break;
                    }
                }
            }
            drop(visit);
            (matches, files)
        })
        .await
        .map_err(|error| ToolError::Other(error.to_string()))?;

        let (matches, files) = results;
        if matches.is_empty() {
            return Ok(ToolOutput::success(format!(
                "No matches for `{pattern}` under {}",
                display_path(ctx.cwd(), &base)
            )));
        }
        let mut output = format!(
            "{} match{} in {files} file{}:\n{}",
            matches.len(),
            if matches.len() == 1 { "" } else { "es" },
            if files == 1 { "" } else { "s" },
            matches.join("\n")
        );
        if matches.len() >= max_results {
            output.push_str("\n[result limit reached; narrow the search]");
        }
        let (output, _) = truncate_output(
            &output,
            ctx.config.agent.max_output_bytes,
            ctx.config.agent.max_output_lines,
        );
        Ok(ToolOutput::success(output))
    }
}
