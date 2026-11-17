use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex, RwLock};

use kudelma_config::Config;
use kudelma_providers::{Model, resolve_model};
use kudelma_storage::{ArtifactStore, AuthStore, now_rfc3339};

use crate::lead::LeadMode;
use crate::orchestration::{
    Complexity, ContractStatus, ContractStore, ExecutionLedger, TaskContract,
};
use crate::skills::SkillRegistry;
use crate::verification::{
    EvidenceResult, ExecutionEvidence, GateKind, GateStatus, ReviewReport, ReviewStatus, Verifier,
};
use crate::workflows::{WorkflowKind, WorkflowState, WorkflowStore};

pub struct RuntimeState {
    pub session_id: String,
    pub cwd: PathBuf,
    pub config: Arc<Config>,
    pub trusted: bool,
    pub yolo: bool,
    pub auth: RwLock<Option<AuthStore>>,
    pub events: tokio::sync::mpsc::UnboundedSender<crate::events::AgentEvent>,
    pub mode: RwLock<LeadMode>,
    pub contract: RwLock<Option<TaskContract>>,
    pub workflow: Mutex<WorkflowState>,
    pub skills: Arc<SkillRegistry>,
    pub ledger: Mutex<ExecutionLedger>,
    pub verifier: Verifier,
    pub reviews: ArtifactStore,
    pub contracts: ContractStore,
    pub workflows: WorkflowStore,
    pub loaded_skills: Mutex<HashSet<String>>,
    pub blocked_attempts: AtomicU32,
    pub active_workers: AtomicU32,
    pub mutation_generation: std::sync::atomic::AtomicU64,
}

impl RuntimeState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: &str,
        cwd: &Path,
        config: Arc<Config>,
        trusted: bool,
        yolo: bool,
        auth: Option<AuthStore>,
        events: tokio::sync::mpsc::UnboundedSender<crate::events::AgentEvent>,
        skills: Arc<SkillRegistry>,
        workflow: WorkflowState,
    ) -> Result<Self, String> {
        Ok(Self {
            session_id: session_id.into(),
            cwd: cwd.to_path_buf(),
            config,
            trusted,
            yolo,
            auth: RwLock::new(auth),
            events,
            mode: RwLock::new(LeadMode::Build),
            contract: RwLock::new(None),
            workflow: Mutex::new(workflow.clone()),
            skills,
            ledger: Mutex::new(ExecutionLedger::new(workflow.id.clone())),
            verifier: Verifier::new().map_err(|error| error.to_string())?,
            reviews: ArtifactStore::new(
                kudelma_storage::reviews_dir().map_err(|error| error.to_string())?,
            ),
            contracts: ContractStore::new().map_err(|error| error.to_string())?,
            workflows: WorkflowStore::new().map_err(|error| error.to_string())?,
            loaded_skills: Mutex::new(HashSet::new()),
            blocked_attempts: AtomicU32::new(0),
            active_workers: AtomicU32::new(0),
            mutation_generation: std::sync::atomic::AtomicU64::new(0),
        })
    }

    pub fn mode(&self) -> LeadMode {
        self.mode
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn mode_name(&self) -> String {
        self.mode().name().into()
    }

    pub fn set_mode(&self, mode: LeadMode, reason: &str) -> Result<bool, String> {
        let current_mode = self.mode();
        if current_mode == mode {
            return Ok(false);
        }
        if let LeadMode::Plan(path) = &current_mode {
            let text = std::fs::read_to_string(path).map_err(|error| {
                format!(
                    "plan artifact {} cannot be read: {error}; write the plan before leaving PLAN",
                    path.display()
                )
            })?;
            validate_plan_text(&text, self.workflow().complexity)?;
            let hash = plan_hash(&text);
            let mut workflow = self.workflow.lock().unwrap_or_else(|e| e.into_inner());
            workflow.plan_path = Some(path.display().to_string());
            workflow.plan_hash = Some(hash.clone());
            workflow.set_gate(
                GateKind::Plan,
                GateStatus::Passed,
                Some(format!("plan {} ({hash})", path.display())),
                Some(format!("plan:{hash}")),
            );
            if workflow.complexity == Complexity::L3 {
                let design_hash = design_section_hash(&text).ok_or_else(|| {
                    "L3 plan must contain non-empty `## Design` and `## Risks` sections".to_string()
                })?;
                workflow.design_hash = Some(design_hash.clone());
                workflow.set_gate(
                    GateKind::Design,
                    GateStatus::Passed,
                    Some(format!("design section ({design_hash})")),
                    Some(format!("design:{design_hash}")),
                );
            }
            self.save_workflow_locked(&workflow);
        }
        {
            let mut current = self.mode.write().unwrap_or_else(|error| error.into_inner());
            let from = current.name().to_string();
            let to = mode.name().to_string();
            *current = mode;
            drop(current);
            let mut workflow = self.workflow.lock().unwrap_or_else(|e| e.into_inner());
            workflow.record_mode(&from, &to, reason);
            self.save_workflow_locked(&workflow);
            let _ = self.events.send(crate::events::AgentEvent::ModeChanged {
                from,
                to,
                reason: reason.into(),
            });
            let _ = self
                .events
                .send(crate::events::AgentEvent::WorkflowUpdated(workflow.clone()));
        }
        Ok(true)
    }

    pub fn contract(&self) -> Option<TaskContract> {
        self.contract
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn set_contract(&self, contract: TaskContract) -> Result<(), String> {
        contract.validate()?;
        let previous = self.contract();
        let new_task = previous
            .as_ref()
            .map(|previous| {
                previous.contract_id != contract.contract_id
                    || previous.contract_version != contract.contract_version
            })
            .unwrap_or(true);
        if new_task {
            let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
            ledger.accepted = false;
            ledger.event(
                "contract",
                format!("{} v{}", contract.contract_id, contract.contract_version),
            );
            drop(ledger);
            self.save_ledger();
        }
        self.contracts
            .save(&contract)
            .map_err(|error| error.to_string())?;
        {
            let mut workflow = self.workflow.lock().unwrap_or_else(|e| e.into_inner());
            workflow.attach_contract(&contract);
            self.save_workflow_locked(&workflow);
        }
        *self
            .contract
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(contract.clone());
        let _ = self
            .events
            .send(crate::events::AgentEvent::ContractUpdated(contract));
        let _ = self
            .events
            .send(crate::events::AgentEvent::WorkflowUpdated(self.workflow()));
        Ok(())
    }

    pub fn approve_contract(&self, contract_id: &str) -> Result<TaskContract, String> {
        let contract = self
            .contracts
            .approve(contract_id)
            .map_err(|error| error.to_string())?;
        {
            let mut workflow = self.workflow.lock().unwrap_or_else(|e| e.into_inner());
            workflow.attach_contract(&contract);
            self.save_workflow_locked(&workflow);
        }
        *self
            .contract
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(contract.clone());
        let _ = self
            .events
            .send(crate::events::AgentEvent::ContractUpdated(contract.clone()));
        let _ = self
            .events
            .send(crate::events::AgentEvent::WorkflowUpdated(self.workflow()));
        Ok(contract)
    }

    pub fn workflow(&self) -> WorkflowState {
        self.workflow
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn save_workflow(&self) {
        let workflow = self
            .workflow
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.save_workflow_locked(&workflow);
    }

    fn save_workflow_locked(&self, workflow: &WorkflowState) {
        let _ = self.workflows.save(workflow);
    }

    pub fn mutation_generation(&self) -> u64 {
        self.mutation_generation
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn bump_mutation_generation(&self) {
        self.mutation_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    pub async fn current_revision(&self) -> crate::verification::WorkspaceRevision {
        crate::verification::WorkspaceRevision::capture(&self.cwd, self.mutation_generation()).await
    }

    pub fn record_verification(&self, evidence: &ExecutionEvidence) {
        let status = match evidence.result {
            EvidenceResult::Passed if evidence.revision_after.is_some() => GateStatus::Passed,
            EvidenceResult::Passed => GateStatus::Unverified,
            EvidenceResult::Failed => GateStatus::Failed,
            EvidenceResult::Skipped if self.config.workflow.require_verification => {
                GateStatus::Unverified
            }
            EvidenceResult::Skipped => GateStatus::Skipped,
            EvidenceResult::Blocked => GateStatus::Unverified,
        };
        {
            let mut workflow = self.workflow.lock().unwrap_or_else(|e| e.into_inner());
            workflow.record_evidence(&evidence.id, status);
            self.save_workflow_locked(&workflow);
        }
        let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        ledger.event(
            "verification",
            format!("{} {}", evidence.result.name(), evidence.command),
        );
        drop(ledger);
        self.save_ledger();
        let _ = self
            .events
            .send(crate::events::AgentEvent::Verification(evidence.clone()));
        let _ = self
            .events
            .send(crate::events::AgentEvent::WorkflowUpdated(self.workflow()));
    }

    pub fn require_review_after_worker(&self) {
        let mut workflow = self.workflow.lock().unwrap_or_else(|e| e.into_inner());
        if workflow.kind == WorkflowKind::SimpleTask {
            return;
        }
        workflow.set_gate(
            GateKind::Review,
            GateStatus::Pending,
            Some("worker completed; review required".into()),
            None,
        );
        self.save_workflow_locked(&workflow);
    }

    pub fn begin_worker(&self) -> Result<(), String> {
        let max = self.config.lead.max_children() as u32;
        if self
            .active_workers
            .load(std::sync::atomic::Ordering::SeqCst)
            >= max
        {
            return Err(format!(
                "worker limit reached ({}); finish the current tasks before dispatching more",
                max
            ));
        }
        self.active_workers
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        ledger.workers_started += 1;
        drop(ledger);
        self.save_ledger();
        Ok(())
    }

    pub fn finish_worker(&self, succeeded: bool) {
        self.active_workers
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        if succeeded {
            ledger.workers_succeeded += 1;
        }
        drop(ledger);
        self.save_ledger();
    }

    pub fn record_skill(&self, name: &str, hash: &str) {
        self.loaded_skills
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string());
        let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        ledger.record_skill(name, hash);
        drop(ledger);
        {
            let mut workflow = self.workflow.lock().unwrap_or_else(|e| e.into_inner());
            if !workflow.skills_loaded.iter().any(|skill| skill == name) {
                workflow.skills_loaded.push(name.to_string());
                self.save_workflow_locked(&workflow);
            }
        }
        self.save_ledger();
    }

    pub fn record_turn(&self, role: &str, input: u64, output: u64, cost: f64) {
        let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        ledger.record_turn(role, input, output, cost);
        drop(ledger);
        self.save_ledger();
    }

    pub fn save_ledger(&self) {
        let ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(dir) = kudelma_storage::workflows_dir() {
            let store = kudelma_storage::ArtifactStore::new(dir);
            let workflow_id = self.workflow().id;
            let _ = store.save(&format!("ledger-{workflow_id}"), &*ledger);
        }
    }

    pub fn budget_blocker(&self) -> Option<String> {
        let ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        for (role, limit) in &self.config.budgets.role_cost_usd {
            if let Some(usage) = ledger.roles.get(role)
                && *limit > 0.0
                && usage.cost_usd > *limit
            {
                return Some(format!(
                    "{role} cost budget exceeded (${:.4} > ${limit:.4})",
                    usage.cost_usd
                ));
            }
        }
        if self.config.budgets.task_cost_usd > 0.0
            && ledger.total_cost() > self.config.budgets.task_cost_usd
        {
            return Some(format!(
                "task cost budget exceeded (${:.4} > ${:.4})",
                ledger.total_cost(),
                self.config.budgets.task_cost_usd
            ));
        }
        if self.config.budgets.task_wall_clock_secs > 0 {
            let workflow = self.workflow.lock().unwrap_or_else(|e| e.into_inner());
            if let Ok(started) = chrono::DateTime::parse_from_rfc3339(&workflow.created_at) {
                let elapsed = chrono::Utc::now().signed_duration_since(started);
                if elapsed.num_seconds() as u64 > self.config.budgets.task_wall_clock_secs {
                    return Some(format!(
                        "task wall-clock budget exceeded ({}s > {}s)",
                        elapsed.num_seconds(),
                        self.config.budgets.task_wall_clock_secs
                    ));
                }
            }
        }
        None
    }

    pub async fn completion_blocker(&self) -> Option<String> {
        let mode = self.mode_name();
        let (verification_id, review_snapshot) = {
            let mut workflow = self.workflow.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(contract) = self.contract() {
                if contract.status == ContractStatus::Approved {
                    workflow.set_gate(GateKind::Contract, GateStatus::Passed, None, None);
                } else if contract.needs_approval() {
                    return Some(format!(
                        "contract approval required: run /contract approve {}",
                        contract.contract_id
                    ));
                }
            }
            if let Some(path) = workflow.plan_path.clone() {
                match std::fs::read_to_string(&path) {
                    Ok(text) if Some(plan_hash(&text)) == workflow.plan_hash => {
                        if workflow.complexity == Complexity::L3
                            && workflow.design_hash.as_deref()
                                != design_section_hash(&text).as_deref()
                        {
                            workflow.set_gate(
                                GateKind::Design,
                                GateStatus::Pending,
                                Some("design section changed after approval".into()),
                                None,
                            );
                            self.save_workflow_locked(&workflow);
                            return Some(
                                "design gate: the plan design section changed; review it again"
                                    .into(),
                            );
                        }
                    }
                    Ok(_) => {
                        workflow.set_gate(
                            GateKind::Plan,
                            GateStatus::Pending,
                            Some("plan artifact changed after approval".into()),
                            None,
                        );
                        self.save_workflow_locked(&workflow);
                        return Some(
                            "plan gate: the plan file changed; review it and leave PLAN again"
                                .into(),
                        );
                    }
                    Err(error) => {
                        workflow.set_gate(
                            GateKind::Plan,
                            GateStatus::Pending,
                            Some(format!("plan artifact unavailable: {error}")),
                            None,
                        );
                        self.save_workflow_locked(&workflow);
                        return Some("plan gate: the plan artifact is missing".into());
                    }
                }
            }
            if workflow.complexity.requires_plan()
                && workflow.gate(GateKind::Plan) == GateStatus::Pending
            {
                return Some(
                    "plan gate: finish the plan, then use lead_mode to move to build".into(),
                );
            }
            if workflow.complexity == Complexity::L3
                && workflow.gate(GateKind::Design) == GateStatus::Pending
            {
                return Some(
                    "design gate: record and review the design decision, then move to build".into(),
                );
            }
            if self.config.workflow.require_review
                && workflow.gate(GateKind::Review) != GateStatus::Passed
                && workflow.gate(GateKind::Review) != GateStatus::Skipped
            {
                return Some(
                    "review gate: switch to REVIEW and submit a structured `submit_review` report"
                        .into(),
                );
            }
            if self.config.workflow.require_verification
                && workflow.gate(GateKind::Verification) != GateStatus::Passed
                && workflow.gate(GateKind::Verification) != GateStatus::Skipped
            {
                return Some(
                    "verification gate: run the verify tool for the relevant checks before finishing"
                        .into(),
                );
            }
            self.save_workflow_locked(&workflow);
            let verification = if workflow.gate(GateKind::Verification) == GateStatus::Passed {
                workflow
                    .gates
                    .get(GateKind::Verification.name())
                    .and_then(|gate| gate.evidence_id.clone())
                    .or_else(|| workflow.evidence_ids.last().cloned())
            } else {
                None
            };
            let review = workflow.review_report_id.as_ref().map(|id| {
                (
                    id.clone(),
                    workflow.review_fingerprint.clone(),
                    workflow.review_contract_version,
                    workflow.contract_version,
                )
            });
            (verification, review)
        };

        if let Some(id) = verification_id {
            let evidence = match self.verifier.load(&id) {
                Ok(evidence) => evidence,
                Err(error) => {
                    self.reset_verification_gate(format!(
                        "verification evidence {id} cannot be loaded: {error}"
                    ));
                    return Some("verification evidence is missing; run verify again".into());
                }
            };
            let current = self.current_revision().await;
            let same_workspace = evidence.cwd == current.workspace;
            let same_revision = evidence
                .revision_after
                .as_ref()
                .is_some_and(|revision| revision.matches(&current));
            if !same_workspace || !same_revision {
                self.reset_verification_gate(
                    "workspace changed after the last verification".into(),
                );
                return Some(
                    "verification gate: the workspace changed after the last passing evidence; run verify again"
                        .into(),
                );
            }
        }
        if let Some((id, fingerprint, contract_version, current_version)) = review_snapshot {
            let current = self.current_revision().await;
            let same_revision = fingerprint.as_deref() == Some(current.fingerprint.as_str());
            let same_contract = contract_version == Some(current_version);
            if !same_revision || !same_contract || self.reviews.load::<ReviewReport>(&id).is_err() {
                self.reset_review_gate(
                    "workspace or contract changed after the review report".into(),
                );
                return Some(
                    "review gate: the review report no longer matches the current workspace/contract; submit a new review"
                        .into(),
                );
            }
        } else if mode == "review" && self.config.workflow.require_review {
            return Some(
                "review gate: no structured review report exists; call `submit_review` from REVIEW"
                    .into(),
            );
        }
        None
    }

    fn reset_review_gate(&self, reason: String) {
        let mut workflow = self.workflow.lock().unwrap_or_else(|e| e.into_inner());
        workflow.review_completed = false;
        workflow.review_report_id = None;
        workflow.review_fingerprint = None;
        workflow.review_contract_version = None;
        workflow.set_gate(GateKind::Review, GateStatus::Pending, Some(reason), None);
        self.save_workflow_locked(&workflow);
    }

    fn reset_verification_gate(&self, reason: String) {
        let mut workflow = self.workflow.lock().unwrap_or_else(|e| e.into_inner());
        workflow.set_gate(
            GateKind::Verification,
            GateStatus::Pending,
            Some(reason),
            None,
        );
        self.save_workflow_locked(&workflow);
    }

    pub fn invalidate_verification(&self) {
        self.bump_mutation_generation();
        let mut workflow = self.workflow.lock().unwrap_or_else(|e| e.into_inner());
        workflow.review_completed = false;
        workflow.review_report_id = None;
        workflow.review_fingerprint = None;
        workflow.review_contract_version = None;
        let mut changed = false;
        if workflow.gate(GateKind::Verification) == GateStatus::Passed {
            workflow.set_gate(
                GateKind::Verification,
                GateStatus::Pending,
                Some("code changed after the last verification".into()),
                None,
            );
            changed = true;
        }
        if workflow.gate(GateKind::Review) == GateStatus::Passed {
            workflow.set_gate(
                GateKind::Review,
                GateStatus::Pending,
                Some("code changed after review".into()),
                None,
            );
            changed = true;
        }
        if changed {
            self.save_workflow_locked(&workflow);
            let _ = self
                .events
                .send(crate::events::AgentEvent::WorkflowUpdated(workflow.clone()));
        }
    }

    pub fn mark_accepted(&self) {
        {
            let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
            ledger.accepted = true;
            ledger.event("accepted", now_rfc3339());
        }
        self.save_ledger();
        let mut workflow = self.workflow.lock().unwrap_or_else(|e| e.into_inner());
        workflow.accepted = true;
        workflow.stage = crate::workflows::WorkflowStage::Done;
        workflow.set_gate(GateKind::Finish, GateStatus::Passed, None, None);
        self.save_workflow_locked(&workflow);
    }

    pub fn approve_if_configured(&self) -> bool {
        matches!(
            self.config.workflow.require_spec_approval,
            kudelma_config::ApprovalPolicy::Never
        )
    }

    pub fn default_workflow(&self) -> WorkflowKind {
        WorkflowKind::from_preset(self.config.workflow.default).unwrap_or(WorkflowKind::SimpleTask)
    }

    pub fn set_workflow_kind(&self, kind: WorkflowKind) {
        let mut workflow = self.workflow.lock().unwrap_or_else(|e| e.into_inner());
        workflow.kind = kind;
        self.save_workflow_locked(&workflow);
    }

    pub async fn submit_review(&self, report: ReviewReport) -> Result<ReviewReport, String> {
        report.validate()?;
        let contract = self
            .contract()
            .ok_or_else(|| "a review requires an approved task contract".to_string())?;
        if report.contract_id != contract.contract_id
            || report.contract_version != contract.contract_version
        {
            return Err("review report does not match the current contract version".into());
        }
        let current = self.current_revision().await;
        if !report.revision.matches(&current) {
            return Err("workspace changed before the review report was submitted".into());
        }
        for expected in &contract.acceptance_criteria {
            if !report.criteria.iter().any(|criterion| {
                criterion
                    .criterion
                    .trim()
                    .eq_ignore_ascii_case(expected.trim())
            }) {
                return Err(format!(
                    "review report is missing the acceptance criterion `{expected}`"
                ));
            }
        }
        for id in &report.evidence_ids {
            let evidence = self
                .verifier
                .load(id)
                .map_err(|error| format!("review references unknown evidence {id}: {error}"))?;
            if !evidence
                .revision_after
                .as_ref()
                .is_some_and(|revision| revision.matches(&current))
            {
                return Err(format!(
                    "review evidence {id} does not match the current workspace revision"
                ));
            }
            if report.status == ReviewStatus::Approved && evidence.result != EvidenceResult::Passed
            {
                return Err(format!(
                    "review evidence {id} did not pass; an approved review needs passing evidence"
                ));
            }
        }
        let approved = report.status == ReviewStatus::Approved;
        self.reviews
            .save(&report.review_id, &report)
            .map_err(|error| error.to_string())?;
        {
            let mut workflow = self.workflow.lock().unwrap_or_else(|e| e.into_inner());
            workflow.record_review(
                &report.review_id,
                &report.revision.fingerprint,
                report.contract_version,
                approved,
                Some(report.status.name().into()),
            );
            self.save_workflow_locked(&workflow);
        }
        let _ = self
            .events
            .send(crate::events::AgentEvent::ReviewUpdated(report.clone()));
        Ok(report)
    }

    pub fn model_for(&self, role: &str, fallback: &Model) -> Result<Model, String> {
        match self.config.role_model_spec(role) {
            Some(spec) => {
                let auth = self
                    .auth
                    .read()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
                resolve_model(&self.config, auth.as_ref(), &spec).map_err(|error| error.to_string())
            }
            None => Ok(fallback.clone()),
        }
    }

    pub fn update_auth(&self, auth: AuthStore) {
        *self.auth.write().unwrap_or_else(|error| error.into_inner()) = Some(auth);
    }
}

fn validate_plan_text(text: &str, complexity: Complexity) -> Result<(), String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("plan artifact is empty".into());
    }
    let lines = trimmed
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    if lines < 3 {
        return Err("plan artifact is too short; include ordered steps and verification".into());
    }
    let lower = trimmed.to_ascii_lowercase();
    if !lower.contains("test") && !lower.contains("verify") {
        return Err("plan artifact must describe how the change will be verified".into());
    }
    if complexity == Complexity::L3 {
        design_section_hash(trimmed).ok_or_else(|| {
            "L3 plan needs non-empty `## Design` and `## Risks` sections".to_string()
        })?;
    }
    Ok(())
}

fn plan_hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn design_section_hash(text: &str) -> Option<String> {
    let mut sections: std::collections::BTreeMap<&str, String> = std::collections::BTreeMap::new();
    let mut current: Option<&str> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(title) = trimmed.strip_prefix("## ") {
            current = match title.trim().to_ascii_lowercase().as_str() {
                "design" => Some("design"),
                "risks" | "risk" => Some("risks"),
                _ => None,
            };
            if let Some(key) = current {
                sections.entry(key).or_default();
            }
            continue;
        }
        if let Some(key) = current {
            sections.entry(key).or_default().push_str(line);
            sections.entry(key).or_default().push('\n');
        }
    }
    let design = sections.get("design")?.trim();
    let risks = sections.get("risks")?.trim();
    if design.is_empty() || risks.is_empty() {
        return None;
    }
    Some(plan_hash(&format!("{design}\n{risks}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_requires_content_and_verification() {
        assert!(validate_plan_text("", Complexity::L2).is_err());
        assert!(validate_plan_text("do it", Complexity::L2).is_err());
        assert!(
            validate_plan_text(
                "1. change code\n2. add test\n3. run cargo test",
                Complexity::L2
            )
            .is_ok()
        );
    }

    #[test]
    fn l3_plan_requires_design_and_risks() {
        let text = "1. change code\n2. add test\n3. run cargo test";
        assert!(validate_plan_text(text, Complexity::L3).is_err());
        let text = format!("{text}\n## Design\nUse a queue.\n## Risks\nLatency.");
        assert!(validate_plan_text(&text, Complexity::L3).is_ok());
    }
}
