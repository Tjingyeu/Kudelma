# AGENTS.md

Kudelma is a Rust workspace implementing a terminal AI coding agent. Keep the
agent loop, provider adapters and UI boundaries intact when changing behavior.

## Commands

```bash
cargo check --workspace --all-targets
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release
```

## Layout

- `crates/kudelma-config` — layered TOML config, permission rules, MCP/hooks.
- `crates/kudelma-storage` — XDG paths, atomic writes, sessions, auth, trust, memory.
- `crates/kudelma-providers` — Anthropic / OpenAI-compatible / Gemini streaming clients.
- `crates/kudelma-permissions` — permission decisions, Tree-sitter bash scopes, path boundaries.
- `crates/kudelma-tools` — `WorkspaceEnv` plus built-in tools (`index`, `read`, `edit`, `bash`, ...).
- `crates/kudelma-core` — orchestration: Agency/Lead/Coder, shared deterministic exploration,
  Lead modes, Task Contracts, Context Packs, Skills, Workflows, Verifier, MCP, hooks.
- `crates/kudelma-tui` — ratatui interface with role/mode/gate, contract and skill visibility.
- `crates/kudelma-acp` — ACP stdio server.
- `src/` — CLI commands and headless mode.

## Invariants

- There are three LLM roles: Agency (intent/routing), Lead (technical authority), Coder
  (scoped implementation). Verifier is deterministic.
- Exploration is a shared read-only capability (`crates/kudelma-core/src/exploration/`), not
  an agent and never a model call. Agency and Lead interpret its evidence with their own model.
- PLAN/BUILD/DEBUG/REVIEW are modes of one Lead session, never separate agents.
- PLAN and REVIEW are enforced in `kudelma-permissions` before `--yolo`. PLAN requires a real
  plan artifact before leaving; REVIEW requires a structured `ReviewReport` bound to the current
  workspace revision.
- Coder has no Bash/Verify by default. Enabling `agents.coder.allow_shell` removes the file-write
  isolation guarantee and must not be described as a hard sandbox.
- Verification evidence is bound to `WorkspaceRevision`; code or workspace changes invalidate it.
- Complex shell, redirection and outside-workspace cwd require exact approval; `--yolo` and
  `--allowed-tools bash` do not bypass exact approval.
- Pre-tool hooks are fail-closed: only exit code 0 allows the tool.
- Agency and Lead histories are separate; lead receives a Context Pack, not the Agency transcript.
- Only Lead may dispatch Coder. Agency may invoke shared deterministic exploration read-only.
- Skills and Workflows never grant permissions; `kudelma-permissions` remains the final authority.
- Completion requires real `ExecutionEvidence`; code changes invalidate a passed verification gate.
- Conversation history is append-only; do not rewrite earlier messages except at an
  explicit compaction/rewind boundary. Tool-use blocks must always be closed.
- Model-visible file access goes through `WorkspaceEnv`; host state uses `StateDir`.
- Permissions are fail-closed. `deny` wins, untrusted projects cannot widen access,
  and shell redirections require exact approval. Never bypass `PermissionManager`
  in a tool.
- Untrusted project config must not change providers, hooks or MCP servers.
- Serialized session/message formats are forward-compatible: add fields with
  defaults and keep `ContentBlock::Unknown`.

## Style

- Prefer small modules and explicit errors (`thiserror`); no panics on user input.
- No comments that restate the code. Comment money/security invariants only.
- Add tests beside the implementation or in `crates/*/tests/`.
