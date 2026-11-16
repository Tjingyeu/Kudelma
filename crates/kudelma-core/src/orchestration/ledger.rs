use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoleUsage {
    pub turns: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillUsage {
    pub name: String,
    pub content_hash: String,
    pub loaded_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEvent {
    pub at: String,
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionLedger {
    pub task_id: String,
    pub roles: BTreeMap<String, RoleUsage>,
    pub skills_loaded: Vec<SkillUsage>,
    pub workers_started: u32,
    pub workers_succeeded: u32,
    pub rework_count: u32,
    pub developer_interventions: u32,
    pub accepted: bool,
    pub events: Vec<LedgerEvent>,
}

impl ExecutionLedger {
    pub fn new(task_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            ..Default::default()
        }
    }

    pub fn record_turn(&mut self, role: &str, input_tokens: u64, output_tokens: u64, cost: f64) {
        let usage = self.roles.entry(role.to_string()).or_default();
        usage.turns += 1;
        usage.input_tokens = usage.input_tokens.saturating_add(input_tokens);
        usage.output_tokens = usage.output_tokens.saturating_add(output_tokens);
        usage.cost_usd += cost;
    }

    pub fn record_skill(&mut self, name: &str, hash: &str) {
        if self
            .skills_loaded
            .iter()
            .any(|skill| skill.name == name && skill.content_hash == hash)
        {
            return;
        }
        self.skills_loaded.push(SkillUsage {
            name: name.into(),
            content_hash: hash.into(),
            loaded_at: kudelma_storage::now_rfc3339(),
        });
    }

    pub fn event(&mut self, kind: impl Into<String>, detail: impl Into<String>) {
        self.events.push(LedgerEvent {
            at: kudelma_storage::now_rfc3339(),
            kind: kind.into(),
            detail: detail.into(),
        });
    }

    pub fn total_cost(&self) -> f64 {
        self.roles.values().map(|usage| usage.cost_usd).sum()
    }

    pub fn total_tokens(&self) -> u64 {
        self.roles
            .values()
            .map(|usage| usage.input_tokens + usage.output_tokens)
            .sum()
    }
}
