use serde::{Deserialize, Serialize};

use super::request::{Caller, InvestigationMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    ObservedFact,
    Inference,
}

impl FindingKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::ObservedFact => "observed fact",
            Self::Inference => "inference",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplorationStatus {
    Completed,
    Partial,
    Blocked,
}

impl ExplorationStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceCite {
    pub file: String,
    pub symbol: Option<String>,
    pub lines: [usize; 2],
    pub note: Option<String>,
}

impl EvidenceCite {
    pub fn file(file: impl Into<String>, lines: [usize; 2]) -> Self {
        Self {
            file: file.into(),
            symbol: None,
            lines,
            note: None,
        }
    }

    pub fn symbol(mut self, symbol: impl Into<String>) -> Self {
        self.symbol = Some(symbol.into());
        self
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    pub fn render(&self) -> String {
        let mut out = self.file.clone();
        if let Some(symbol) = &self.symbol {
            out.push_str(&format!("::{symbol}"));
        }
        if self.lines[0] > 0 {
            if self.lines[0] == self.lines[1] {
                out.push_str(&format!(":{}", self.lines[0]));
            } else {
                out.push_str(&format!(":{}-{}", self.lines[0], self.lines[1]));
            }
        }
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub claim: String,
    pub kind: FindingKind,
    pub evidence: Vec<EvidenceCite>,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolRelationship {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CoverageReport {
    pub searched_files: Vec<String>,
    pub searched_symbols: Vec<String>,
    pub omitted_or_unavailable: Vec<String>,
    pub files_inspected: usize,
    pub result_limit_reached: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationReport {
    pub request_id: String,
    pub caller: Caller,
    pub investigation_mode: InvestigationMode,
    pub status: ExplorationStatus,
    pub summary: String,
    pub findings: Vec<Finding>,
    pub relationships: Vec<SymbolRelationship>,
    pub unknowns: Vec<String>,
    pub coverage: CoverageReport,
    pub external_sources: Vec<String>,
    pub revision: Option<String>,
    #[serde(default)]
    pub cached: bool,
    pub created_at: String,
}

impl ExplorationReport {
    pub fn render(&self) -> String {
        let mut out = format!(
            "Exploration report {} ({})\nmode: {} · caller: {} · status: {}\nSummary: {}\n",
            self.request_id,
            self.created_at,
            self.investigation_mode.name(),
            self.caller.name(),
            self.status.label(),
            self.summary
        );
        if self.cached {
            out.push_str("cached: yes (source revision unchanged)\n");
        }
        if !self.findings.is_empty() {
            out.push_str("\nFindings:\n");
            for finding in &self.findings {
                out.push_str(&format!(
                    "- [{} / {:?}] {}\n",
                    finding.kind.label(),
                    finding.confidence,
                    finding.claim
                ));
                for evidence in &finding.evidence {
                    out.push_str(&format!("  evidence: {}\n", evidence.render()));
                    if let Some(note) = &evidence.note {
                        out.push_str(&format!("    note: {note}\n"));
                    }
                }
            }
        }
        if !self.relationships.is_empty() {
            out.push_str("\nRelationships:\n");
            for relationship in &self.relationships {
                out.push_str(&format!(
                    "- {} -> {} ({}) at {}:{}\n",
                    relationship.from,
                    relationship.to,
                    relationship.kind,
                    relationship.file,
                    relationship.line
                ));
            }
        }
        if !self.unknowns.is_empty() {
            out.push_str("\nUnknowns:\n");
            for unknown in &self.unknowns {
                out.push_str(&format!("- {unknown}\n"));
            }
        }
        out.push_str(&format!(
            "\nCoverage: {} file(s) inspected; result limit reached: {}\n",
            self.coverage.files_inspected, self.coverage.result_limit_reached
        ));
        if !self.coverage.omitted_or_unavailable.is_empty() {
            out.push_str("Omitted or unavailable:\n");
            for entry in &self.coverage.omitted_or_unavailable {
                out.push_str(&format!("- {entry}\n"));
            }
        }
        if !self.external_sources.is_empty() {
            out.push_str("External sources:\n");
            for source in &self.external_sources {
                out.push_str(&format!("- {source}\n"));
            }
        }
        out
    }
}
