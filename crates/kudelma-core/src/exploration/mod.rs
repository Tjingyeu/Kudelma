//! Shared read-only exploration capability.
//!
//! Exploration is not an agent and has no model or session of its own. It
//! performs deterministic retrieval and returns source-grounded evidence;
//! Agency or Lead interprets the report with its own configured model.

pub mod evidence;
pub mod executor;
pub mod request;

pub use evidence::{
    Confidence, CoverageReport, EvidenceCite, ExplorationReport, ExplorationStatus, Finding,
    FindingKind, SymbolRelationship,
};
pub use executor::{ScanOptions, explore, request_from_tool, scan};
pub use request::{
    Caller, ExplorationLimits, ExplorationRequest, ExplorationScope, InvestigationMode,
};
