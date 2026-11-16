use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use kudelma_config::{Config, INSTRUCTION_FILES};
use kudelma_storage::{MemoryStore, canonicalize_lenient};

use crate::agent::AgentMode;

const MAX_INSTRUCTION_BYTES: usize = 32 * 1024;
const MAX_INSTRUCTION_LEVELS: usize = 4;

pub fn build_system_prompt(
    config: &Config,
    cwd: &Path,
    mode: &AgentMode,
    custom: Option<&str>,
    append: Option<&str>,
) -> String {
    if let Some(custom) = custom {
        let mut prompt = custom.to_string();
        if let Some(append) = append {
            prompt.push_str("\n\n");
            prompt.push_str(append);
        }
        return prompt;
    }
    let mut prompt = String::new();
    prompt.push_str(
        "You are Kudelma, an AI coding agent running in the user's terminal.\n\
         Work directly in the repository, keep changes focused, and verify your work.\n\n",
    );
    prompt.push_str(&format!(
        "Working directory: {}\nPlatform: {}\nDate: {}\n\n",
        cwd.display(),
        std::env::consts::OS,
        chrono::Utc::now().format("%Y-%m-%d")
    ));
    prompt.push_str(
        "Tool guidelines:\n\
         - Use `index` to locate symbols before reading a large file; then `read` only the lines you need.\n\
         - Read a file before editing it. Use `edit` for targeted changes and `write` only for new files or full rewrites.\n\
         - Use `glob` and `grep` instead of shell `find`/`grep`; they respect .gitignore and return less noise.\n\
         - Use `todo_write` for multi-step work so progress is visible.\n\
         - Use `bash` for builds, tests and git. Long output keeps its beginning and end; full logs are saved for `read`.\n\
         - Prefer `task` with `explore` for broad code searches and with `plan` for architecture work; only the summary returns here.\n\
         - Never invent file contents. If a tool fails, read the error and try a different approach.\n\
         - Be concise. Explain what you changed and why, not a play-by-play.\n\n",
    );
    if let AgentMode::Plan(path) = mode {
        prompt.push_str(&format!(
            "# Plan mode\n\
             You are planning, not implementing. Explore the code and write a concrete plan to {}.\n\
             Do not modify other files and do not run mutating shell commands; ask before running anything.\n\n",
            path.display()
        ));
    }

    let memory = memory_context(cwd);
    if !memory.is_empty() {
        prompt.push_str("# Memories\n");
        prompt.push_str(&memory);
        prompt.push_str("\n\n");
    }
    let instructions = instruction_context(cwd);
    if !instructions.is_empty() {
        prompt.push_str("# Project instructions\n");
        prompt.push_str(&instructions);
        prompt.push_str("\n\n");
    }
    if let Some(extra) = config.system_prompt_extra() {
        prompt.push_str(&extra);
        prompt.push('\n');
    }
    if let Some(append) = append {
        prompt.push_str(append);
        prompt.push('\n');
    }
    prompt
}

pub fn build_subagent_prompt(cwd: &Path, kind: &str) -> String {
    let role = match kind {
        "explore" => {
            "You are an exploration subagent. Search the repository and report concrete findings. \
             Use read, glob, grep and index. Never edit files. Answer with file paths and line numbers."
        }
        "plan" => {
            "You are a planning subagent. Inspect the code and produce an ordered implementation plan \
             with risks and verification steps. Never edit files."
        }
        _ => {
            "You are an implementation subagent. Make the requested focused change, run the relevant \
             checks, and report exactly what changed. Do not expand the scope."
        }
    };
    format!(
        "{role}\n\nWorking directory: {}\nPlatform: {}\n\
         Keep the final report short: what you found or changed, file:line references, and any \
         command whose output matters.\n",
        cwd.display(),
        std::env::consts::OS
    )
}

pub fn instruction_context(cwd: &Path) -> String {
    let mut output = String::new();
    let mut seen = HashSet::new();
    let mut dirs: Vec<PathBuf> = cwd
        .ancestors()
        .take(MAX_INSTRUCTION_LEVELS)
        .map(Path::to_path_buf)
        .collect();
    dirs.reverse();
    for dir in dirs {
        for file in INSTRUCTION_FILES {
            let path = dir.join(file);
            let key = canonicalize_lenient(&path);
            if !seen.insert(key) {
                continue;
            }
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let text = truncate(&text, MAX_INSTRUCTION_BYTES);
            if text.trim().is_empty() {
                continue;
            }
            output.push_str(&format!("From {}:\n{}\n\n", path.display(), text.trim()));
            break;
        }
    }
    if let Ok(global) = std::env::var("KUDELMA_CONFIG_DIR") {
        let path = PathBuf::from(global).join("AGENTS.md");
        if let Ok(text) = fs::read_to_string(path) {
            output.push_str(&format!(
                "Global instructions:\n{}\n",
                truncate(&text, MAX_INSTRUCTION_BYTES)
            ));
        }
    }
    output
}

fn memory_context(cwd: &Path) -> String {
    let Ok(store) = MemoryStore::new(cwd) else {
        return String::new();
    };
    let mut output = String::new();
    for entry in store.list("project").into_iter().take(10) {
        output.push_str(&format!("- {}\n", entry.content));
    }
    for entry in store.list("global").into_iter().take(10) {
        output.push_str(&format!("- [global] {}\n", entry.content));
    }
    output
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[instructions truncated]", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_project_instructions() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "Use tabs.").unwrap();
        let text = instruction_context(dir.path());
        assert!(text.contains("Use tabs."));
    }

    #[test]
    fn plan_mode_is_named_in_the_prompt() {
        let config = Config::default();
        let dir = tempfile::tempdir().unwrap();
        let prompt = build_system_prompt(
            &config,
            dir.path(),
            &AgentMode::Plan(dir.path().join("plan.md")),
            None,
            None,
        );
        assert!(prompt.contains("Plan mode"));
    }
}
