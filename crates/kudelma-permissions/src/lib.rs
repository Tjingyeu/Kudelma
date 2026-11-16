#![allow(clippy::collapsible_if)]

//! Fail-closed permission checks and shell command analysis.

pub mod bash;
pub mod scope;

pub use bash::{BashAnalysis, analyze_bash};
pub use scope::{Scope, is_universal, scope_matches};

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use kudelma_config::{Config, DefaultEffect, Effect, PermissionRule, PermissionsConfig, ToolKey};
use kudelma_storage::{atomic_write, canonicalize_lenient, is_within};

pub const WRITE_TOOLS: &[&str] = &["write", "edit", "multiedit", "edit_lines", "insert_lines"];
pub const READ_TOOLS: &[&str] = &["read", "glob", "grep", "index", "ls"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionCheck {
    Allowed,
    Denied(String),
    NeedsPrompt {
        tool: ToolKey,
        scopes: Vec<Scope>,
        reason: Option<String>,
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
        }
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
        if scopes.is_empty() {
            return self.check_scopes(tool, &[Scope::exact("")], force_prompt);
        }
        self.check_scopes(tool, scopes, force_prompt)
    }

    fn check_scopes(
        &self,
        tool: &ToolKey,
        scopes: &[Scope],
        force_prompt: bool,
    ) -> PermissionCheck {
        let session = self.session_rules.read().unwrap_or_else(|e| e.into_inner());
        let all_rules = || session.iter().chain(self.rules.iter());

        for scope in scopes {
            for rule in all_rules() {
                if rule.effect == Effect::Deny
                    && reaches(&rule.tool, tool)
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

        let plan = self.plan_path();
        if let Some(plan_path) = &plan {
            if is_write_tool(tool) {
                let normalized_plan = canonicalize_lenient(plan_path);
                let all_plan = scopes
                    .iter()
                    .all(|scope| canonicalize_lenient(Path::new(&scope.value)) == normalized_plan);
                if all_plan {
                    return PermissionCheck::Allowed;
                }
                return PermissionCheck::NeedsPrompt {
                    tool: tool.clone(),
                    scopes: scopes.to_vec(),
                    reason: Some("plan mode only allows writing the plan file".into()),
                };
            }
            if is_bash(tool) || tool.is_mcp() {
                return PermissionCheck::NeedsPrompt {
                    tool: tool.clone(),
                    scopes: scopes.to_vec(),
                    reason: Some("plan mode asks before running commands or MCP tools".into()),
                };
            }
        }

        if self.yolo {
            return PermissionCheck::Allowed;
        }

        let mut pending = Vec::new();
        for scope in scopes {
            let allowed = all_rules().any(|rule| {
                rule.effect == Effect::Allow
                    && reaches(&rule.tool, tool)
                    && rule_matches(rule, scope)
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
                reason: Some("command uses shell metacharacters; approve it exactly".into()),
            };
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
            },
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
                }
            });
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
        for scope in scopes {
            let value = generalize(tool, scope);
            rules.push(PermissionRule {
                tool: tool.clone(),
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
        let manager = manager(DefaultEffect::Allow, vec![]);
        manager.set_plan_path(Some(PathBuf::from("/work/plan.md")));
        let allowed = manager.check(&ToolKey::native("write"), &[Scope::path("/work/plan.md")]);
        assert_eq!(allowed, PermissionCheck::Allowed);
        let denied = manager.check(
            &ToolKey::native("write"),
            &[Scope::path("/work/src/main.rs")],
        );
        assert!(matches!(denied, PermissionCheck::NeedsPrompt { .. }));
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
    fn yolo_still_allows_redirection_commands() {
        let mut manager = manager(DefaultEffect::Prompt, vec![]);
        manager.set_yolo(true);
        assert_eq!(
            manager.check(
                &ToolKey::native("bash"),
                &[Scope::exact("echo hi > out.txt")]
            ),
            PermissionCheck::Allowed
        );
    }
}
