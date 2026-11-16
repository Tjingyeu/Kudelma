use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    Contract,
    Design,
    Plan,
    TddRed,
    TddGreen,
    Review,
    Verification,
    Finish,
}

impl GateKind {
    pub const ALL: &'static [Self] = &[
        Self::Contract,
        Self::Design,
        Self::Plan,
        Self::TddRed,
        Self::TddGreen,
        Self::Review,
        Self::Verification,
        Self::Finish,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Contract => "contract",
            Self::Design => "design",
            Self::Plan => "plan",
            Self::TddRed => "tdd-red",
            Self::TddGreen => "tdd-green",
            Self::Review => "review",
            Self::Verification => "verification",
            Self::Finish => "finish",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateStatus {
    Pending,
    Passed,
    Failed,
    Skipped,
    Unverified,
}

impl GateStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Unverified => "unverified",
        }
    }

    pub fn blocking(self) -> bool {
        matches!(self, Self::Pending | Self::Failed | Self::Unverified)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateView {
    pub kind: GateKind,
    pub status: GateStatus,
    pub reason: Option<String>,
    pub evidence_id: Option<String>,
}
