#![allow(clippy::collapsible_if)]

//! Fail-closed permission checks and shell command analysis.

pub mod bash;
pub mod scope;

pub use bash::{BashAnalysis, analyze_bash};
pub use scope::{Scope, is_universal, scope_matches};

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use kudelma_config::{Config, DefaultEffect, Effect, PermissionRule, PermissionsConfig, ToolKey};
use kudelma_storage::{atomic_write, canonicalize_lenient, is_within};

pub const WRITE_TOOLS: &[&str] = &["write", "edit", "multiedit", "edit_lines", "insert_lines"];
pub const READ_TOOLS: &[&str] = &["read", "glob", "grep", "index", "ls", "explore_codebase"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Agency,
    Lead,
    Coder,
}

impl AgentRole {
    pub fn name(self) -> &'static str {
        match self {
            Self::Agency => "agency",
            Self::Lead => "lead",
            Self::Coder => "coder",
        }
    }

    pub fn read_only(self) -> bool {
        matches!(self, Self::Agency)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Plan,
    Build,
    Debug,
    Review,
}

impl PermissionMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Build => "build",
            Self::Debug => "debug",
            Self::Review => "review",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CoderScope {
    pub allowed_paths: Vec<PathBuf>,
    pub forbidden_paths: Vec<PathBuf>,
    pub allow_shell: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionCheck {
    Allowed,
    Denied(String),
    NeedsPrompt {
        tool: ToolKey,
        scopes: Vec<Scope>,
        reason: Option<String>,
        requires_exact: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionAnswer {
    AllowOnce,
    AllowSession,
    AllowAlwaysProject,
    AllowAlwaysGlobal,
    Deny,
    DenyWithGuidance(String),
    DenyAlwaysProject,
    DenyAlwaysGlobal,
}

impl PermissionAnswer {
    pub fn is_allow(&self) -> bool {
        matches!(
            self,
            Self::AllowOnce
                | Self::AllowSession
                | Self::AllowAlwaysProject
                | Self::AllowAlwaysGlobal
        )
    }

    pub fn guidance(&self) -> Option<&str> {
        match self {
            Self::DenyWithGuidance(text) => Some(text),
            _ => None,
        }
    }

    pub fn encode(&self) -> String {
        match self {
            Self::AllowOnce => "allow".into(),
            Self::AllowSession => "allow_session".into(),
            Self::AllowAlwaysProject => "allow_project".into(),
            Self::AllowAlwaysGlobal => "allow_global".into(),
            Self::Deny => "deny".into(),
            Self::DenyWithGuidance(text) => format!("deny:{text}"),
            Self::DenyAlwaysProject => "deny_project".into(),
            Self::DenyAlwaysGlobal => "deny_global".into(),
        }
    }

    pub fn decode(raw: &str) -> Option<Self> {
        Some(match raw {
            "allow" => Self::AllowOnce,
            "allow_session" => Self::AllowSession,
            "allow_project" => Self::AllowAlwaysProject,
            "allow_global" => Self::AllowAlwaysGlobal,
            "deny" => Self::Deny,
            "deny_project" => Self::DenyAlwaysProject,
            "deny_global" => Self::DenyAlwaysGlobal,
            other => Self::DenyWithGuidance(other.strip_prefix("deny:")?.to_string()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaggedAnswer {
    pub request_id: String,
    pub answer: PermissionAnswer,
}

impl TaggedAnswer {
    pub fn new(request_id: impl Into<String>, answer: PermissionAnswer) -> Self {
        Self {
            request_id: request_id.into(),
            answer,
        }
    }
}

pub struct PermissionManager {
    cwd: PathBuf,
    rules: Vec<PermissionRule>,
    default: DefaultEffect,
    session_rules: RwLock<Vec<PermissionRule>>,
    yolo: bool,
    plan_path: RwLock<Option<PathBuf>>,
    trusted: bool,
    project_file: PathBuf,
    global_file: PathBuf,
    pub role: AgentRole,
    pub coder_scope: CoderScope,
    pub mode: RwLock<PermissionMode>,
}

impl PermissionManager {
    pub fn new(config: &Config, cwd: &Path, trusted: bool, yolo_flag: bool) -> Self {
        let mut rules = config.permissions.rules.clone();
        for rule in &config.project_permissions.rules {
            if trusted || rule.effect == Effect::Deny {
                rules.push(rule.clone());
            }
        }
        let default = if trusted {
            config
                .project_permissions
                .default
                .unwrap_or(config.permissions.default)
        } else {
            config.permissions.default
        };
        let yolo = yolo_flag
            || config.permissions.yolo
            || (trusted && config.project_permissions.yolo.unwrap_or(false));
        Self {
            cwd: cwd.to_path_buf(),
            rules,
            default,
            session_rules: RwLock::new(Vec::new()),
            yolo,
            plan_path: RwLock::new(None),
            trusted,
            project_file: kudelma_config::project_permissions_path(cwd),
            global_file: kudelma_config::global_config_path(),
            role: AgentRole::Lead,
            coder_scope: CoderScope::default(),
            mode: RwLock::new(PermissionMode::Build),
        }
    }

    pub fn with_files(mut self, project_file: PathBuf, global_file: PathBuf) -> Self {
        self.project_file = project_file;
        self.global_file = global_file;
        self
    }

    pub fn from_rules(cwd: &Path, config: PermissionsConfig) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            rules: config.rules,
            default: config.default,
            session_rules: RwLock::new(Vec::new()),
            yolo: config.yolo,
            plan_path: RwLock::new(None),
            trusted: true,
            project_file: cwd.join(".kudelma/permissions.toml"),
            global_file: cwd.join(".kudelma/global.toml"),
            role: AgentRole::Lead,
            coder_scope: CoderScope::default(),
            mode: RwLock::new(PermissionMode::Build),
        }
    }

    pub fn for_role(&self, role: AgentRole, coder_scope: CoderScope) -> Self {
        Self {
            cwd: self.cwd.clone(),
            rules: self.rules.clone(),
            default: self.default,
            session_rules: RwLock::new(
                self.session_rules
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
            ),
            yolo: self.yolo,
            plan_path: RwLock::new(None),
            trusted: self.trusted,
            project_file: self.project_file.clone(),
            global_file: self.global_file.clone(),
            role,
            coder_scope,
            mode: RwLock::new(*self.mode.read().unwrap_or_else(|error| error.into_inner())),
        }
    }

    pub fn for_mode(&self, mode: PermissionMode) -> Self {
        let manager = self.for_role(self.role, self.coder_scope.clone());
        *manager
            .mode
            .write()
            .unwrap_or_else(|error| error.into_inner()) = mode;
        *manager
            .plan_path
            .write()
            .unwrap_or_else(|error| error.into_inner()) = self.plan_path();
        manager
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn trusted(&self) -> bool {
        self.trusted
    }

    pub fn is_yolo(&self) -> bool {
        self.yolo
    }

    pub fn set_yolo(&mut self, yolo: bool) {
        self.yolo = yolo;
    }

    pub fn set_plan_path(&self, path: Option<PathBuf>) {
        *self.plan_path.write().unwrap_or_else(|e| e.into_inner()) = path;
    }

    pub fn plan_path(&self) -> Option<PathBuf> {
        self.plan_path
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn mode(&self) -> PermissionMode {
        *self.mode.read().unwrap_or_else(|error| error.into_inner())
    }

    pub fn set_mode(&self, mode: PermissionMode) {
        *self.mode.write().unwrap_or_else(|error| error.into_inner()) = mode;
    }

    pub fn session_rules(&self) -> Vec<PermissionRule> {
        self.session_rules
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Pre-approve every scope of a tool for this session. Used by
    /// `--allowed-tools`, which is an explicit user decision.
    pub fn allow_tool_for_session(&self, name: &str) {
        let rule = PermissionRule {
            tool: ToolKey::Native(name.to_string()),
            scope: None,
            effect: Effect::Allow,
        };
        let mut rules = self
            .session_rules
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if !rules
            .iter()
            .any(|existing| existing.tool == rule.tool && existing.scope.is_none())
        {
            rules.push(rule);
        }
    }

    pub fn check(&self, tool: &ToolKey, scopes: &[Scope]) -> PermissionCheck {
        self.check_with_force(tool, scopes, false)
    }

    pub fn check_with_force(
        &self,
        tool: &ToolKey,
        scopes: &[Scope],
        force_prompt: bool,
    ) -> PermissionCheck {
        self.check_with_context(tool, scopes, force_prompt, None)
    }

    pub fn check_with_context(
        &self,
        tool: &ToolKey,
        scopes: &[Scope],
        force_prompt: bool,
        cwd: Option<&Path>,
    ) -> PermissionCheck {
        if scopes.is_empty() {
            return self.check_scopes(tool, &[Scope::exact("")], force_prompt, cwd);
        }
        self.check_scopes(tool, scopes, force_prompt, cwd)
    }

    fn check_scopes(
        &self,
        tool: &ToolKey,
        scopes: &[Scope],
        force_prompt: bool,
        cwd: Option<&Path>,
    ) -> PermissionCheck {
        let session = self.session_rules.read().unwrap_or_else(|e| e.into_inner());
        let all_rules = || session.iter().chain(self.rules.iter());
        let rule_key = rule_key_for(tool);

        for scope in scopes {
            for rule in all_rules() {
                if rule.effect == Effect::Deny
                    && reaches(&rule.tool, &rule_key)
                    && rule_matches(rule, scope)
                {
                    return PermissionCheck::Denied(match &rule.scope {
                        Some(pattern) => {
                            format!("deny rule `{pattern}` for `{}`", rule.tool)
                        }
                        None => format!("blanket deny rule for `{}`", rule.tool),
                    });
                }
            }
        }

        if let Some(reason) = self.role_denial(tool, scopes) {
            return PermissionCheck::Denied(reason);
        }

        let mode = self.mode();
        if matches!(tool, ToolKey::Native(name) if name == "submit_review")
            && mode != PermissionMode::Review
        {
            return PermissionCheck::Denied(
                "submit_review is only available in review mode".into(),
            );
        }
        if mode == PermissionMode::Plan {
            if is_write_tool(tool) {
                let plan = self.plan_path();
                let all_plan = plan.is_some_and(|path| {
                    let normalized = canonicalize_lenient(&path);
                    scopes
                        .iter()
                        .all(|scope| canonicalize_lenient(Path::new(&scope.value)) == normalized)
                });
                if all_plan {
                    return PermissionCheck::Allowed;
                }
                return PermissionCheck::Denied(
                    "plan mode only allows writing the plan file".into(),
                );
            }
            if is_bash(tool) || is_verify(tool) || tool.is_mcp() || is_task(tool) || is_memory(tool)
            {
                return PermissionCheck::Denied(
                    "plan mode is read-only: commands, MCP, task dispatch and memory writes are blocked"
                        .into(),
                );
            }
        }
        if mode == PermissionMode::Review {
            if is_write_tool(tool)
                || is_bash(tool)
                || tool.is_mcp()
                || is_task(tool)
                || is_memory(tool)
            {
                return PermissionCheck::Denied(
                    "review mode is read-only: code edits, shell, MCP, task dispatch and memory writes are blocked"
                        .into(),
                );
            }
            if is_verify(tool) {
                return PermissionCheck::Allowed;
            }
        }

        if let Some(cwd) = cwd
            && (is_bash(tool) || is_verify(tool))
            && !is_within(&self.cwd, cwd)
        {
            return PermissionCheck::NeedsPrompt {
                tool: tool.clone(),
                scopes: scopes.to_vec(),
                reason: Some("working directory is outside the workspace".into()),
                requires_exact: true,
            };
        }

        let mut pending = Vec::new();
        for scope in scopes {
            let allowed = all_rules().any(|rule| {
                rule.effect == Effect::Allow
                    && reaches(&rule.tool, &rule_key)
                    && rule_matches(rule, scope)
                    && !(force_prompt && rule.scope.is_none())
            });
            if !allowed {
                pending.push(scope.clone());
            }
        }
        if pending.is_empty() {
            return PermissionCheck::Allowed;
        }
        if force_prompt {
            if self.default == DefaultEffect::Deny {
                return PermissionCheck::Denied(format!(
                    "denied by the default permission for `{tool}`"
                ));
            }
            return PermissionCheck::NeedsPrompt {
                tool: tool.clone(),
                scopes: pending,
                reason: Some("command requires exact approval".into()),
                requires_exact: true,
            };
        }
        if self.yolo {
            return PermissionCheck::Allowed;
        }

        if let Some(check) = self.builtin_check(tool, &pending) {
            return check;
        }

        match self.default {
            DefaultEffect::Allow => PermissionCheck::Allowed,
            DefaultEffect::Deny => {
                PermissionCheck::Denied(format!("denied by the default permission for `{tool}`"))
            }
            DefaultEffect::Prompt => PermissionCheck::NeedsPrompt {
                tool: tool.clone(),
                scopes: pending,
                reason: None,
                requires_exact: false,
            },
        }
    }

    fn role_denial(&self, tool: &ToolKey, scopes: &[Scope]) -> Option<String> {
        if let ToolKey::Native(name) = tool {
            match name.as_str() {
                "submit_contract" if self.role != AgentRole::Agency => {
                    return Some("only agency may submit a task contract".into());
                }
                "explore_codebase" if !matches!(self.role, AgentRole::Agency | AgentRole::Lead) => {
                    return Some(
                        "only agency and lead may use the shared exploration capability".into(),
                    );
                }
                "lead_mode" if self.role != AgentRole::Lead => {
                    return Some("only lead may change the lead mode".into());
                }
                "submit_review" if self.role != AgentRole::Lead => {
                    return Some("only lead may submit a review report".into());
                }
                "verify"
                    if !matches!(self.role, AgentRole::Lead)
                        && !(self.role == AgentRole::Coder && self.coder_scope.allow_shell) =>
                {
                    return Some("only lead may run verification".into());
                }
                _ => {}
            }
        }
        match self.role {
            AgentRole::Lead => None,
            AgentRole::Agency => {
                let denied = is_write_tool(tool)
                    || is_bash(tool)
                    || tool.is_mcp()
                    || matches!(tool, ToolKey::Native(name) if name == "task");
                denied.then(|| {
                    "agency is read-only and cannot write, run commands, dispatch coder, or call MCP"
                        .to_string()
                })
            }
            AgentRole::Coder => {
                if matches!(tool, ToolKey::Native(name) if name == "task") {
                    return Some("coder cannot dispatch agents".into());
                }
                if (is_bash(tool) || is_verify(tool)) && !self.coder_scope.allow_shell {
                    return Some(
                        "coder shell and command execution are disabled by default; Lead must run verification"
                            .into(),
                    );
                }
                if !is_write_tool(tool) {
                    return None;
                }
                for scope in scopes {
                    let path = canonicalize_lenient(Path::new(&scope.value));
                    for forbidden in &self.coder_scope.forbidden_paths {
                        let forbidden = canonicalize_lenient(forbidden);
                        if path == forbidden || path.starts_with(&forbidden) {
                            return Some(format!(
                                "path {} is outside the coder's allowed scope",
                                path.display()
                            ));
                        }
                    }
                    if !self.coder_scope.allowed_paths.is_empty() {
                        let allowed = self.coder_scope.allowed_paths.iter().any(|allowed| {
                            let allowed = canonicalize_lenient(allowed);
                            path == allowed || path.starts_with(&allowed)
                        });
                        if !allowed {
                            return Some(format!(
                                "path {} is outside the coder's allowed paths",
                                path.display()
                            ));
                        }
                    }
                }
                None
            }
        }
    }

    fn builtin_check(&self, tool: &ToolKey, scopes: &[Scope]) -> Option<PermissionCheck> {
        let ToolKey::Native(name) = tool else {
            return None;
        };
        if READ_TOOLS.contains(&name.as_str()) {
            let inside = scopes
                .iter()
                .all(|scope| is_within(&self.cwd, Path::new(&scope.value)));
            return Some(if inside {
                PermissionCheck::Allowed
            } else {
                PermissionCheck::NeedsPrompt {
                    tool: tool.clone(),
                    scopes: scopes.to_vec(),
                    reason: Some("path is outside the workspace".into()),
                    requires_exact: false,
                }
            });
        }
        if WRITE_TOOLS.contains(&name.as_str()) {
            let inside = scopes
                .iter()
                .all(|scope| is_within(&self.cwd, Path::new(&scope.value)));
            return Some(if inside {
                PermissionCheck::Allowed
            } else {
                PermissionCheck::NeedsPrompt {
                    tool: tool.clone(),
                    scopes: scopes.to_vec(),
                    reason: Some("write is outside the workspace".into()),
                    requires_exact: false,
                }
            });
        }
        if matches!(
            name.as_str(),
            "skill" | "lead_mode" | "submit_contract" | "submit_review"
        ) {
            return Some(PermissionCheck::Allowed);
        }
        if matches!(name.as_str(), "todo_write" | "memory" | "task") {
            return Some(PermissionCheck::Allowed);
        }
        None
    }

    pub fn apply_decision(
        &self,
        tool: &ToolKey,
        scopes: &[Scope],
        answer: &PermissionAnswer,
    ) -> Result<(), String> {
        let persisted = matches!(
            answer,
            PermissionAnswer::AllowSession
                | PermissionAnswer::AllowAlwaysProject
                | PermissionAnswer::AllowAlwaysGlobal
                | PermissionAnswer::DenyAlwaysProject
                | PermissionAnswer::DenyAlwaysGlobal
        );
        if !persisted {
            return Ok(());
        }
        let effect = if answer.is_allow() {
            Effect::Allow
        } else {
            Effect::Deny
        };
        let mut rules = Vec::new();
        let rule_key = rule_key_for(tool);
        for scope in scopes {
            let value = generalize(&rule_key, scope);
            rules.push(PermissionRule {
                tool: rule_key.clone(),
                scope: Some(value),
                effect,
            });
        }
        {
            let mut session = self
                .session_rules
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for rule in &rules {
                if !session.iter().any(|r| {
                    r.tool == rule.tool && r.scope == rule.scope && r.effect == rule.effect
                }) {
                    session.push(rule.clone());
                }
            }
        }
        match answer {
            PermissionAnswer::AllowAlwaysProject | PermissionAnswer::DenyAlwaysProject => {
                if !self.trusted {
                    return Err(
                        "project is not trusted; the rule only applies to this session".into(),
                    );
                }
                persist_rules(&self.project_file, &rules, false)
            }
            PermissionAnswer::AllowAlwaysGlobal | PermissionAnswer::DenyAlwaysGlobal => {
                persist_rules(&self.global_file, &rules, true)
            }
            _ => Ok(()),
        }
    }
}

fn is_write_tool(tool: &ToolKey) -> bool {
    matches!(tool, ToolKey::Native(name) if WRITE_TOOLS.contains(&name.as_str()))
}

fn is_bash(tool: &ToolKey) -> bool {
    matches!(tool, ToolKey::Native(name) if name == "bash")
}

fn is_verify(tool: &ToolKey) -> bool {
    matches!(tool, ToolKey::Native(name) if name == "verify")
}

fn is_task(tool: &ToolKey) -> bool {
    matches!(tool, ToolKey::Native(name) if name == "task")
}

fn is_memory(tool: &ToolKey) -> bool {
    matches!(tool, ToolKey::Native(name) if name == "memory")
}

fn rule_key_for(tool: &ToolKey) -> ToolKey {
    if is_verify(tool) {
        ToolKey::native("bash")
    } else {
        tool.clone()
    }
}

fn reaches(rule: &ToolKey, actual: &ToolKey) -> bool {
    match (rule, actual) {
        (ToolKey::Wildcard, _) => true,
        (ToolKey::Native(a), ToolKey::Native(b)) => a == b,
        (ToolKey::McpServer(a), ToolKey::McpTool(b, _)) => a == b,
        (ToolKey::McpTool(a, x), ToolKey::McpTool(b, y)) => a == b && x == y,
        _ => false,
    }
}

fn rule_matches(rule: &PermissionRule, scope: &Scope) -> bool {
    match &rule.scope {
        None => true,
        Some(pattern) => {
            if scope.exact && rule.effect == Effect::Allow {
                pattern == &scope.value
            } else {
                scope_matches(pattern, &scope.value)
            }
        }
    }
}

fn generalize(tool: &ToolKey, scope: &Scope) -> String {
    if scope.exact {
        return scope.value.clone();
    }
    match tool {
        ToolKey::Native(name) if name == "bash" => bash::generalize_command(&scope.value),
        ToolKey::Native(name) if WRITE_TOOLS.contains(&name.as_str()) => {
            let path = canonicalize_lenient(Path::new(&scope.value));
            let dir = path.parent().unwrap_or(&path);
            format!("{}/**", dir.display())
        }
        ToolKey::McpTool(..) | ToolKey::McpServer(..) => "*".into(),
        _ => scope.value.clone(),
    }
}

fn persist_rules(path: &Path, rules: &[PermissionRule], nested: bool) -> Result<(), String> {
    let mut root = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.parse::<toml::Value>().ok())
        .unwrap_or_else(|| toml::Value::Table(toml::Table::new()));
    let table = if nested {
        let root_table = root
            .as_table_mut()
            .ok_or_else(|| "permissions file is not a TOML table".to_string())?;
        let permissions = root_table
            .entry("permissions")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        permissions
            .as_table_mut()
            .ok_or_else(|| "[permissions] is not a table".to_string())?
    } else {
        root.as_table_mut()
            .ok_or_else(|| "permissions file is not a TOML table".to_string())?
    };
    let rules_value = table
        .entry("rules")
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let array = rules_value
        .as_array_mut()
        .ok_or_else(|| "rules is not an array".to_string())?;
    for rule in rules {
        let mut entry = toml::Table::new();
        entry.insert("tool".into(), toml::Value::String(rule.tool.to_string()));
        if let Some(scope) = &rule.scope {
            entry.insert("scope".into(), toml::Value::String(scope.clone()));
        }
        entry.insert(
            "effect".into(),
            toml::Value::String(
                match rule.effect {
                    Effect::Allow => "allow",
                    Effect::Deny => "deny",
                }
                .into(),
            ),
        );
        array.push(toml::Value::Table(entry));
    }
    let text = toml::to_string_pretty(&root).map_err(|e| e.to_string())?;
    atomic_write(path, text.as_bytes()).map_err(|e| e.to_string())
}

pub fn unique_scopes(scopes: Vec<Scope>) -> Vec<Scope> {
    let mut seen = HashSet::new();
    scopes
        .into_iter()
        .filter(|scope| seen.insert((scope.value.clone(), scope.exact)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kudelma_config::{PermissionRule, PermissionsConfig};

    fn manager(default: DefaultEffect, rules: Vec<PermissionRule>) -> PermissionManager {
        PermissionManager::from_rules(
            Path::new("/work"),
            PermissionsConfig {
                default,
                rules,
                yolo: false,
            },
        )
    }

    #[test]
    fn deny_beats_every_allow_and_yolo() {
        let mut manager = manager(
            DefaultEffect::Allow,
            vec![PermissionRule {
                tool: ToolKey::native("bash"),
                scope: Some("rm *".into()),
                effect: Effect::Deny,
            }],
        );
        manager.set_yolo(true);
        let check = manager.check(&ToolKey::native("bash"), &[Scope::command("rm -rf /tmp/x")]);
        assert!(matches!(check, PermissionCheck::Denied(_)));
    }

    #[test]
    fn reads_inside_cwd_are_allowed() {
        let manager = manager(DefaultEffect::Prompt, vec![]);
        let check = manager.check(
            &ToolKey::native("read"),
            &[Scope::path("/work/src/main.rs")],
        );
        assert_eq!(check, PermissionCheck::Allowed);
    }

    #[test]
    fn reads_outside_cwd_prompt() {
        let manager = manager(DefaultEffect::Allow, vec![]);
        let check = manager.check(&ToolKey::native("read"), &[Scope::path("/etc/passwd")]);
        assert!(matches!(check, PermissionCheck::NeedsPrompt { .. }));
    }

    #[test]
    fn plan_mode_allows_only_the_plan_file() {
        let manager = manager(DefaultEffect::Allow, vec![]).for_mode(PermissionMode::Plan);
        manager.set_plan_path(Some(PathBuf::from("/work/plan.md")));
        let allowed = manager.check(&ToolKey::native("write"), &[Scope::path("/work/plan.md")]);
        assert_eq!(allowed, PermissionCheck::Allowed);
        let denied = manager.check(
            &ToolKey::native("write"),
            &[Scope::path("/work/src/main.rs")],
        );
        assert!(matches!(denied, PermissionCheck::Denied(_)));
        assert!(matches!(
            manager.check(&ToolKey::native("bash"), &[Scope::command("cargo test")]),
            PermissionCheck::Denied(_)
        ));
    }

    #[test]
    fn review_is_read_only_but_allows_verification() {
        let manager = manager(DefaultEffect::Allow, vec![]).for_mode(PermissionMode::Review);
        assert!(matches!(
            manager.check(
                &ToolKey::native("edit"),
                &[Scope::path("/work/src/main.rs")]
            ),
            PermissionCheck::Denied(_)
        ));
        assert!(matches!(
            manager.check(&ToolKey::native("bash"), &[Scope::command("cargo test")]),
            PermissionCheck::Denied(_)
        ));
        assert!(matches!(
            manager.check(&ToolKey::native("task"), &[Scope::exact("task:x")]),
            PermissionCheck::Denied(_)
        ));
        assert_eq!(
            manager.check(
                &ToolKey::native("read"),
                &[Scope::path("/work/src/main.rs")]
            ),
            PermissionCheck::Allowed
        );
        assert_eq!(
            manager.check(&ToolKey::native("verify"), &[Scope::command("cargo test")]),
            PermissionCheck::Allowed
        );
        assert_eq!(
            manager.check(&ToolKey::native("submit_review"), &[Scope::exact("review")]),
            PermissionCheck::Allowed
        );
        let build = manager.for_mode(PermissionMode::Build);
        assert!(matches!(
            build.check(&ToolKey::native("submit_review"), &[Scope::exact("review")]),
            PermissionCheck::Denied(_)
        ));
    }

    #[test]
    fn review_mode_still_honors_bash_deny_for_verify() {
        let manager = manager(
            DefaultEffect::Allow,
            vec![PermissionRule {
                tool: ToolKey::native("bash"),
                scope: Some("cargo test".into()),
                effect: Effect::Deny,
            }],
        )
        .for_mode(PermissionMode::Review);
        assert!(matches!(
            manager.check(&ToolKey::native("verify"), &[Scope::command("cargo test")]),
            PermissionCheck::Denied(_)
        ));
    }

    #[test]
    fn exact_scopes_require_an_exact_allow_rule() {
        let manager = manager(
            DefaultEffect::Prompt,
            vec![PermissionRule {
                tool: ToolKey::native("bash"),
                scope: Some("echo *".into()),
                effect: Effect::Allow,
            }],
        );
        let check = manager.check(
            &ToolKey::native("bash"),
            &[Scope::exact("echo hi > /etc/passwd")],
        );
        assert!(matches!(check, PermissionCheck::NeedsPrompt { .. }));
    }

    #[test]
    fn yolo_does_not_bypass_exact_shell_approval() {
        let mut manager = manager(DefaultEffect::Prompt, vec![]);
        manager.set_yolo(true);
        let check = manager.check_with_force(
            &ToolKey::native("bash"),
            &[Scope::exact("echo hi > out.txt")],
            true,
        );
        assert!(matches!(
            check,
            PermissionCheck::NeedsPrompt {
                requires_exact: true,
                ..
            }
        ));
    }

    #[test]
    fn exact_allow_rule_permits_the_full_command() {
        let manager = manager(
            DefaultEffect::Prompt,
            vec![PermissionRule {
                tool: ToolKey::native("bash"),
                scope: Some("echo hi > out.txt".into()),
                effect: Effect::Allow,
            }],
        );
        assert_eq!(
            manager.check(
                &ToolKey::native("bash"),
                &[Scope::exact("echo hi > out.txt")]
            ),
            PermissionCheck::Allowed
        );
    }

    #[test]
    fn allowed_tools_bash_does_not_bypass_exact_approval() {
        let manager = manager(DefaultEffect::Prompt, vec![]);
        manager.allow_tool_for_session("bash");
        let check = manager.check_with_force(
            &ToolKey::native("bash"),
            &[Scope::exact("echo hi > out.txt")],
            true,
        );
        assert!(matches!(
            check,
            PermissionCheck::NeedsPrompt {
                requires_exact: true,
                ..
            }
        ));
    }

    #[test]
    fn bash_cwd_outside_the_workspace_requires_exact_approval() {
        let manager = manager(DefaultEffect::Allow, vec![]);
        assert!(matches!(
            manager.check_with_context(
                &ToolKey::native("bash"),
                &[Scope::command("ls")],
                false,
                Some(Path::new("/etc"))
            ),
            PermissionCheck::NeedsPrompt {
                requires_exact: true,
                ..
            }
        ));
    }

    #[test]
    fn agency_is_read_only_even_under_yolo() {
        let mut manager = manager(DefaultEffect::Allow, vec![]);
        manager.set_yolo(true);
        let manager = manager.for_role(AgentRole::Agency, CoderScope::default());
        assert!(matches!(
            manager.check(&ToolKey::native("write"), &[Scope::path("/work/a.txt")]),
            PermissionCheck::Denied(_)
        ));
        assert!(matches!(
            manager.check(&ToolKey::native("bash"), &[Scope::command("true")]),
            PermissionCheck::Denied(_)
        ));
        assert!(matches!(
            manager.check(&ToolKey::native("task"), &[Scope::exact("task:x")]),
            PermissionCheck::Denied(_)
        ));
    }

    #[test]
    fn only_agency_and_lead_may_use_shared_exploration() {
        let manager = manager(DefaultEffect::Allow, vec![]);
        for role in [AgentRole::Agency, AgentRole::Lead] {
            let manager = manager.for_role(role, CoderScope::default());
            assert_eq!(
                manager.check(
                    &ToolKey::native("explore_codebase"),
                    &[Scope::path("/work/src/main.rs")]
                ),
                PermissionCheck::Allowed
            );
        }
        let coder = manager.for_role(AgentRole::Coder, CoderScope::default());
        assert!(matches!(
            coder.check(
                &ToolKey::native("explore_codebase"),
                &[Scope::path("/work/src/main.rs")]
            ),
            PermissionCheck::Denied(_)
        ));
    }

    #[test]
    fn exploration_outside_the_workspace_prompts() {
        let manager = manager(DefaultEffect::Allow, vec![]);
        for role in [AgentRole::Agency, AgentRole::Lead] {
            let manager = manager.for_role(role, CoderScope::default());
            assert!(matches!(
                manager.check(&ToolKey::native("explore_codebase"), &[Scope::path("/etc")]),
                PermissionCheck::NeedsPrompt { .. }
            ));
        }
    }

    #[test]
    fn coder_scope_is_enforced_before_yolo() {
        let manager = manager(DefaultEffect::Allow, vec![]).for_role(
            AgentRole::Coder,
            CoderScope {
                allowed_paths: vec![PathBuf::from("/work/src")],
                forbidden_paths: vec![PathBuf::from("/work/src/secret")],
                allow_shell: false,
            },
        );
        assert_eq!(
            manager.check(
                &ToolKey::native("write"),
                &[Scope::path("/work/src/main.rs")]
            ),
            PermissionCheck::Allowed
        );
        assert!(matches!(
            manager.check(&ToolKey::native("write"), &[Scope::path("/work/other.rs")]),
            PermissionCheck::Denied(_)
        ));
        assert!(matches!(
            manager.check(
                &ToolKey::native("write"),
                &[Scope::path("/work/src/secret/key.rs")]
            ),
            PermissionCheck::Denied(_)
        ));
        assert!(matches!(
            manager.check(&ToolKey::native("task"), &[Scope::exact("task:x")]),
            PermissionCheck::Denied(_)
        ));
    }

    #[test]
    fn coder_shell_and_verify_are_denied_even_under_yolo() {
        let mut base = manager(DefaultEffect::Allow, vec![]);
        base.set_yolo(true);
        let coder = base.for_role(AgentRole::Coder, CoderScope::default());
        assert!(matches!(
            coder.check(&ToolKey::native("bash"), &[Scope::command("cargo test")]),
            PermissionCheck::Denied(_)
        ));
        assert!(matches!(
            coder.check(&ToolKey::native("verify"), &[Scope::command("cargo test")]),
            PermissionCheck::Denied(_)
        ));
    }

    #[test]
    fn coder_can_run_shell_only_with_the_explicit_scope_flag() {
        let coder = manager(DefaultEffect::Allow, vec![]).for_role(
            AgentRole::Coder,
            CoderScope {
                allow_shell: true,
                ..CoderScope::default()
            },
        );
        assert_eq!(
            coder.check(&ToolKey::native("bash"), &[Scope::command("cargo test")]),
            PermissionCheck::Allowed
        );
    }
}
