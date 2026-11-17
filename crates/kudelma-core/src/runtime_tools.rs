use std::sync::Arc;

use async_trait::async_trait;
use kudelma_config::ToolKey;
use kudelma_permissions::{AgentRole, Scope, analyze_bash};
use kudelma_storage::plans_dir;
use serde_json::{Value, json};

use crate::exploration::{self, Caller, ExplorationLimits, InvestigationMode};
use crate::lead::LeadMode;
use crate::orchestration::{Complexity, ContractStatus, IntentKind, TaskContract};
use crate::runtime::RuntimeState;
use crate::verification::{
    CriterionResult, CriterionStatus, EvidenceKind, ReviewFinding, ReviewReport, ReviewStatus,
};
use crate::workflows::WorkflowKind;
use kudelma_tools::{
    Tool, ToolContext, ToolError, ToolOutput, ToolPermission, object_schema, required_str,
};

pub struct SkillTool {
    pub runtime: Arc<RuntimeState>,
    pub role: AgentRole,
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &'static str {
        "skill"
    }

    fn description(&self) -> &'static str {
        "Load the full instructions of a skill listed in the system prompt. Use it when the current phase or task matches the skill."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "name": { "type": "string", "description": "Skill name from the available-skills list" }
            }),
            &["name"],
        )
    }

    fn read_only(&self) -> bool {
        true
    }

    fn permission(&self, args: &Value, _ctx: &ToolContext) -> Result<ToolPermission, ToolError> {
        Ok(ToolPermission::new(vec![Scope::exact(format!(
            "skill:{}",
            args.get("name").and_then(Value::as_str).unwrap_or("")
        ))]))
    }

    fn key(&self) -> ToolKey {
        ToolKey::native("skill")
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let name = required_str(&args, "name")?;
        let mode = self.runtime.mode_name();
        let skill = self
            .runtime
            .skills
            .load(&name, self.role.name(), &mode)
            .map_err(ToolError::Other)?;
        self.runtime
            .record_skill(&skill.metadata.name, &skill.content_hash);
        Ok(ToolOutput::success(format!(
            "# Skill: {}\n{}\n\n<!-- {} / {} -->",
            skill.metadata.name,
            skill.body,
            skill.source.label(),
            skill.content_hash
        )))
    }
}

pub struct LeadModeTool {
    pub runtime: Arc<RuntimeState>,
}

#[async_trait]
impl Tool for LeadModeTool {
    fn name(&self) -> &'static str {
        "lead_mode"
    }

    fn description(&self) -> &'static str {
        "Switch the Lead phase. Modes: plan (read-only design), build, debug, review. Provide a short reason; the transition is recorded in the workflow."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "mode": { "type": "string", "enum": ["plan", "build", "debug", "review"] },
                "reason": { "type": "string", "description": "Why this transition is needed" }
            }),
            &["mode", "reason"],
        )
    }

    fn key(&self) -> ToolKey {
        ToolKey::native("lead_mode")
    }

    fn read_only(&self) -> bool {
        true
    }

    fn permission(&self, args: &Value, _ctx: &ToolContext) -> Result<ToolPermission, ToolError> {
        Ok(ToolPermission::new(vec![Scope::exact(format!(
            "mode:{}",
            args.get("mode").and_then(Value::as_str).unwrap_or("")
        ))]))
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let mode_name = required_str(&args, "mode")?;
        let reason = required_str(&args, "reason")?;
        let plan_path = plans_dir()
            .map_err(|error| ToolError::Other(error.to_string()))?
            .join(format!("{}.md", self.runtime.session_id));
        let mode = LeadMode::from_name(&mode_name, plan_path)
            .ok_or_else(|| ToolError::InvalidArgs(format!("unknown mode `{mode_name}`")))?;
        let previous = self.runtime.mode_name();
        match self.runtime.set_mode(mode, &reason) {
            Ok(true) => Ok(ToolOutput::success(format!(
                "Lead mode: {previous} -> {mode_name} ({reason})"
            ))),
            Ok(false) => Ok(ToolOutput::success(format!("Already in {mode_name} mode."))),
            Err(error) => Err(ToolError::Other(error)),
        }
    }
}

pub struct VerifyTool {
    pub runtime: Arc<RuntimeState>,
}

#[async_trait]
impl Tool for VerifyTool {
    fn name(&self) -> &'static str {
        "verify"
    }

    fn description(&self) -> &'static str {
        "Run a deterministic build/test/lint command through the Verifier. Records command, cwd, exit code, git revision, diff hash and output as execution evidence. Use skip_reason instead of inventing a pass."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "command": { "type": "string", "description": "Command to execute, e.g. cargo test --workspace" },
                "kind": { "type": "string", "enum": ["test", "build", "lint", "acceptance", "custom"] },
                "skip_reason": { "type": "string", "description": "Record why this check cannot run instead of executing it" }
            }),
            &["command"],
        )
    }

    fn key(&self) -> ToolKey {
        ToolKey::native("verify")
    }

    fn read_only(&self) -> bool {
        true
    }

    fn permission(&self, args: &Value, ctx: &ToolContext) -> Result<ToolPermission, ToolError> {
        if args.get("skip_reason").and_then(Value::as_str).is_some() {
            return Ok(ToolPermission::new(vec![Scope::exact(format!(
                "skip:{}",
                args.get("command").and_then(Value::as_str).unwrap_or("")
            ))]));
        }
        let command = required_str(args, "command")?;
        let analysis = analyze_bash(&command);
        Ok(ToolPermission {
            scopes: analysis.scopes,
            force_prompt: analysis.force_prompt,
            reason: Some("verification commands are subject to permission policy".into()),
            cwd: Some(ctx.cwd().to_path_buf()),
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let command = required_str(&args, "command")?;
        let kind = match args.get("kind").and_then(Value::as_str).unwrap_or("custom") {
            "test" => EvidenceKind::Test,
            "build" => EvidenceKind::Build,
            "lint" => EvidenceKind::Lint,
            "acceptance" => EvidenceKind::Acceptance,
            _ => EvidenceKind::Custom,
        };
        if let Some(reason) = args.get("skip_reason").and_then(Value::as_str) {
            let evidence = self
                .runtime
                .verifier
                .record_skip(&command, reason, kind)
                .map_err(ToolError::Other)?;
            self.runtime.record_verification(&evidence);
            return Ok(ToolOutput::success(format!(
                "Recorded skipped check: {} ({reason})",
                evidence.id
            )));
        }
        let evidence = self
            .runtime
            .verifier
            .run(
                &command,
                ctx.cwd(),
                kind,
                self.runtime.mutation_generation(),
                &ctx.cancel,
            )
            .await
            .map_err(ToolError::Other)?;
        self.runtime.record_verification(&evidence);
        let output = format!(
            "{}\nstdout: {}\nstderr: {}\nchanged: {}\nlog: {}",
            evidence.summary(),
            evidence.stdout_excerpt,
            evidence.stderr_excerpt,
            evidence.changed_files.join(", "),
            evidence.log_path.as_deref().unwrap_or("none")
        );
        Ok(
            if evidence.result == crate::verification::EvidenceResult::Passed {
                ToolOutput::success(output)
            } else {
                ToolOutput::error(output)
            },
        )
    }
}

pub struct ExploreCodebaseTool {
    pub runtime: Arc<RuntimeState>,
    pub role: AgentRole,
}

#[async_trait]
impl Tool for ExploreCodebaseTool {
    fn name(&self) -> &'static str {
        "explore_codebase"
    }

    fn description(&self) -> &'static str {
        "Shared read-only exploration capability. Deterministically searches code, symbols and recent runtime evidence, then returns source-grounded findings. This does not start another model; interpret the evidence in the current session."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "objective": { "type": "string", "description": "The specific question or lookup objective" },
                "question": { "type": "string", "description": "Alias for objective" },
                "questions": { "type": "array", "items": { "type": "string" }, "description": "Additional focused questions" },
                "investigation_mode": { "type": "string", "enum": ["codebase", "code_explanation", "technical_research", "runtime_investigation", "project_knowledge"] },
                "scope_paths": { "type": "array", "items": { "type": "string" }, "description": "Files or directories to focus on" },
                "symbols": { "type": "array", "items": { "type": "string" }, "description": "Symbols or identifiers to locate" },
                "sources": { "type": "array", "items": { "type": "string" }, "description": "Sources: codebase, documentation, runtime, project_memory" },
                "expected_output": { "type": "string", "description": "What form of answer is useful" },
                "max_files": { "type": "integer", "minimum": 1 },
                "max_results": { "type": "integer", "minimum": 1 }
            }),
            &[],
        )
    }

    fn read_only(&self) -> bool {
        true
    }

    fn summary(&self, args: &Value) -> String {
        args.get("objective")
            .or_else(|| args.get("question"))
            .and_then(Value::as_str)
            .unwrap_or("exploration")
            .chars()
            .take(80)
            .collect()
    }

    fn key(&self) -> ToolKey {
        ToolKey::native("explore_codebase")
    }

    fn permission(&self, args: &Value, ctx: &ToolContext) -> Result<ToolPermission, ToolError> {
        let mut scopes = Vec::new();
        if let Some(paths) = args.get("scope_paths").and_then(Value::as_array) {
            for path in paths.iter().filter_map(Value::as_str) {
                scopes.push(Scope::path(ctx.env.resolve(path)));
            }
        }
        if scopes.is_empty() {
            scopes.push(Scope::path(ctx.cwd()));
        }
        Ok(ToolPermission::new(scopes))
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let objective = args
            .get("objective")
            .and_then(Value::as_str)
            .or_else(|| args.get("question").and_then(Value::as_str))
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| ToolError::InvalidArgs("missing `objective`".into()))?
            .to_string();
        let questions = string_list(&args, "questions");
        let mode_name = args
            .get("investigation_mode")
            .and_then(Value::as_str)
            .unwrap_or("codebase");
        let investigation_mode = InvestigationMode::parse(mode_name).ok_or_else(|| {
            ToolError::InvalidArgs(format!(
                "unknown investigation_mode `{mode_name}`; use codebase, code_explanation, technical_research, runtime_investigation or project_knowledge"
            ))
        })?;
        let paths = string_list(&args, "scope_paths");
        let symbols = string_list(&args, "symbols");
        let sources = string_list(&args, "sources");
        let expected_output = args
            .get("expected_output")
            .and_then(Value::as_str)
            .map(str::to_string);
        let limits = ExplorationLimits {
            read_only: true,
            max_files: args
                .get("max_files")
                .and_then(Value::as_u64)
                .map(|value| value as usize)
                .unwrap_or(ExplorationLimits::default().max_files),
            max_results: args
                .get("max_results")
                .and_then(Value::as_u64)
                .map(|value| value as usize)
                .unwrap_or(ExplorationLimits::default().max_results),
        };
        let caller = match self.role {
            AgentRole::Agency => Caller::Agency,
            _ => Caller::Lead,
        };
        let request = exploration::request_from_tool(
            caller,
            objective,
            questions,
            investigation_mode,
            paths,
            symbols,
            sources,
            expected_output,
            limits,
        );
        let report = exploration::explore(&self.runtime, ctx, request)
            .await
            .map_err(ToolError::Other)?;
        Ok(ToolOutput::success(report.render()).with_metadata(json!({
            "status": report.status,
            "findings": report.findings.len(),
            "relationships": report.relationships.len(),
            "files_inspected": report.coverage.files_inspected,
            "cached": report.cached,
        })))
    }
}

pub struct ReviewTool {
    pub runtime: Arc<RuntimeState>,
}

#[async_trait]
impl Tool for ReviewTool {
    fn name(&self) -> &'static str {
        "submit_review"
    }

    fn description(&self) -> &'static str {
        "Submit the structured REVIEW result for the current contract and workspace revision. Only available to Lead in review mode. Approved reports must cite verification evidence and mark every acceptance criterion as met."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "status": { "type": "string", "enum": ["approved", "changes_requested", "blocked"] },
                "criteria": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "criterion": { "type": "string" },
                            "status": { "type": "string", "enum": ["met", "partial", "unmet"] },
                            "evidence_ids": { "type": "array", "items": { "type": "string" } },
                            "note": { "type": "string" }
                        },
                        "required": ["criterion", "status"]
                    }
                },
                "findings": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "severity": { "type": "string" },
                            "message": { "type": "string" },
                            "file": { "type": "string" }
                        },
                        "required": ["severity", "message"]
                    }
                },
                "evidence_ids": { "type": "array", "items": { "type": "string" } }
            }),
            &["status", "criteria"],
        )
    }

    fn key(&self) -> ToolKey {
        ToolKey::native("submit_review")
    }

    fn read_only(&self) -> bool {
        true
    }

    fn permission(&self, _args: &Value, _ctx: &ToolContext) -> Result<ToolPermission, ToolError> {
        Ok(ToolPermission::new(vec![Scope::exact("review")]))
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        if self.runtime.mode_name() != "review" {
            return Err(ToolError::Other(
                "submit_review is only available in Lead review mode".into(),
            ));
        }
        let contract = self
            .runtime
            .contract()
            .ok_or_else(|| ToolError::Other("a review requires a task contract".into()))?;
        let status = match required_str(&args, "status")?.as_str() {
            "approved" => ReviewStatus::Approved,
            "changes_requested" => ReviewStatus::ChangesRequested,
            "blocked" => ReviewStatus::Blocked,
            other => {
                return Err(ToolError::InvalidArgs(format!(
                    "unknown review status `{other}`"
                )));
            }
        };
        let criteria = args
            .get("criteria")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| CriterionResult {
                        criterion: item["criterion"].as_str().unwrap_or_default().into(),
                        status: match item["status"].as_str() {
                            Some("met") => Some(CriterionStatus::Met),
                            Some("partial") => Some(CriterionStatus::Partial),
                            Some("unmet") => Some(CriterionStatus::Unmet),
                            _ => None,
                        },
                        evidence_ids: string_list(item, "evidence_ids"),
                        note: item["note"].as_str().map(str::to_string),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let findings = args
            .get("findings")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| ReviewFinding {
                        severity: item["severity"].as_str().unwrap_or("info").into(),
                        message: item["message"].as_str().unwrap_or_default().into(),
                        file: item["file"].as_str().map(str::to_string),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let revision = self.runtime.current_revision().await;
        let report = ReviewReport {
            report_version: 1,
            review_id: kudelma_storage::new_id(),
            session_id: self.runtime.session_id.clone(),
            contract_id: contract.contract_id.clone(),
            contract_version: contract.contract_version,
            revision,
            criteria,
            findings,
            evidence_ids: string_list(&args, "evidence_ids"),
            status,
            created_at: kudelma_storage::now_rfc3339(),
        };
        let report = self
            .runtime
            .submit_review(report)
            .await
            .map_err(ToolError::Other)?;
        Ok(ToolOutput::success(report.render()).with_metadata(json!({
            "review_id": report.review_id,
            "status": report.status.name(),
        })))
    }
}

pub struct SubmitContractTool {
    pub runtime: Arc<RuntimeState>,
}
#[async_trait]
impl Tool for SubmitContractTool {
    fn name(&self) -> &'static str {
        "submit_contract"
    }

    fn description(&self) -> &'static str {
        "Create or revise the Task Contract for a development request. Only call this when the intent is sufficiently actionable or the remaining questions are explicitly listed."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "intent_kind": { "type": "string", "enum": ["general_question", "code_question", "architecture_question", "feature_request", "bug_report", "change_request", "planning_request", "status_question"] },
                "complexity": { "type": "string", "enum": ["L0", "L1", "L2", "L3", "bug"] },
                "summary": { "type": "string" },
                "goals": { "type": "array", "items": { "type": "string" } },
                "requirements": { "type": "array", "items": { "type": "string" } },
                "non_goals": { "type": "array", "items": { "type": "string" } },
                "constraints": { "type": "array", "items": { "type": "string" } },
                "decisions": { "type": "array", "items": { "type": "string" } },
                "assumptions": { "type": "array", "items": { "type": "string" } },
                "unresolved_questions": { "type": "array", "items": { "type": "string" } },
                "acceptance_criteria": { "type": "array", "items": { "type": "string" } },
                "project_files": { "type": "array", "items": { "type": "string" } },
                "workflow": { "type": "string", "description": "Optional workflow name" }
            }),
            &["intent_kind", "complexity", "summary"],
        )
    }

    fn key(&self) -> ToolKey {
        ToolKey::native("submit_contract")
    }

    fn permission(&self, _args: &Value, _ctx: &ToolContext) -> Result<ToolPermission, ToolError> {
        Ok(ToolPermission::new(vec![Scope::exact("contract")]))
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let intent_kind = parse_intent(&required_str(&args, "intent_kind")?)?;
        let complexity = Complexity::parse(&required_str(&args, "complexity")?)
            .ok_or_else(|| ToolError::InvalidArgs("invalid complexity".into()))?;
        let summary = required_str(&args, "summary")?;
        let existing = self.runtime.contract();
        let mut contract =
            TaskContract::new(&self.runtime.session_id, intent_kind, complexity, summary);
        if let Some(existing) = existing {
            contract.contract_id = existing.contract_id.clone();
            contract.contract_version = existing.contract_version + 1;
            contract.created_at = existing.created_at;
        }
        contract.intent.goals = string_list(&args, "goals");
        contract.intent.requirements = string_list(&args, "requirements");
        contract.intent.non_goals = string_list(&args, "non_goals");
        contract.constraints = string_list(&args, "constraints");
        contract.decisions = string_list(&args, "decisions");
        contract.assumptions = string_list(&args, "assumptions");
        contract.unresolved_questions = string_list(&args, "unresolved_questions");
        contract.acceptance_criteria = string_list(&args, "acceptance_criteria");
        contract.references.project_files = string_list(&args, "project_files");
        contract.workflow = args
            .get("workflow")
            .and_then(Value::as_str)
            .unwrap_or_else(|| WorkflowKind::from_contract(&contract).name())
            .to_string();
        if !contract.unresolved_questions.is_empty() {
            contract.status = ContractStatus::NeedsClarification;
        } else {
            let policy = self.runtime.config.workflow.require_spec_approval;
            let auto_approve = match policy {
                kudelma_config::ApprovalPolicy::Never => true,
                kudelma_config::ApprovalPolicy::RiskBased => !complexity.requires_approval(),
                kudelma_config::ApprovalPolicy::Always => false,
            };
            if auto_approve {
                contract.approve().map_err(ToolError::Other)?;
            } else {
                contract.status = ContractStatus::ReadyForLead;
            }
        }
        contract.validate().map_err(ToolError::Other)?;
        self.runtime
            .set_contract(contract.clone())
            .map_err(ToolError::Other)?;
        self.runtime
            .set_workflow_kind(WorkflowKind::from_contract(&contract));
        let mut text = contract.render();
        if contract.needs_approval() {
            text.push_str(&format!(
                "\nContract is ready but needs developer approval: /contract approve {}\n",
                contract.contract_id
            ));
        } else if contract.status == ContractStatus::Approved {
            text.push_str("\nContract approved. Lead may take over.\n");
        } else {
            text.push_str(
                "\nContract needs clarification; ask the developer the unresolved questions.\n",
            );
        }
        let _ = ctx;
        Ok(ToolOutput::success(text))
    }
}

fn parse_intent(raw: &str) -> Result<IntentKind, ToolError> {
    Ok(match raw {
        "general_question" => IntentKind::GeneralQuestion,
        "code_question" => IntentKind::CodeQuestion,
        "architecture_question" => IntentKind::ArchitectureQuestion,
        "feature_request" => IntentKind::FeatureRequest,
        "bug_report" => IntentKind::BugReport,
        "change_request" => IntentKind::ChangeRequest,
        "planning_request" => IntentKind::PlanningRequest,
        "status_question" => IntentKind::StatusQuestion,
        other => {
            return Err(ToolError::InvalidArgs(format!(
                "unknown intent kind `{other}`"
            )));
        }
    })
}

fn string_list(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub fn register_lead_tools(
    registry: &kudelma_tools::ToolRegistry,
    runtime: Arc<RuntimeState>,
    role: AgentRole,
) {
    registry.register(Arc::new(SkillTool {
        runtime: runtime.clone(),
        role,
    }));
    if role == AgentRole::Lead {
        registry.register(Arc::new(LeadModeTool {
            runtime: runtime.clone(),
        }));
    }
    if role == AgentRole::Lead {
        registry.register(Arc::new(VerifyTool {
            runtime: runtime.clone(),
        }));
        registry.register(Arc::new(ReviewTool {
            runtime: runtime.clone(),
        }));
    }
    if matches!(role, AgentRole::Agency | AgentRole::Lead) {
        registry.register(Arc::new(ExploreCodebaseTool {
            runtime: runtime.clone(),
            role,
        }));
    }
    if role == AgentRole::Agency {
        registry.register(Arc::new(SubmitContractTool { runtime }));
    }
}
