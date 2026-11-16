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
- `crates/kudelma-core` — agent loop, sessions, compaction, subagents, MCP, hooks.
- `crates/kudelma-tui` — ratatui interface.
- `crates/kudelma-acp` — ACP stdio server.
- `src/` — CLI commands and headless mode.

## Invariants

- Conversation history is append-only; do not rewrite earlier messages except at
  an explicit compaction/rewind boundary. Tool-use blocks must always be closed.
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
