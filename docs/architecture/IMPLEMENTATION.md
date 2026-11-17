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
creates a new Lead session. `kudelma-permissions::PermissionMode` mirrors these phases:
PLAN and REVIEW are enforced before YOLO; REVIEW is read-only except for controlled verification
and `submit_review`. Leaving PLAN requires a non-empty plan artifact with verification steps;
L3 additionally requires `## Design` and `## Risks` sections.

## Verification and review

`verification/runner.rs` records command, cwd, timestamps, exit code, Git HEAD, diff hash and
output. Every evidence record stores `WorkspaceRevision` before and after execution. Completion
recomputes the revision and rejects passed evidence from another cwd or revision. Any successful
non-read-only tool bumps the mutation generation and invalidates verification/review gates.

`verification/review.rs` defines the structured `ReviewReport`. REVIEW only passes when a report
matches the current contract version and workspace revision, cites verification evidence, and
marks every acceptance criterion as met. `changes_requested` or `blocked` keeps Finish blocked.

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

`kudelma-permissions` has three roles: `AgentRole::{Agency, Lead, Coder}` plus `CoderScope` and
`PermissionMode`. Role and mode restrictions are evaluated before `--yolo`; Coder writes are
checked against `allowed_paths` and `forbidden_paths`; deny rules remain highest priority. Agency
is read-only. Agency and Lead may call `explore_codebase`; Coder cannot dispatch it. Coder has no
Bash or Verify by default; `allow_shell` is an explicit opt-out from file-write isolation.
Complex shell and outside-workspace Bash cwd require exact approval (`requires_exact`) and are
denied by the headless AutoApprover. Pre-tool hooks are fail-closed. Untrusted projects still
contribute deny rules but cannot change providers, hooks, MCP servers or skills.

## Provider connections

`kudelma-providers` ships built-in presets plus user-defined OpenAI-compatible providers.
`AuthStore` holds API keys in the private state file; `/connect` and `kudelma models --connect`
write there. `opencode-go` is a built-in preset for the OpenCode Go subscription:
OpenAI-compatible Chat Completions at
`https://opencode.ai/inference/go/openai/v1`, credential `OPENCODE_API_KEY`, and the documented
Go model catalog. Go requests attach `kudelma/<version>` as the User-Agent and a stable
`x-opencode-session` derived from the session/role. OpenCode Go does not use a separate
transport in this baseline.

## Interfaces

- CLI: `kudelma skill|workflow|task`, `kudelma auth login <provider>`,
  `kudelma models`, `kudelma models --connect`, `--mode`, `--workflow`, `--direct-lead`.
- TUI: role/mode/workflow/gate status bar, `/connect`, `/models`, `/mode`,
  `/contract approve`, `/workflow`, `/skills`, `/verify`, `/agency`.
- Headless: same orchestration; contracts are auto-approved for unattended runs.
- ACP: `session/new`, `session/prompt`, `session/update`, `session/cancel`, mode/model updates.
