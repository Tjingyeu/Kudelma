use serde::{Deserialize, Serialize};

use super::revision::WorkspaceRevision;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Approved,
    ChangesRequested,
    Blocked,
}

impl ReviewStatus {
    pub fn name(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::ChangesRequested => "changes_requested",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionStatus {
    Met,
    Partial,
    Unmet,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CriterionResult {
    #[serde(default)]
    pub criterion: String,
    #[serde(default)]
    pub status: Option<CriterionStatus>,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewFinding {
    #[serde(default)]
    pub severity: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewReport {
    #[serde(default = "default_version")]
    pub report_version: u32,
    pub review_id: String,
    pub session_id: String,
    pub contract_id: String,
    pub contract_version: u32,
    pub revision: WorkspaceRevision,
    pub criteria: Vec<CriterionResult>,
    #[serde(default)]
    pub findings: Vec<ReviewFinding>,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    pub status: ReviewStatus,
    pub created_at: String,
}

fn default_version() -> u32 {
    1
}

impl ReviewReport {
    pub fn validate(&self) -> Result<(), String> {
        if self.contract_id.trim().is_empty() {
            return Err("review report has no contract id".into());
        }
        if self.criteria.is_empty() {
            return Err("review report has no acceptance-criterion results".into());
        }
        if self.status == ReviewStatus::Approved {
            if self.evidence_ids.is_empty() {
                return Err("an approved review must cite verification evidence".into());
            }
            if let Some(missing) = self
                .criteria
                .iter()
                .find(|criterion| criterion.status != Some(CriterionStatus::Met))
            {
                return Err(format!(
                    "criterion `{}` is not met; the review cannot be approved",
                    missing.criterion
                ));
            }
        }
        Ok(())
    }
    pub fn render(&self) -> String {
        format!(
            "Review {} ({})\ncontract {} v{}\nrevision {}\ncriteria: {}\nfindings: {}\nevidence: {}",
            self.review_id,
            self.status.name(),
            self.contract_id,
            self.contract_version,
            self.revision.fingerprint,
            self.criteria.len(),
            self.findings.len(),
            self.evidence_ids.join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(status: ReviewStatus) -> ReviewReport {
        ReviewReport {
            report_version: 1,
            review_id: "r1".into(),
            session_id: "s".into(),
            contract_id: "c".into(),
            contract_version: 1,
            revision: WorkspaceRevision {
                fingerprint: "rev".into(),
                ..Default::default()
            },
            criteria: vec![CriterionResult {
                criterion: "works".into(),
                status: Some(CriterionStatus::Met),
                evidence_ids: vec!["e1".into()],
                note: None,
            }],
            findings: Vec::new(),
            evidence_ids: vec!["e1".into()],
            status,
            created_at: "now".into(),
        }
    }

    #[test]
    fn approved_review_needs_evidence_and_met_criteria() {
        assert!(report(ReviewStatus::Approved).validate().is_ok());
        let mut no_evidence = report(ReviewStatus::Approved);
        no_evidence.evidence_ids.clear();
        assert!(no_evidence.validate().is_err());
        let mut unmet = report(ReviewStatus::Approved);
        unmet.criteria[0].status = Some(CriterionStatus::Unmet);
        assert!(unmet.validate().is_err());
        assert!(report(ReviewStatus::ChangesRequested).validate().is_ok());
    }
}
