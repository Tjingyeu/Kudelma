# Kudelma Architecture Implementation Map

This document maps the baseline in [`KUDELMA_AGENT_WORKFLOW_SPEC.md`](./KUDELMA_AGENT_WORKFLOW_SPEC.md) (KUD-ARCH-001 v1.1, three-model design) to the Rust workspace.

## Roles and capabilities

| Spec component | Implementation |
| --- | --- |
| Agency | `crates/kudelma-core/src/agents/agency.rs`, `runtime_tools.rs::SubmitContractTool`, `ExploreCodebaseTool` |
| Lead | `crates/kudelma-core/src/agent.rs`, `lead/mod.rs`, `runtime_tools.rs::LeadModeTool` |
| Shared exploration capability | `crates/kudelma-core/src/exploration/` (request, deterministic executor, evidence), exposed as the `explore_codebase` tool |
| Coder | `crates/kudelma-core/src/subagent.rs` (`task` with `subagent_type=coder`), role-scoped permissions |
| Verifier | `crates/kudelma-core/src/verification/` |

Exploration is **not** an agent, model profile, or session. `exploration::executor` performs deterministic retrieval (Tree-sitter outlines from `kudelma-tools`, file walking, literal/identifier matching, recent evidence and project memory) and returns an `ExplorationReport`. Agency or Lead interprets it with its own model.

Agency and Lead histories are separate inside one session record (`StoredSession.messages` and
`StoredSession.agency_messages`). Lead receives a `ContextPack`, not the Agency transcript.

## Communication contracts

- `orchestration/contract.rs` — versioned `TaskContract`, status and approval.
- `orchestration/context.rs` — `ContextPack` with confirmed fact / observed evidence /
  assumption / unknown labels.
- `exploration/request.rs` — `ExplorationRequest`, `Caller` (`agency | lead`),
  `InvestigationMode`, scope and limits.
- `exploration/evidence.rs` — `ExplorationReport`, findings with `file:symbol:lines`,
  relationships, coverage and unknowns.
- `orchestration/worker.rs` — `WorkerTask` / `WorkerReport`.
- `orchestration/ledger.rs` — `ExecutionLedger` (role turns/tokens/cost, loaded skills,
  worker outcomes, acceptance).

## Modes and transitions

`lead/mod.rs` defines `LeadMode::{Plan, Build, Debug, Review}`. `runtime.rs` owns the current
mode and records every transition with a reason into `WorkflowState`. Switching mode never
creates a new Lead session.

## Skills

`skills/parser.rs` parses `SKILL.md` frontmatter and hashes contents. `skills/registry.rs`
loads built-ins from `crates/kudelma-core/assets/skills/`, then user skills
(`~/.config/kudelma/skills`) and project skills (`.kudelma/skills`). Only metadata applicable to
the current role/mode is injected; full instructions load through the `skill` tool. Untrusted
project skills are marked `ProjectUntrusted` and cannot load.

## Workflows and gates

`workflows/mod.rs` defines workflow kinds, stages, mode transition history and gates.
`runtime.rs::completion_blocker` enforces contract/plan/design/review/verification gates.
`verification/runner.rs` records command, cwd, timestamps, exit code, Git HEAD, diff hash,
changed files and output. Code edits invalidate a previously passed verification gate.

## Permissions

`kudelma-permissions` has three roles: `AgentRole::{Agency, Lead, Coder}` plus `CoderScope`.
Role restrictions are evaluated before `--yolo`; Coder writes are checked against
`allowed_paths` and `forbidden_paths`; deny rules remain highest priority. Agency is read-only.
Agency and Lead may call `explore_codebase`; Coder cannot dispatch it. Untrusted projects still
contribute deny rules but cannot change providers, hooks, MCP servers or skills.

## Interfaces

- CLI: `kudelma skill|workflow|task`, `--mode`, `--workflow`, `--direct-lead`.
- TUI: role/mode/workflow/gate status bar, `/mode`, `/contract approve`, `/workflow`,
  `/skills`, `/verify`, `/agency`.
- Headless: same orchestration; contracts are auto-approved for unattended runs.
- ACP: `session/new`, `session/prompt`, `session/update`, `session/cancel`, mode/model updates.
