use serde::{Deserialize, Serialize};

use super::contract::TaskContract;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactKind {
    ConfirmedFact,
    ObservedEvidence,
    Assumption,
    Unknown,
}

impl FactKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::ConfirmedFact => "confirmed fact",
            Self::ObservedEvidence => "observed evidence",
            Self::Assumption => "assumption",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextItem {
    pub kind: FactKind,
    pub statement: String,
    pub source: Option<String>,
}

impl ContextItem {
    pub fn fact(statement: impl Into<String>) -> Self {
        Self {
            kind: FactKind::ConfirmedFact,
            statement: statement.into(),
            source: None,
        }
    }

    pub fn evidence(statement: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            kind: FactKind::ObservedEvidence,
            statement: statement.into(),
            source: Some(source.into()),
        }
    }

    pub fn assumption(statement: impl Into<String>) -> Self {
        Self {
            kind: FactKind::Assumption,
            statement: statement.into(),
            source: None,
        }
    }

    pub fn unknown(statement: impl Into<String>) -> Self {
        Self {
            kind: FactKind::Unknown,
            statement: statement.into(),
            source: None,
        }
    }
}

/// The minimum task-relevant information handed to a role. It never contains
/// another agent's full conversation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextPack {
    pub caller: String,
    pub purpose: String,
    pub contract: Option<TaskContract>,
    pub items: Vec<ContextItem>,
    pub allowed_paths: Vec<String>,
    pub forbidden_paths: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub output_schema: Option<String>,
}

impl ContextPack {
    pub fn new(caller: impl Into<String>, purpose: impl Into<String>) -> Self {
        Self {
            caller: caller.into(),
            purpose: purpose.into(),
            ..Default::default()
        }
    }

    pub fn with_contract(mut self, contract: TaskContract) -> Self {
        self.acceptance_criteria = contract.acceptance_criteria.clone();
        self.contract = Some(contract);
        self
    }

    pub fn push(&mut self, item: ContextItem) {
        self.items.push(item);
    }

    pub fn render(&self) -> String {
        let mut out = format!("Caller: {}\nPurpose: {}\n", self.caller, self.purpose);
        if let Some(contract) = &self.contract {
            out.push_str("\n## Task Contract\n");
            out.push_str(&contract.render());
        }
        if !self.items.is_empty() {
            out.push_str("\n## Context\n");
            for item in &self.items {
                match &item.source {
                    Some(source) => out.push_str(&format!(
                        "- [{}] {} ({source})\n",
                        item.kind.label(),
                        item.statement
                    )),
                    None => {
                        out.push_str(&format!("- [{}] {}\n", item.kind.label(), item.statement))
                    }
                }
            }
        }
        if !self.allowed_paths.is_empty() {
            out.push_str(&format!(
                "\nAllowed paths: {}\n",
                self.allowed_paths.join(", ")
            ));
        }
        if !self.forbidden_paths.is_empty() {
            out.push_str(&format!(
                "Forbidden paths: {}\n",
                self.forbidden_paths.join(", ")
            ));
        }
        if !self.allowed_tools.is_empty() {
            out.push_str(&format!(
                "Allowed tools: {}\n",
                self.allowed_tools.join(", ")
            ));
        }
        if !self.acceptance_criteria.is_empty() {
            out.push_str("\nAcceptance criteria:\n");
            for criterion in &self.acceptance_criteria {
                out.push_str(&format!("- {criterion}\n"));
            }
        }
        if let Some(schema) = &self.output_schema {
            out.push_str(&format!("\nExpected output schema:\n{schema}\n"));
        }
        out
    }
}
