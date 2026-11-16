use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex, RwLock};

use kudelma_config::Config;
use kudelma_providers::{Model, resolve_model};
use kudelma_storage::{AuthStore, now_rfc3339};

use crate::lead::LeadMode;
use crate::orchestration::{
    Complexity, ContractStatus, ContractStore, ExecutionLedger, TaskContract,
};
use crate::skills::SkillRegistry;
use crate::verification::{EvidenceResult, ExecutionEvidence, GateKind, GateStatus, Verifier};
use crate::workflows::{WorkflowKind, WorkflowState, WorkflowStore};

pub struct RuntimeState {
    pub session_id: String,
    pub cwd: PathBuf,
    pub config: Arc<Config>,
    pub trusted: bool,
    pub yolo: bool,
    pub auth: Option<AuthStore>,
    pub events: tokio::sync::mpsc::UnboundedSender<crate::events::AgentEvent>,
    pub mode: RwLock<LeadMode>,
    pub contract: RwLock<Option<TaskContract>>,
    pub workflow: Mutex<WorkflowState>,
    pub skills: Arc<SkillRegistry>,
    pub ledger: Mutex<ExecutionLedger>,
    pub verifier: Verifier,
    pub contracts: ContractStore,
    pub workflows: WorkflowStore,
    pub loaded_skills: Mutex<HashSet<String>>,
    pub blocked_attempts: AtomicU32,
    pub active_workers: AtomicU32,
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
            auth,
            events,
            mode: RwLock::new(LeadMode::Build),
            contract: RwLock::new(None),
            workflow: Mutex::new(workflow.clone()),
            skills,
            ledger: Mutex::new(ExecutionLedger::new(workflow.id.clone())),
            verifier: Verifier::new().map_err(|error| error.to_string())?,
            contracts: ContractStore::new().map_err(|error| error.to_string())?,
            workflows: WorkflowStore::new().map_err(|error| error.to_string())?,
            loaded_skills: Mutex::new(HashSet::new()),
            blocked_attempts: AtomicU32::new(0),
            active_workers: AtomicU32::new(0),
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

    pub fn set_mode(&self, mode: LeadMode, reason: &str) -> bool {
        let mut current = self.mode.write().unwrap_or_else(|error| error.into_inner());
        if *current == mode {
            return false;
        }
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
        true
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

    pub fn record_verification(&self, evidence: &ExecutionEvidence) {
        let status = match evidence.result {
            EvidenceResult::Passed => GateStatus::Passed,
            EvidenceResult::Failed => GateStatus::Failed,
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

    pub fn completion_blocker(&self) -> Option<String> {
        let mode = self.mode_name();
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
        if mode == "review" {
            workflow.mark_review_complete();
        }
        if workflow.complexity.requires_plan()
            && workflow.gate(GateKind::Plan) == GateStatus::Pending
        {
            return Some("plan gate: finish the plan, then use lead_mode to move to build".into());
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
                "review gate: switch to review mode with the lead_mode tool, inspect the diff and evidence, then answer again"
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
        None
    }

    pub fn invalidate_verification(&self) {
        let mut workflow = self.workflow.lock().unwrap_or_else(|e| e.into_inner());
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

    pub fn model_for(&self, role: &str, fallback: &Model) -> Result<Model, String> {
        match self.config.role_model_spec(role) {
            Some(spec) => resolve_model(&self.config, self.auth.as_ref(), &spec)
                .map_err(|error| error.to_string()),
            None => Ok(fallback.clone()),
        }
    }
}
