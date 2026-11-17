# Safety Remediation Status

This document records the implementation status of the safety/workflow repair plan.
It is intentionally explicit about partial and unimplemented items.

## Completed

- [x] FIX-00 baseline: rustc/cargo available; fmt, check, clippy and workspace tests passed before changes.
- [x] FIX-01 Coder shell boundary: Bash/Verify removed from default Coder, denied in permissions before YOLO, `allow_shell` is an explicit unsafe opt-in; Lead Bash `cwd` participates in approval.
- [x] FIX-02 permission modes: `PermissionMode::{Plan, Build, Debug, Review}` in `kudelma-permissions`; Review is read-only except controlled Verify/`submit_review`; Plan writes only the plan file; all three UIs route through the same session mode path.
- [x] FIX-03 exact approval: redirection/unparsable shell and outside-workspace Bash cwd require exact approval; `--yolo` and blanket `--allowed-tools bash` do not bypass it; headless AutoApprover rejects `requires_exact`.
- [x] FIX-04 hooks: pre-tool is fail-closed for exit 2, other non-zero exits, launch failure and cancellation; post/session-end failures produce visible diagnostics without changing tool results.
- [x] FIX-05 contract state machine: only `ReadyForLead` can be approved; unresolved questions block approval; `ContractStore::update` clears approval and requires re-approval; tests cover both paths.
- [x] FIX-06 ToolPolicy: shared allowed/disallowed policy filters schema, execution and deferred `tool_search`; Agency/Lead/Coder use it.
- [x] FIX-07 WorkspaceRevision: evidence stores revision before/after; Git tracked diff, status, untracked content and mutation generation are included; non-Git uses a bounded file fingerprint; completion rejects evidence from another cwd/revision or after mutation; background Bash tasks block final acceptance.
- [x] FIX-08 plan artifact: leaving PLAN requires a non-empty plan file with verification; L3 requires `## Design` and `## Risks`; plan hash is recorded and modifications invalidate the gate.
- [x] FIX-09 ReviewReport: versioned structured report, Lead/Review-only `submit_review`, evidence + acceptance-criterion validation, current revision/contract binding; Review mode alone no longer passes; changes_requested/blocked keep Finish blocked.
- [x] FIX-10 stable prompt prefix: Lead/Agency system prompts are stable; mode, contract, workflow, skills, memory, instructions and date are appended as context updates; tests assert stable prompts exclude dynamic state.
- [x] FIX-11 budgets and Coder turns: Agency/Lead/Coder check budgets before and after usage; Coder uses `agents.coder.max_turns`; blocked turns are not accepted.
- [x] FIX-12 config honesty: inline strategy disables Task; `review_after_each_task` reopens Review; unsupported/reserved values produce `doctor` warnings.
- [x] FIX-13 WorkerReport: report records actual write paths, before/after revisions and timestamps; tests remain empty unless actually run; no whole-repository dirty status attribution.
- [x] FIX-15 CI: stable matrix on Ubuntu/macOS; MSRV job; fmt/check/clippy/test.
- [x] FIX-16 documentation: README, AGENTS and implementation map describe the new permission, revision, review and reserved-config behavior.

## Partial / deferred

- [ ] FIX-08 `workflow.require_plan_approval = always` does not yet have a dedicated developer approval UI. Plan artifacts are validated, and `doctor` reports the limitation.
- [ ] FIX-10 `tool_search` does not yet have a named ToolSchemaFrame boundary; dynamic tool schema changes remain an explicit cache exception.
- [ ] FIX-11 Agency clarification budget remains advisory rather than a hard structured stop.
- [ ] FIX-12 `max_parallel_coders`, `fresh_coder_per_task=false` and `exploration.max_investigation_rounds` are reserved; `doctor` warns, but no execution behavior is claimed.
- [ ] FIX-14 exploration and project-instruction loading still use local filesystem access; remote `WorkspaceEnv` and a remote RevisionProvider are not implemented.
- [ ] FIX-15 no `cargo audit`/`cargo deny`, and the Mock SSE end-to-end scenario is still a local/manual harness rather than a committed CI test.

## Verification

Run from the repository root:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release
```

Latest run: 98 tests passed; release build succeeded.

Manual Mock SSE scenario verified:

- Agency -> Task Contract -> Lead PLAN
- Plan artifact write -> PLAN -> BUILD
- deterministic Verify with `WorkspaceRevision`
- BUILD -> REVIEW -> structured `submit_review`
- Finish/acceptance only after matching revision and verification evidence
