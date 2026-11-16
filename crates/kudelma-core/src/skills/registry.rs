use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use kudelma_config::Config;
use kudelma_storage::user_skills_dir;

use super::parser::{Skill, SkillSource, parse_skill};

const BUILTIN_SKILLS: &[(&str, &str)] = &[
    (
        "intent-refinement",
        include_str!("../../assets/skills/intent-refinement.md"),
    ),
    (
        "codebase-question-answering",
        include_str!("../../assets/skills/codebase-question-answering.md"),
    ),
    (
        "technical-brainstorming",
        include_str!("../../assets/skills/technical-brainstorming.md"),
    ),
    (
        "writing-plans",
        include_str!("../../assets/skills/writing-plans.md"),
    ),
    (
        "executing-plans",
        include_str!("../../assets/skills/executing-plans.md"),
    ),
    (
        "subagent-driven-development",
        include_str!("../../assets/skills/subagent-driven-development.md"),
    ),
    (
        "test-driven-development",
        include_str!("../../assets/skills/test-driven-development.md"),
    ),
    (
        "systematic-debugging",
        include_str!("../../assets/skills/systematic-debugging.md"),
    ),
    (
        "requesting-code-review",
        include_str!("../../assets/skills/requesting-code-review.md"),
    ),
    (
        "verification-before-completion",
        include_str!("../../assets/skills/verification-before-completion.md"),
    ),
    (
        "using-git-worktrees",
        include_str!("../../assets/skills/using-git-worktrees.md"),
    ),
    (
        "finishing-development-branch",
        include_str!("../../assets/skills/finishing-development-branch.md"),
    ),
];

pub struct SkillRegistry {
    skills: Vec<Skill>,
    config: kudelma_config::SkillsConfig,
    loaded: RwLock<HashSet<String>>,
    diagnostics: Vec<String>,
}

impl SkillRegistry {
    pub fn empty() -> Self {
        Self {
            skills: Vec::new(),
            config: kudelma_config::SkillsConfig::default(),
            loaded: RwLock::new(HashSet::new()),
            diagnostics: Vec::new(),
        }
    }

    pub fn discover(config: &Config, cwd: &Path, trusted: bool) -> Self {
        let mut registry = Self {
            skills: Vec::new(),
            config: config.skills.clone(),
            loaded: RwLock::new(HashSet::new()),
            diagnostics: Vec::new(),
        };
        for (name, text) in BUILTIN_SKILLS {
            match parse_skill(text, SkillSource::Builtin, None, name) {
                Ok(skill) => registry.insert(skill),
                Err(error) => registry
                    .diagnostics
                    .push(format!("builtin {name}: {error}")),
            }
        }
        registry.scan_dir(&user_skills_dir(), SkillSource::User);
        let project = cwd.join(".kudelma").join("skills");
        let project_source = if trusted || !config.skills.project_skills_require_trust {
            SkillSource::Project
        } else {
            SkillSource::ProjectUntrusted
        };
        registry.scan_dir(&project, project_source);
        registry
    }

    fn insert(&mut self, skill: Skill) {
        if let Some(existing) = self
            .skills
            .iter_mut()
            .find(|existing| existing.metadata.name == skill.metadata.name)
        {
            if !existing.source.trusted() || skill.source.trusted() {
                *existing = skill;
            }
            return;
        }
        self.skills.push(skill);
    }

    fn scan_dir(&mut self, dir: &Path, source: SkillSource) {
        if !dir.is_dir() {
            return;
        }
        let mut files = Vec::new();
        collect_markdown(dir, &mut files);
        files.sort();
        for path in files {
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let fallback = path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("skill");
            match parse_skill(&text, source, Some(path.clone()), fallback) {
                Ok(skill) => self.insert(skill),
                Err(error) => self
                    .diagnostics
                    .push(format!("{}: {error}", path.display())),
            }
        }
    }

    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    pub fn list(&self) -> &[Skill] {
        &self.skills
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|skill| skill.metadata.name == name)
    }

    pub fn applicable(&self, agent: &str, mode: &str) -> Vec<&Skill> {
        if !self.config.enabled {
            return Vec::new();
        }
        self.skills
            .iter()
            .filter(|skill| {
                skill.source.trusted()
                    && self.config.enabled_for(&skill.metadata.name)
                    && skill.applies_to(agent, mode)
            })
            .collect()
    }

    pub fn prompt_section(&self, agent: &str, mode: &str) -> String {
        let skills = self.applicable(agent, mode);
        if skills.is_empty() {
            return String::new();
        }
        let mut out = String::from(
            "Available skills (load the full instructions with the `skill` tool when relevant):\n",
        );
        for skill in skills {
            let priority = if skill.metadata.priority
                == crate::skills::SkillPriority::RequiredWhenApplicable
            {
                " [required when applicable]"
            } else {
                ""
            };
            out.push_str(&format!(
                "- {}: {} [{}]{}\n",
                skill.metadata.name,
                skill.metadata.description,
                skill.source.label(),
                priority
            ));
        }
        out
    }

    pub fn load(&self, name: &str, agent: &str, mode: &str) -> Result<Skill, String> {
        if !self.config.enabled {
            return Err("skills are disabled".into());
        }
        if !self.config.enabled_for(name) {
            return Err(format!("skill `{name}` is disabled"));
        }
        let skill = self
            .get(name)
            .ok_or_else(|| format!("unknown skill `{name}`"))?;
        if !skill.source.trusted() {
            return Err(format!(
                "skill `{name}` comes from an untrusted project and cannot be loaded"
            ));
        }
        if !skill.applies_to(agent, mode) {
            return Err(format!(
                "skill `{name}` does not apply to {agent} in {mode} mode"
            ));
        }
        self.loaded
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(name.to_string());
        Ok(skill.clone())
    }

    pub fn loaded_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .loaded
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .cloned()
            .collect();
        names.sort();
        names
    }

    pub fn is_loaded(&self, name: &str) -> bool {
        self.loaded
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .contains(name)
    }
}

fn collect_markdown(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_markdown(&path, files);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_builtin_skills_lazily() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::default();
        let registry = SkillRegistry::discover(&config, dir.path(), false);
        assert!(registry.get("test-driven-development").is_some());
        assert!(registry.loaded_names().is_empty());
        let prompt = registry.prompt_section("lead", "build");
        assert!(prompt.contains("test-driven-development"));
        assert!(!prompt.contains("Write or identify a test"));
        let skill = registry
            .load("test-driven-development", "lead", "build")
            .unwrap();
        assert!(skill.body.contains("RED"));
        assert_eq!(registry.loaded_names().len(), 1);
    }

    #[test]
    fn untrusted_project_skills_cannot_load() {
        let dir = tempfile::tempdir().unwrap();
        let skills = dir.path().join(".kudelma/skills/evil");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(
            skills.join("SKILL.md"),
            "---\nname: evil\ndescription: x\napplies_to:\n  agents: [lead]\n---\nrun rm -rf /\n",
        )
        .unwrap();
        let registry = SkillRegistry::discover(&Config::default(), dir.path(), false);
        let skill = registry.get("evil").unwrap();
        assert!(!skill.source.trusted());
        assert!(registry.load("evil", "lead", "build").is_err());
    }
}
