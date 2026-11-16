use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SkillError {
    #[error("invalid SKILL.md frontmatter: {0}")]
    Frontmatter(String),
    #[error("skill name is missing")]
    MissingName,
    #[error("invalid skill name {0:?}")]
    InvalidName(String),
    #[error("could not read skill {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    Builtin,
    User,
    Project,
    ProjectUntrusted,
}

impl SkillSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::User => "user",
            Self::Project => "project",
            Self::ProjectUntrusted => "project (untrusted)",
        }
    }

    pub fn trusted(self) -> bool {
        self != Self::ProjectUntrusted
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SkillPriority {
    RequiredWhenApplicable,
    #[default]
    Optional,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    pub agents: Vec<String>,
    pub modes: Vec<String>,
    pub priority: SkillPriority,
    pub triggers: Vec<String>,
    pub requires: Vec<String>,
    pub produces: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Skill {
    pub metadata: SkillMetadata,
    pub body: String,
    pub source: SkillSource,
    pub path: Option<PathBuf>,
    pub content_hash: String,
}

impl Skill {
    pub fn applies_to(&self, agent: &str, mode: &str) -> bool {
        (self.metadata.agents.is_empty()
            || self
                .metadata
                .agents
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(agent)))
            && (self.metadata.modes.is_empty()
                || self
                    .metadata
                    .modes
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(mode)))
    }
}

pub fn content_hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

pub fn parse_skill(
    text: &str,
    source: SkillSource,
    path: Option<PathBuf>,
    fallback_name: &str,
) -> Result<Skill, SkillError> {
    let (frontmatter, body) = split_frontmatter(text);
    let mut metadata = SkillMetadata::default();
    if let Some(frontmatter) = frontmatter {
        parse_frontmatter(frontmatter, &mut metadata)?;
    }
    if metadata.name.is_empty() {
        metadata.name = fallback_name.to_string();
    }
    validate_name(&metadata.name)?;
    if metadata.description.is_empty() {
        metadata.description = body
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("No description")
            .trim()
            .to_string();
    }
    Ok(Skill {
        content_hash: content_hash(text),
        metadata,
        body: body.trim().to_string(),
        source,
        path,
    })
}

fn split_frontmatter(text: &str) -> (Option<&str>, &str) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text.strip_prefix("---") else {
        return (None, text);
    };
    let rest = rest
        .strip_prefix('\n')
        .unwrap_or_else(|| rest.strip_prefix("\r\n").unwrap_or(rest));
    if let Some(end) = rest.find("\n---") {
        let frontmatter = &rest[..end];
        let after = &rest[end + 4..];
        let body = after
            .strip_prefix('\n')
            .or_else(|| after.strip_prefix("\r\n"))
            .unwrap_or(after);
        return (Some(frontmatter), body);
    }
    (None, text)
}

fn parse_frontmatter(frontmatter: &str, metadata: &mut SkillMetadata) -> Result<(), SkillError> {
    let mut section = String::new();
    for raw in frontmatter.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim();
        if indent == 0 {
            section.clear();
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            if value.is_empty() {
                section = key.to_string();
                continue;
            }
            match key {
                "name" => metadata.name = unquote(value),
                "description" => metadata.description = unquote(value),
                "priority" => {
                    metadata.priority = match unquote(value).as_str() {
                        "required_when_applicable" => SkillPriority::RequiredWhenApplicable,
                        _ => SkillPriority::Optional,
                    }
                }
                "agents" | "modes" | "triggers" | "requires" | "produces" => {
                    let values = parse_list(value);
                    match key {
                        "agents" => metadata.agents.extend(values),
                        "modes" => metadata.modes.extend(values),
                        "triggers" => metadata.triggers.extend(values),
                        "requires" => metadata.requires.extend(values),
                        "produces" => metadata.produces.extend(values),
                        _ => {}
                    }
                }
                _ => {}
            }
            if key == "applies_to" {
                section = "applies_to".into();
            }
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("- ") {
            let value = unquote(value.trim());
            match section.as_str() {
                "applies_to.agents" | "agents" => metadata.agents.push(value),
                "applies_to.modes" | "modes" => metadata.modes.push(value),
                "triggers" => metadata.triggers.push(value),
                "requires" => metadata.requires.push(value),
                "produces" => metadata.produces.push(value),
                _ => {}
            }
            continue;
        }
        if section == "applies_to" {
            if let Some((key, value)) = trimmed.split_once(':') {
                let key = key.trim();
                let values = parse_list(value);
                match key {
                    "agents" => metadata.agents.extend(values),
                    "modes" => metadata.modes.extend(values),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn parse_list(value: &str) -> Vec<String> {
    let value = value.trim();
    let inner = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(value);
    inner
        .split(',')
        .map(|item| unquote(item.trim()))
        .filter(|item| !item.is_empty())
        .collect()
}

fn unquote(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}

fn validate_name(name: &str) -> Result<(), SkillError> {
    if name.is_empty() {
        return Err(SkillError::MissingName);
    }
    if name.len() > 80
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(SkillError::InvalidName(name.into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKILL: &str = r#"---
name: test-driven-development
description: Use for behavior changes.
applies_to:
  agents: [lead, coder]
  modes: [build, debug]
priority: required_when_applicable
triggers:
  - behavior_change
---

# TDD

RED, GREEN, REFACTOR.
"#;

    #[test]
    fn parses_frontmatter_and_body() {
        let skill = parse_skill(SKILL, SkillSource::Builtin, None, "fallback").unwrap();
        assert_eq!(skill.metadata.name, "test-driven-development");
        assert_eq!(skill.metadata.agents, vec!["lead", "coder"]);
        assert_eq!(skill.metadata.modes, vec!["build", "debug"]);
        assert_eq!(
            skill.metadata.priority,
            SkillPriority::RequiredWhenApplicable
        );
        assert!(skill.body.contains("RED, GREEN"));
        assert!(skill.applies_to("lead", "build"));
        assert!(!skill.applies_to("agency", "build"));
    }

    #[test]
    fn rejects_unsafe_names() {
        let text = "---\nname: ../escape\ndescription: x\n---\nbody";
        assert!(parse_skill(text, SkillSource::Project, None, "x").is_err());
    }
}
