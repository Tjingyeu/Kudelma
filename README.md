# Kudelma

> 一款用 Rust 编写的高性能终端 AI 编程智能体（AI Coding Agent）。单二进制、快速启动、低 Token 消耗，并带有三模型协作、共享确定性探索、工程化 Skills/Workflow、确定性验证证据、安全权限、MCP 与 ACP 集成。

Kudelma 的架构参考了 [tontinton/maki](https://github.com/tontinton/maki)、[anomalyco/opencode](https://github.com/anomalyco/opencode) 与 [MoonshotAI/kimi-code](https://github.com/MoonshotAI/kimi-code)，并实现了 [`KUDELMA_AGENT_WORKFLOW_SPEC.md`](docs/architecture/KUDELMA_AGENT_WORKFLOW_SPEC.md) 定义的角色与工作流架构。早期基础设计见 [`PLAN.md`](./PLAN.md)。

---

## 特性

### 三模型协作（Agency / Lead / Coder）+ 共享确定性探索

- **Agency**：面向开发者的意图代理。普通问题直接回答；代码问题调用共享只读 **Exploration Capability** 并返回带 `file:symbol:line` 的证据，**不唤醒 Lead、也不启动额外模型**；开发请求经过最小充分澄清后生成版本化 **Task Contract**。
- **Lead**：唯一技术决策与编排权威，在同一会话内切换 **PLAN / BUILD / DEBUG / REVIEW** 四种模式，可内联实现或派遣 Coder；直接复用同一套确定性探索能力做深度调查。
- **共享 Exploration Capability**：不是 Agent、没有独立模型或会话。确定性地组合文件发现、Tree-sitter 符号骨架、关键字/引用检索、源码片段、项目记忆与最近运行证据，返回区分 observed fact / inference 的 `ExplorationReport`；由调用方（Agency 或 Lead）用自身模型解释。
- **Coder**：受限路径与工具的实现 Worker，不能派生子 Agent，返回结构化 Worker Report。
- **Verifier**：非 LLM 确定性子系统。执行 build/test/lint/验收命令，记录命令、工作目录、退出码、起止时间、Git HEAD、diff 哈希与输出日志，生成不可由模型伪造的 `ExecutionEvidence`。

### 工程工作流与 Skills

- **Skills 引擎**：解析 `SKILL.md` Frontmatter，内置 12 个工程技能（intent-refinement、writing-plans、executing-plans、TDD、systematic-debugging、code review、verification 等）；按角色/模式匹配，**按需懒加载**，未信任项目 Skill 默认隔离。
- **Workflows 与硬门禁**：Feature / Bug Fix / Refactoring / Simple Task / Codebase Question 工作流；Contract、Plan、Design、Review、Verification、Finish 等 Gate 由运行时检查，模型不能只靠文字宣称完成。
- **Task Contract**：版本化、可审批、保留历史版本；未解决关键问题时不得标记为 approved。
- **Context Pack**：Agency -> Lead、Lead -> Coder 传递最小任务上下文，不复制完整聊天历史，并区分 `confirmed fact` / `observed evidence` / `assumption` / `unknown`。
- **Cost per Accepted Task**：Execution Ledger 记录每个角色的 Turn/Token/成本、加载的 Skill 哈希、Worker 成功率与返工次数；支持任务/角色成本与 wall-clock 预算。

### 其他能力

- **原生 Rust 单二进制**：无 Node.js / Bun 运行时，毫秒级启动；`ratatui` 交互式 TUI。
- **多 Provider**：Anthropic Messages、OpenAI Chat Completions 兼容协议、Google Gemini。
  - 内置 Anthropic、OpenAI、Moonshot Kimi、DeepSeek、Gemini、OpenRouter、xAI、Ollama 预设，支持任意 OpenAI 兼容端点。
  - 流式输出、SSE、`Retry-After` 重试、Prompt Cache 断点、Token / 成本核算。
- **省 Token 的工具链**：
  - `index`：Tree-sitter 提取 Rust / TS / TSX / JS / Python / Go / C / C++ 的符号骨架与行号，先定位再精读。
  - `read` 分页读取、`grep` / `glob` 遵循 `.gitignore`、`edit` 精确替换并返回 diff。
  - `bash` 保留长输出首尾、完整日志落盘、进程组超时清理、后台任务回执。
  - `todo_write`、`memory`、带 SSRF 防护的 `webfetch`。
- **安全权限模型**：
  - `deny` 优先；工作区读与写默认允许，工作区外询问；`bash` 默认询问。
  - 三模型能力矩阵由 Rust 权限层强制：Agency 只读，Coder 仅限授权路径，Lead 可编排但不能绕过 deny；`--yolo` 不绕过这些硬边界。
  - Tree-sitter 拆解复合 Shell 命令（`git status && rm -rf /` → 两个作用域），重定向/替换等高风险形式要求精确批准。
  - Plan 模式只允许写计划文件；Review 模式默认只读并依赖 Verifier 证据。
  - 未信任目录只生效 `deny` 规则，不加载项目 MCP / hooks / Skills。
- **会话与上下文**：
  - Agency 与 Lead 会话历史分轨持久化；恢复会话时保留角色、模式、契约和工作流状态。
  - 会话自动保存到 XDG state 目录，支持 `-c` / `-r` 恢复、导出/导入、跨进程锁。
  - 自动上下文压缩（阈值可配），超长工具输出首尾保留。
  - `Esc` 中断保留已生成内容；`Esc Esc` 回滚到历史 Prompt。
- **MCP**：stdio 与 Streamable HTTP MCP 客户端；工具过多时通过 `tool_search` 延迟注入 Schema。
- **ACP**：`kudelma acp` 提供 Agent Client Protocol stdio 服务，可在 Zed 等编辑器中直接使用。
- **Headless / SDK**：
  - `kudelma -p "..."` 单轮执行；默认走 Agency 路由，`--direct-lead` 可跳过。
  - `--output-format text|json|stream-json`。
  - `--input-format stream-json` 从 stdin 持续接收用户消息。

---

## 安装

需要 Rust 1.85+。

```bash
git clone https://github.com/Tjingyeu/Kudelma.git
cd Kudelma
cargo install --path . --locked
```

或本地构建：

```bash
cargo build --release
./target/release/kudelma --help
```

---

## 快速开始

```bash
# 设置 API Key（任选其一）
export ANTHROPIC_API_KEY=...
# 或写入 ~/.local/state/kudelma/auth.json (0600)
kudelma auth login anthropic

# 查看可用模型与凭据状态
kudelma models

# 在当前项目启动 TUI（默认先由 Agency 理解意图）
cd your-project
kudelma

# 直接进入 Lead（高级用法）
kudelma --direct-lead

# 指定 Lead 初始模式 / 工作流
kudelma --mode plan
kudelma --workflow bug_fix

# 查看 Skills 与工作流
kudelma skill list
kudelma workflow list
kudelma task history

# 指定模型
kudelma -m moonshot/kimi-k2.5

# 非交互单轮
kudelma -p --yolo "解释这个项目的目录结构"

# 结构化输出
kudelma -p --yolo --output-format stream-json "修复失败的单元测试"

# 恢复最近会话 / 指定会话
kudelma -c
kudelma -r <SESSION_ID>

# 只读规划模式
kudelma --plan
```
> Headless 模式默认**拒绝**权限询问（避免无人值守时静默放行）。需要执行命令或写工作区外文件时加 `--yolo`，或用 `--allowed-tools` 精确放行。Headless 会为自动化流程自动批准 Task Contract；TUI 中需用 `/contract approve` 确认高风险契约。

---

## 配置

全局配置：`~/.config/kudelma/config.toml`（`KUDELMA_CONFIG_DIR` 可覆盖）。
项目配置：`<project>/.kudelma/config.toml`；项目权限：`<project>/.kudelma/permissions.toml`。

`kudelma config init` 会生成模板。

> 升级提示：v1.1 已移除 `[agents.explorer]` 模型配置；只读探索现在由 `[exploration]` 控制，确定性执行，不产生额外模型调用。

```toml
default_model = "anthropic/claude-sonnet-4-5"

[ui]
theme = "system"          # system | dark | light
mouse = true

[agent]
max_turns = 50
compaction_percent = 80
bash_timeout_secs = 120
mcp_defer_threshold = 15

[agency]
max_turns = 10
max_clarification_rounds = 8

[lead]
max_turns = 40
max_children = 4

[agents.coder]
max_turns = 25

# 共享只读探索：确定性检索，没有独立模型。
[exploration]
enabled = true
read_only = true
prefer_deterministic_retrieval = true
max_files = 30
max_results = 100
max_investigation_rounds = 5
allow_external_fetch = true

[workflow]
default = "adaptive"          # adaptive | feature | bug_fix | refactor | simple
require_review = true
require_verification = true
require_spec_approval = "risk_based"
require_plan_approval = "risk_based"
verification_commands = ["cargo test --workspace"]

[workflow.execution]
strategy = "adaptive"
max_parallel_coders = 3
fresh_coder_per_task = true

[skills]
enabled = true
lazy_loading = true
project_skills_require_trust = true

[budgets]
task_cost_usd = 0.0
task_wall_clock_secs = 0

[permissions]
default = "prompt"

[[permissions.rules]]
tool = "bash"
scope = "cargo *"
effect = "allow"

# 自定义 OpenAI 兼容 Provider
[providers.my-gateway]
kind = "openai"
base_url = "https://gateway.example.com/v1"
api_key_env = "MY_GATEWAY_KEY"

[[providers.my-gateway.models]]
id = "my-model"
context_window = 128000
max_output_tokens = 8192
input_cost = 1.0
output_cost = 3.0
tier = "medium"

# MCP Server
[mcp.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "."]

[mcp.docs]
url = "https://mcp.example.com/mcp"

[hooks]
# pre_tool_use = "scripts/pre-tool.sh"   # 退出码 2 表示拒绝
# post_tool_use = "scripts/post-tool.sh"
# session_end = "scripts/session-end.sh"
```

### 项目信任

项目级配置、hooks 与 MCP 只在目录被信任后加载：

```bash
kudelma trust add          # 当前目录
kudelma trust list
kudelma --trust            # 仅本次运行信任
```

未信任目录仍会读取 `deny` 权限规则（只能收紧，不能放宽）。

---

## 权限

| 行为 | 默认 |
| --- | --- |
| Agency 写文件、执行 Shell、派发 Coder、调用 MCP | 总是拒绝（`--yolo` 也不行） |
| Agency / Lead 使用共享探索读取工作区外路径 | 询问 |
| Coder 调用 `explore_codebase` | 总是拒绝 |
| Coder 写 `allowed_paths` 之外或 `forbidden_paths` 内 | 总是拒绝 |
| `read` / `glob` / `grep` / `ls` / `index`（工作区内） | 允许 |
| `write` / `edit`（工作区内） | 允许 |
| 工作区外读写 | 询问 |
| `bash`、`webfetch`、MCP 工具 | 询问 |
| Plan 模式写非计划文件 / 执行命令 / MCP | 询问 |
| 显式 `deny` 规则 | 总是拒绝，`--yolo` 也不例外 |

`bash` 命令会先经过 Tree-sitter 拆解为独立作用域；含重定向、`$()`、反引号等无法静态判断影响面的命令，只能通过**精确字符串**规则批准，`echo *` 这类通配符不会放行 `echo hi > /etc/passwd`。

TUI 权限弹窗：

- `y` 允许一次
- `a` 本会话允许
- `p` 当前项目永久允许（需信任）
- `g` 全局永久允许
- `n` / `Esc` 拒绝

---

## TUI 快捷键

| 按键 | 作用 |
| --- | --- |
| `Enter` | 发送 |
| `Shift+Enter` / `Alt+Enter` / `Ctrl+J` | 换行 |
| `Tab` | 补全 `/命令` 或 `@文件` |
| `Esc` | 中断当前回合；空输入时再次 `Esc` 打开回滚 |
| `Ctrl+B` | 将当前长工具移到后台 |
| `Ctrl+X` | 任务 / 子 Agent 窗口 |
| `Ctrl+T` | 展开/折叠 Thinking |
| `Ctrl+O` | 展开/折叠完整工具输出 |
| `PageUp` / `PageDown` | 滚动 |
| `Ctrl+C` | 退出 |

内置命令：`/help`、`/new`、`/sessions`、`/resume`、`/model`、`/mode`、`/contract`、`/workflow`、`/skills`、`/verify`、`/compact`、`/usage`、`/tasks`、`/rewind`、`/clear`、`/cd`、`/init`、`/quit`。

TUI 顶部状态栏显示：当前角色（`agency` / `lead`）、Lead 模式（`PLAN/BUILD/DEBUG/REVIEW`）、模型、工作流阶段与 Gate 进度、上下文占用和成本。发生迁移时显示原因，例如 `BUILD -> DEBUG: verification failed`。

- `/mode` 无参数循环切换，`/mode debug` 直接切换。
- `/contract` 查看当前契约，`/contract approve` 在 TUI 中批准高风险契约。
- `/verify cargo test` 直接运行确定性验证并记录证据。

---

## CLI

```text
kudelma [OPTIONS] [PROMPT]
kudelma <COMMAND>

Commands:
  auth     管理凭据 (login/logout/status)
  models   列出模型、上下文与价格
  session  会话列表/删除/导出/导入
  skill    技能列表/检视/启用/禁用
  workflow 工作流预设、运行状态、恢复
  task     Worker Task/Report、Exploration Report 历史
  index    Tree-sitter 符号骨架
  trust    管理受信任目录
  prompt   打印 System Prompt 或工具定义
  acp      ACP stdio 服务
  config   path/show/init
  mcp      列出 MCP Server
  doctor   环境与配置诊断
```

常用参数：`-p`、`-m`、`-c`、`-r`、`--yolo`、`--trust`、`--plan`、`--mode plan|build|debug|review`、`--workflow adaptive|feature|bug_fix|refactor|simple`、`--direct-lead`、`--output-format`、`--input-format`、`--max-turns`、`--system-prompt`、`--append-system-prompt`、`--allowed-tools`、`--disallowed-tools`、`--verbose`。

### Skills / Workflow / Task 检视

```bash
kudelma skill list --role lead --mode build
kudelma skill inspect test-driven-development
kudelma skill disable systematic-debugging

kudelma workflow list
kudelma workflow inspect <ID>
kudelma workflow resume <ID>

kudelma task history
kudelma task inspect <TASK_OR_REPORT_ID>
```

### 会话导出 / 导入

```bash
kudelma session list
kudelma session export <ID> > session.json
kudelma session import session.json
```

### ACP（Zed 等编辑器）

```json
{
  "agent_servers": {
    "Kudelma": {
      "type": "custom",
      "command": "kudelma",
      "args": ["acp"]
    }
  }
}
```

首次使用前执行一次 `kudelma auth login <provider>`，ACP 会复用已保存凭据。

---

## 架构

```text
kudelma (src/)
 ├── cli.rs / cmd.rs / headless.rs
 └── crates/
     ├── kudelma-config      分层配置、角色/工作流/Skills、权限规则、MCP/Hook
     ├── kudelma-storage     XDG 路径、原子写入、会话、契约/工作流/证据账本
     ├── kudelma-providers   Anthropic / OpenAI-compatible / Gemini 流式客户端
     ├── kudelma-permissions 角色能力矩阵、Coder 路径沙箱、Bash 拆解、路径边界
     ├── kudelma-tools       WorkspaceEnv + 内置工具（index/read/edit/bash/...）
     ├── kudelma-core
     │    ├── agents/        Agency（Coder 复用 task worker）
     │    ├── exploration/   共享只读确定性检索与证据报告（无模型、无会话）
     │    ├── lead/          PLAN / BUILD / DEBUG / REVIEW 模式
     │    ├── orchestration/ TaskContract / ContextPack / WorkerTask / Ledger
     │    ├── skills/        SKILL.md 解析、注册、懒加载、信任隔离
     │    ├── workflows/     工作流状态、阶段与 Gate
     │    ├── verification/  Verifier、ExecutionEvidence、Gate 状态
     │    └── runtime.rs     共享角色/模式/契约/工作流/账本状态
     ├── kudelma-tui         ratatui TUI（角色/模式/Gate、契约、Skills、权限弹窗）
     └── kudelma-acp         Agent Client Protocol stdio 服务
```

核心不变量：

- **角色边界**：Agency 只澄清与通信；Lead 是唯一技术决策者；Exploration 是无模型的只读能力；Coder 受限；Verifier 非 LLM。
- **Append-only**：对话只追加，不重写历史前缀，保护 Prompt Cache。
- **环境隔离**：模型可见文件操作经 `WorkspaceEnv`，宿主状态只落 `StateDir`。
- **Fail-closed**：拒绝优先；未信任目录只能收紧权限；带标签的权限响应防止迟到审批误用。
- **证据优先**：代码变更会使旧的验证 Gate 失效；完成声明必须经过当前 diff 的 `ExecutionEvidence`。
- **中断不浪费**：取消时保留已生成内容并闭合悬空工具调用。

---

## 开发

```bash
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

测试覆盖配置合并、原子写入与软链接逃逸、权限规则与 Bash 拆解、工具截断/索引/SSRF、Provider 消息转换、TUI 渲染基础等。测试不依赖真实网络或 API Key。

---

## 已知边界 / 后续方向

- 目前 `WorkspaceEnv` 的实现是本地文件系统；接口已预留 SSH / 容器远端执行。
- 图片输入在协议层已建模，CLI 尚未暴露附件参数。
- Coder 调度当前按任务顺序执行；`max_parallel_coders` 已进入配置与 Ledger，但并行 worktree 编排仍在演进。
- MCP OAuth、Lua 插件热加载与 Web 搜索尚未内置；MCP stdio/HTTP、Skills、Workflow、Hooks 与 ACP 已可用。
- TDD Gate 已建模；当前基线通过 `verify` 的测试/构建证据完成验证，RED/GREEN 的细粒度自动判定可继续增强。
- 模型价格与上下文窗口为内置目录数据，可通过 `[providers.*.models]` 覆盖。

欢迎提交 Issue 与 PR。

## License

MIT
