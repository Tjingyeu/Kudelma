use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use kudelma_config::{Config, INSTRUCTION_FILES};
use kudelma_storage::{MemoryStore, canonicalize_lenient};

use crate::lead::LeadMode;
use crate::orchestration::TaskContract;
use crate::skills::SkillRegistry;
use crate::workflows::WorkflowState;

const MAX_INSTRUCTION_BYTES: usize = 32 * 1024;
const MAX_INSTRUCTION_LEVELS: usize = 4;

pub fn build_agency_prompt(
    config: &Config,
    cwd: &Path,
    skills: &SkillRegistry,
    contract: Option<&TaskContract>,
    workflow: Option<&WorkflowState>,
) -> String {
    let mut prompt = build_agency_stable_prompt(config, cwd);
    prompt.push('\n');
    prompt.push_str(&build_agency_context(
        config, cwd, skills, contract, workflow,
    ));
    prompt
}

pub fn build_agency_stable_prompt(config: &Config, cwd: &Path) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "You are Agency, the developer-facing intent agent for Kudelma.\n\
         You clarify what the developer wants. You do not implement code, run commands, or make final technical decisions.\n\
         Exploration is a shared read-only capability, not another agent or model: `explore_codebase` performs deterministic retrieval and returns file:symbol:line evidence that you interpret yourself.\n\n",
    );
    prompt.push_str(&format!(
        "Working directory: {}\nPlatform: {}\n\n",
        cwd.display(),
        std::env::consts::OS
    ));
    prompt.push_str(
        "Routing rules:\n\
         - General question: answer directly and concisely.\n\
         - Codebase or architecture question: call `explore_codebase` for deterministic read-only evidence, then answer with file:symbol:line citations. Do not activate Lead.\n\
         - Feature, bug, change, or planning request: ask only questions that materially change scope or acceptance; then call `submit_contract`.\n\
         - Status question: explain the current contract/workflow state if available, without starting work.\n\
         - Never edit files, never run shell commands, never dispatch coder, never call MCP.\n\
         - Ask at most one focused clarification question per turn; record safe assumptions instead of blocking.\n\
         - If important ambiguity remains, put it in unresolved_questions and keep the contract in needs_clarification.\n\
         - Do not mark work approved yourself. Developer approval is a separate step.\n\n",
    );
    if let Some(extra) = config.system_prompt_extra() {
        prompt.push_str(&extra);
    }
    prompt
}

pub fn build_agency_context(
    _config: &Config,
    cwd: &Path,
    skills: &SkillRegistry,
    contract: Option<&TaskContract>,
    workflow: Option<&WorkflowState>,
) -> String {
    let mut prompt = format!("Date: {}\n", chrono::Utc::now().format("%Y-%m-%d"));
    if let Some(contract) = contract {
        prompt.push_str("## Current Task Contract\n");
        prompt.push_str(&contract.render());
        prompt.push_str(
            "\nUse submit_contract to revise it when the developer changes the request.\n\n",
        );
    }
    if let Some(workflow) = workflow {
        prompt.push_str("## Current Workflow\n");
        prompt.push_str(&workflow.render());
        prompt.push('\n');
    }
    let skill_section = skills.prompt_section("agency", "build");
    if !skill_section.is_empty() {
        prompt.push_str("## Skills\n");
        prompt.push_str(&skill_section);
        prompt.push('\n');
    }
    let memory = memory_context(cwd);
    if !memory.is_empty() {
        prompt.push_str("## Memories\n");
        prompt.push_str(&memory);
        prompt.push('\n');
    }
    prompt
}

pub fn build_lead_prompt(
    config: &Config,
    cwd: &Path,
    mode: &LeadMode,
    skills: &SkillRegistry,
    contract: Option<&TaskContract>,
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
        "You are Lead, the single technical decision-maker and orchestrator for Kudelma.\n\
         You own PLAN, BUILD, DEBUG and REVIEW as modes of this same session. You may implement inline or delegate bounded implementation to Coder. Use `explore_codebase` for shared deterministic read-only evidence; it does not start another model.\n\n",
    );
    prompt.push_str(&format!(
        "Working directory: {}\nPlatform: {}\nDate: {}\nCurrent mode: {}\n\n",
        cwd.display(),
        std::env::consts::OS,
        chrono::Utc::now().format("%Y-%m-%d"),
        mode.name()
    ));
    if let Some(contract) = contract {
        prompt.push_str("## Approved Task Contract\n");
        prompt.push_str(&contract.render());
        prompt.push('\n');
    }
    prompt.push_str(match mode {
        LeadMode::Plan(_) => {
            "PLAN posture: read-only. Gather evidence, explore alternatives, write a small ordered plan with tests and risks. Use `lead_mode` to move to build when the plan is actionable.\n"
        }
        LeadMode::Build => {
            "BUILD posture: implement the approved plan or task. Keep changes focused. Use TDD when behavior changes. Use `verify` for commands that must become evidence. Use `lead_mode` to move to review.\n"
        }
        LeadMode::Debug => {
            "DEBUG posture: reproduce, collect evidence, test hypotheses, identify the root cause, then fix the smallest cause. Do not stack speculative edits. Use `verify` for regression checks.\n"
        }
        LeadMode::Review => {
            "REVIEW posture: compare the actual diff and evidence with every acceptance criterion, then check quality, error handling, security and compatibility. Do not edit code in review. A final answer in this mode records review completion.\n"
        }
    });
    prompt.push_str(
        "\nTool rules:\n\
         - Use `index` before reading large files; read before editing.\n\
         - Use `grep`/`glob` for narrow searches and `explore_codebase` for task-scoped evidence; deterministic exploration has no model of its own.\n\
         - Use `task` only to delegate bounded implementation to a Coder.\n\
         - Do not claim a test/build passed without evidence from `verify`.\n\
         - Only switch modes with the `lead_mode` tool and state a reason.\n\
         - Ask the developer only through Agency; if blocked, report the blocker instead of inventing requirements.\n\
         - Be concise; explain changes and evidence, not a play-by-play.\n\n",
    );
    let skill_section = skills.prompt_section("lead", mode.name());
    if !skill_section.is_empty() {
        prompt.push_str("## Skills\n");
        prompt.push_str(&skill_section);
        prompt.push('\n');
    }
    let memory = memory_context(cwd);
    if !memory.is_empty() {
        prompt.push_str("## Memories\n");
        prompt.push_str(&memory);
        prompt.push('\n');
    }
    let instructions = instruction_context(cwd);
    if !instructions.is_empty() {
        prompt.push_str("## Project instructions\n");
        prompt.push_str(&instructions);
        prompt.push('\n');
    }
    if let Some(extra) = config.system_prompt_extra() {
        prompt.push_str(&extra);
    }
    if let Some(append) = append {
        prompt.push_str(append);
        prompt.push('\n');
    }
    prompt
}

pub fn build_lead_stable_prompt(
    config: &Config,
    cwd: &Path,
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
        "You are Lead, the single technical decision-maker and orchestrator for Kudelma.\n\
         You own PLAN, BUILD, DEBUG and REVIEW as modes of this same session. You may implement inline or delegate bounded implementation to Coder. Use `explore_codebase` for shared deterministic read-only evidence; it does not start another model.\n\n",
    );
    prompt.push_str(&format!(
        "Working directory: {}\nPlatform: {}\n\n",
        cwd.display(),
        std::env::consts::OS
    ));
    prompt.push_str(
        "\nTool rules:\n\
         - Use `index` before reading large files; read before editing.\n\
         - Use `grep`/`glob` for narrow searches and `explore_codebase` for task-scoped evidence; deterministic exploration has no model of its own.\n\
         - Use `task` only to delegate bounded implementation to a Coder.\n\
         - Do not claim a test/build passed without evidence from `verify`.\n\
         - Only switch modes with the `lead_mode` tool and state a reason.\n\
         - Ask the developer only through Agency; if blocked, report the blocker instead of inventing requirements.\n\
         - Be concise; explain changes and evidence, not a play-by-play.\n\n",
    );
    if let Some(extra) = config.system_prompt_extra() {
        prompt.push_str(&extra);
    }
    if let Some(append) = append {
        prompt.push_str(append);
        prompt.push('\n');
    }
    prompt
}

pub fn build_lead_context(
    _config: &Config,
    cwd: &Path,
    mode: &LeadMode,
    skills: &SkillRegistry,
    contract: Option<&TaskContract>,
    workflow: Option<&WorkflowState>,
) -> String {
    let mut prompt = format!(
        "Date: {}\nCurrent mode: {}\n",
        chrono::Utc::now().format("%Y-%m-%d"),
        mode.name()
    );
    prompt.push_str(match mode {
        LeadMode::Plan(_) => {
            "PLAN posture: read-only. Gather evidence, explore alternatives, write a small ordered plan with tests and risks. Use `lead_mode` to move to build when the plan is actionable.\n"
        }
        LeadMode::Build => {
            "BUILD posture: implement the approved plan or task. Keep changes focused. Use TDD when behavior changes. Use `verify` for commands that must become evidence. Use `lead_mode` to move to review.\n"
        }
        LeadMode::Debug => {
            "DEBUG posture: reproduce, collect evidence, test hypotheses, identify the root cause, then fix the smallest cause. Do not stack speculative edits. Use `verify` for regression checks.\n"
        }
        LeadMode::Review => {
            "REVIEW posture: compare the actual diff and evidence with every acceptance criterion, then check quality, error handling, security and compatibility. Do not edit code in review. Call `submit_review` with a structured report; a final answer in this mode is not enough to pass the review gate.\n"
        }
    });
    if let LeadMode::Plan(path) = mode {
        prompt.push_str(&format!("Plan artifact path: {}\n", path.display()));
    }
    if let Some(contract) = contract {
        prompt.push_str("## Approved Task Contract\n");
        prompt.push_str(&contract.render());
        prompt.push('\n');
    }
    if let Some(workflow) = workflow {
        prompt.push_str("## Current Workflow\n");
        prompt.push_str(&workflow.render());
        prompt.push('\n');
    }
    let skill_section = skills.prompt_section("lead", mode.name());
    if !skill_section.is_empty() {
        prompt.push_str("## Skills\n");
        prompt.push_str(&skill_section);
        prompt.push('\n');
    }
    let memory = memory_context(cwd);
    if !memory.is_empty() {
        prompt.push_str("## Memories\n");
        prompt.push_str(&memory);
        prompt.push('\n');
    }
    let instructions = instruction_context(cwd);
    if !instructions.is_empty() {
        prompt.push_str("## Project instructions\n");
        prompt.push_str(&instructions);
        prompt.push('\n');
    }
    prompt
}

pub fn build_system_prompt(
    config: &Config,
    cwd: &Path,
    mode: &LeadMode,
    custom: Option<&str>,
    append: Option<&str>,
) -> String {
    build_lead_prompt(
        config,
        cwd,
        mode,
        &SkillRegistry::empty(),
        None,
        custom,
        append,
    )
}

pub fn build_subagent_prompt(cwd: &Path) -> String {
    format!(
        "You are a Coder, a scoped implementation worker. Make the requested focused change, \
         run authorized checks, and report exactly what changed. Do not expand the scope or dispatch agents.\n\n\
         Working directory: {}\nPlatform: {}\n\
         Keep the final report short: what you changed, file:line references, and any command whose output matters.\n",
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
            &LeadMode::Plan(dir.path().join("plan.md")),
            None,
            None,
        );
        assert!(prompt.contains("PLAN posture"));
    }

    #[test]
    fn stable_prompt_excludes_dynamic_state() {
        let config = Config::default();
        let dir = tempfile::tempdir().unwrap();
        let stable = build_lead_stable_prompt(&config, dir.path(), None, None);
        assert!(!stable.contains("Current mode:"));
        assert!(!stable.contains("Date:"));
        let context = build_lead_context(
            &config,
            dir.path(),
            &LeadMode::Plan(dir.path().join("plan.md")),
            &SkillRegistry::empty(),
            None,
            None,
        );
        assert!(context.contains("Current mode: plan"));
        assert!(context.contains("Date:"));
        assert!(context.contains("PLAN posture"));
    }

    #[test]
    fn agency_stable_prompt_excludes_contract_and_workflow() {
        let config = Config::default();
        let dir = tempfile::tempdir().unwrap();
        let stable = build_agency_stable_prompt(&config, dir.path());
        assert!(!stable.contains("Current Task Contract"));
        assert!(!stable.contains("Current Workflow"));
        assert!(!stable.contains("Date:"));
    }
}
