# Kudelma 构建计划与架构设计文档 (Build & Architecture Plan)

> **Note:** This document describes the initial v0.1 foundation. The implemented
> four-role / Skills / Workflow architecture follows
> [`docs/architecture/KUDELMA_AGENT_WORKFLOW_SPEC.md`](docs/architecture/KUDELMA_AGENT_WORKFLOW_SPEC.md)
> and [`docs/architecture/IMPLEMENTATION.md`](docs/architecture/IMPLEMENTATION.md).

> **Kudelma** 是一款基于 **Rust** 构建的高性能、低 Token 消耗、安全可扩展的终端 AI 编程智能体（AI Coding Agent CLI / TUI）。
> 本计划深度参考并融合了 [**tontinton/maki**](https://github.com/tontinton/maki)、[**anomalyco/opencode**](https://github.com/anomalyco/opencode) 与 [**MoonshotAI/kimi-code**](https://github.com/MoonshotAI/kimi-code) 三大项目的核心架构设计。

---

## 目录

1. [参考仓库深度剖析与核心借鉴](#1-参考仓库深度剖析与核心借鉴)
2. [核心工程不变量 (Core Engineering Invariants)](#2-核心工程不变量-core-engineering-invariants)
3. [Rust Cargo Workspace 拓扑与模块划分](#3-rust-cargo-workspace-拓扑与模块划分)
4. [核心子系统详细设计](#4-核心子系统详细设计)
   - [4.1 存储与配置层 (`kudelma-storage` & `kudelma-config`)](#41-存储与配置层-kudelma-storage--kudelma-config)
   - [4.2 多模型适配与 Token 经济层 (`kudelma-providers`)](#42-多模型适配与-token-经济层-kudelma-providers)
   - [4.3 安全权限引擎与 Bash AST 审计 (`kudelma-permissions`)](#43-安全权限引擎与-bash-ast-审计-kudelma-permissions)
   - [4.4 高效代码工具链与环境隔离 (`kudelma-tools`)](#44-高效代码工具链与环境隔离-kudelma-tools)
   - [4.5 Agent 核心循环与子 Agent 编排 (`kudelma-core`)](#45-agent-核心循环与子-agent-编排-kudelma-core)
   - [4.6 Ratatui 60FPS 交互式终端界面 (`kudelma-tui`)](#46-ratatui-60fps-交互式终端界面-kudelma-tui)
   - [4.7 外部生态：MCP、Headless SDK 与 ACP 协议 (`kudelma-acp`)](#47-外部生态mcpheadless-sdk-与-acp-协议-kudelma-acp)
5. [CLI 命令体系与运行路径矩阵](#5-cli-命令体系与运行路径矩阵)
6. [分阶段实施路线图 (Phased Roadmap)](#6-分阶段实施路线图-phased-roadmap)
7. [代码规范与测试验证体系](#7-代码规范与测试验证体系)

---

## 1. 参考仓库深度剖析与核心借鉴

### 1.1 `tontinton/maki` —— 原生 Rust 性能与极致 Token 经济
- **架构特点**：
  - 纯 Rust Workspace（`maki-ui`, `maki-agent`, `maki-providers`, `maki-storage`, `maki-config`, `maki-lua`, `maki-acp`, `maki-interpreter` 等 15 个子 Crate）。
  - 单线程 Elm 架构 `ratatui` TUI + 后台 `tokio` 异步任务驱动 Agent，零 JS 运行时开销，毫秒级启动、60 FPS 渲染。
- **核心亮点借鉴**：
  1. **Token Economy（上下文极致省流）**：
     - `index` 工具：在 `read` 大文件前，通过 `tree-sitter` 解析 30+ 种编程语言，提取函数、类、结构体、接口的高层骨架及精确起止行号（如 `lines 150-165`），大幅减少盲目全文读取带来的 65% Token 开销。
     - `tool_search` 延迟注入 MCP 工具：当 MCP Server 暴露上百个工具时，不在每轮请求中全量发送 Schema，而是隐藏在 `tool_search` 背后按需检索激活。
     - Bash 输出首尾保留截断：长日志同时保留开头上下文与末尾报错/测试汇总。
     - 沙箱批量处理 (`code_execution`)：在隔离沙箱中组合调用工具并过滤数据，仅将精简结果送入模型上下文。
  2. **Append-only 请求不变量**：绝不在会话中途修改 `system` 提示词、`tools` 列表或改写历史消息前缀，确保 Anthropic / OpenAI / Moonshot 的 Prompt Cache 100% 命中，并防止 Thinking 签名块失效报错。
  3. **安全模型**：使用 `tree-sitter-bash` 拆解复合 Shell 命令（如识别 `git diff && rm -rf /` 包含 `git *` 与 `rm *` 两个作用域），配合目录信任机制（`Folder Trust`，未信任仓库仅生效 `deny` 规则）与 `TaggedAnswer`（权限响应强制绑定 `request_id`，防止过期审批误放行）。

### 1.2 `anomalyco/opencode` —— 双模式 Agent 与持久化会话状态机
- **架构特点**：
  - 严格的单向依赖分层：`schema` $\rightarrow$ `core` / `protocol` $\rightarrow$ `server` $\rightarrow$ `client` / `tui`。
  - 事件驱动的持久化会话核心 (`Session Core`)。
- **核心亮点借鉴**：
  1. **Build / Plan 双主 Agent 模式**：
     - **Build 模式**：默认全权限开发 Agent，可执行文件增删改与命令运行。
     - **Plan 模式**：只读分析与架构规划 Agent，默认拒绝修改业务代码文件、对 Shell 命令强制询问，仅允许写入当前会话的计划文档，通过 `Tab` 键即可在两种模式间瞬时切换。
  2. **Session Inbox 与 Steer 抢占式投递**：
     - 将用户 Prompt 入队 (`enqueued`) 与模型执行 (`delivered`) 解耦。
     - 支持在模型执行工具间隙的安全步边界 (`safe step boundaries`) 注入用户转向指令 (`steer`) 或优先执行上下文压缩 (`compaction`)。
  3. **Write-Ahead Execution Claim（执行租约与崩溃恢复）**：通过进程级会话认领锁标记活跃会话，支持异常退出后的会话状态恢复與悬空工具调用 (`dangling tool calls`) 闭合。
  4. **语义化主题令牌 (`Semantic Theme Tokens`)**：UI 颜色严格按语义角色（`text.feedback`, `formfield`, `action`）绑定，自适应终端亮暗背景。

### 1.3 `MoonshotAI/kimi-code` —— 作用域生命周期、双环境隔离与 ACP 生态
- **架构特点**：
  - `App / Session / Agent` 三级生命周期作用域架构 (`agent-core-v2`)。
  - 独立的同构渲染数据层 (`transcript`) 与跨环境执行协议 (`remote exec-server`)。
- **核心亮点借鉴**：
  1. **双文件系统与双进程后端隔离不变量 (`WorkspaceEnvironment` vs `HostStorage`)**：
     - **Workspace 层 (`environment.fs` / `environment.process`)**：所有项目文件读写、`AGENTS.md` 发现、工具代码搜索、工作区 Shell 命令均通过当前 Agent 持有的 `Environment` 句柄访问。这使得本地目录执行与远程 SSH / Docker 容器执行可以无缝切换。
     - **Host 层 (`StateDir`)**：仅用于宿主本机的会话存储、凭据管理、日志与全局配置，绝不与工作区文件系统混用。
  2. **L1-L4 Transcript 增量流式视图架构**：
     - 将会话输出抽象为 `Turn -> Block -> Delta` 三级增量操作流，使 TUI、Headless `stream-json` 与 ACP 协议共享同一套无 UI 框架耦合的渲染状态机。
  3. **专用实现子 Agent (`coder`)** 与 **生命周期 Hooks**：支持在工具调用前后 (`PreToolUse` / `PostToolUse`) 及会话结束 (`SessionEnd`) 挂载本地拦截/审计命令。原 `explore` / `plan` 子 Agent 已在 v1.1 架构中分别收敛为共享确定性探索能力与 Lead 模式。
  4. **原生 ACP (`Agent Client Protocol`) 支持**：通过 `kudelma acp` 提供 stdio NDJSON 服务，直接嵌入 Zed、JetBrains、VS Code 等现代 IDE。

---

## 2. 核心工程不变量 (Core Engineering Invariants)

在 Kudelma 的所有 Crate 实现中，必须严格遵守以下五条不可违背的工程约束：

1. **Append-Only Context Invariant（只追加上下文不变量）**
   - 一旦会话开始，发送给 LLM 的历史消息序列、System Prompt 和工具定义列表严格只增不改。
   - 运行时动态变化（如切换 `Build`/`Plan` 模式、工作目录变更、`AGENTS.md` 增量加载、后台任务完成回执）必须封装为标准的 `ContentBlock::ContextUpdate` 追加在最新消息帧中。
   - 每个 Turn 执行前通过契约测试函数 `assert_append_only(&prev_request, &curr_request)` 验证前缀一致性。

2. **Dual-Environment Isolation Invariant（工作区与宿主双环境隔离不变量）**
   - 任何工具（`read`, `write`, `edit`, `glob`, `grep`, `index`, `bash`）严禁直接调用 `std::fs` 或 `tokio::process::Command` 操作路径，必须通过注入的 `Arc<dyn WorkspaceEnv>` 接口访问。
   - `HostStateDir` 仅服务于 `~/.local/state/kudelma` 和 `~/.config/kudelma`。

3. **Fail-Closed Permission & Untagged Drop Invariant（权限默认安全与标签校验不变量）**
   - 权限检查顺序固定为：`Explicit Deny` $\rightarrow$ `YOLO (非 Plan 下的 Native 工具)` $\rightarrow$ `Allow Rules` $\rightarrow$ `Plan File Write` $\rightarrow$ `Default Effect / Prompt`。任何规则中的 `Deny` 拥有最高一票否决权。
   - 权限审批通道中的所有用户响应必须携带请求唯一标识 `TaggedAnswer { request_id, answer }`；ID 不匹配的迟到响应一律丢弃并记录告警，严禁挪用至后续工具调用。
   - 未获信任的项目目录（Untrusted Folder）仅提取 `.kudelma/permissions.toml` 中的 `deny` 规则，忽略一切 `allow` 规则、`.env` 与项目级插件/MCP 配置。

4. **Forward-Compatible Serialization Invariant（持久化格式前向兼容不变量）**
   - `Message`、`ContentBlock`、`SessionMeta` 等落盘结构体的 JSON/JSONL 序列化格式视为公共 API。
   - 字段只能新增且必须带 `#[serde(default)]`；枚举新增变体时，`ContentBlock` 必须保留 `#[serde(other)] Unknown` 兜底变体，确保旧版本二进制读取新版本会话文件时不丢失整条消息。

5. **Non-Wasteful Interruption Invariant（中断零浪费不变量）**
   - 用户按下 `Esc` 取消正在执行的流式响应或长耗时工具（如 `bash`、`grep`、子 Agent `task`）时，已收集到的标准输出/流式文本必须附带 `[interrupted by user — partial output]` 标记写入 `ToolResult`，同时确保所有悬空的 `ToolUse` 块闭合，使模型在下一轮对话中直接基于已有输出继续，而非重头执行。

---

## 3. Rust Cargo Workspace 拓扑与模块划分

```text
Kudelma/
├── Cargo.toml                       # Workspace 根配置 (Edition 2024, Resolver 3)
├── PLAN.md                          # 架构与构建计划文档
├── README.md                        # 项目说明文档
├── src/                             # 主二进制 crate: `kudelma`
│   ├── main.rs                      # 进程入口、ExitOnPanic 守卫、Telemetry 刷盘
│   ├── cli.rs                       # Clap 参数定义、ScopedFlag 路径校验矩阵
│   ├── cmd.rs                       # 子命令路由分发
│   ├── headless.rs                  # `--print` 与 SDK `stream-json` 模式运行时
│   └── setup.rs                     # Runtime 组装（Config + Storage + Providers + Env）
└── crates/
    ├── kudelma-config/              # 配置解析、合并与校验
    │   └── src/
    │       ├── lib.rs               # Config, RawConfig, SessionDefaults, Merge 逻辑
    │       ├── permissions.rs       # ToolKey, PermissionRule, Effect, DefaultEffect
    │       ├── project.rs           # 目录信任状态 (TrustMode, ProjectConfig)
    │       └── providers.rs         # 自定义 Provider 与模型路由策略配置
    ├── kudelma-storage/             # 崩溃安全持久化层
    │   └── src/
    │       ├── lib.rs               # StateDir, atomic_write, atomic_write_permissions
    │       ├── paths.rs             # XDG 路径解析、incremental_canonicalize 防软链逃逸
    │       ├── id.rs                # UUIDv7 时间序唯一 ID (KudelmaId)
    │       ├── lock.rs              # SessionClaim 跨进程文件锁与崩溃恢复标记
    │       ├── sessions.rs          # 会话元数据、消息历史 JSONL/Snapshot 存储、导入导出
    │       ├── auth.rs              # API Key / OAuth 令牌安全存储 (Keyring + 0600 文件)
    │       └── plans.rs             # Plan 模式下计划文档路径生成与读写
    ├── kudelma-providers/           # 多模型 LLM 统一协议与流式客户端
    │   └── src/
    │       ├── lib.rs               # Provider Trait, StreamResponse, ProviderEvent
    │       ├── types.rs             # Message, Role, ContentBlock, ThinkingConfig, StopReason
    │       ├── model.rs             # Model, ModelTier (Weak/Medium/Strong), ModelPricing
    │       ├── tokens.rs            # ContextGauge, Token 预估与成本累计核算
    │       ├── retry.rs             # 指数退避重试与 Retry-After 头部解析
    │       └── drivers/
    │           ├── anthropic.rs     # Anthropic Messages API (Prompt Cache + Extended Thinking)
    │           ├── openai.rs        # OpenAI Chat / Responses API 驱动
    │           ├── moonshot.rs      # Moonshot Kimi 专属适配 (Kimi k2 / Thinking / 缓存)
    │           ├── gemini.rs        # Google Gemini API 驱动
    │           └── compatible.rs    # DeepSeek / OpenRouter / Ollama / 自定义兼容端点
    ├── kudelma-permissions/         # 安全权限网关与命令语法分析
    │   └── src/
    │       ├── lib.rs               # PermissionManager, ApprovalGate, TaggedAnswer
    │       ├── bash_ast.rs          # 基于 tree-sitter-bash 的复合命令拆解与作用域提取
    │       └── scope.rs             # 路径与命令 Glob 匹配器 (scope_matches, physical_boundary)
    ├── kudelma-tools/               # 工作区环境抽象与内置工具集
    │   └── src/
    │       ├── lib.rs               # Tool Trait, ToolRegistry, ToolContext, ToolOutput
    │       ├── env.rs               # WorkspaceEnv Trait (LocalEnv & RemoteExecEnv 预留)
    │       └── builtin/
    │           ├── index.rs         # Tree-sitter 代码结构骨架生成工具
    │           ├── read.rs          # 行号分页文件读取 + 读取时间戳登记
    │           ├── write.rs         # 原子写入 + Stale-read 冲突检测 + Diff 生成
    │           ├── edit.rs          # 精确文本替换 + 多处替换 (replace_all) + 空白容错回退
    │           ├── glob.rs          # 遵循 .gitignore 的高速文件路径匹配
    │           ├── grep.rs          # 基于 grep-searcher 的正则/字面量内容检索
    │           ├── bash.rs          # 进程组隔离执行、首尾截断、Ctrl-B 后台化支持
    │           ├── todo.rs          # 会话级结构化任务列表 (todo_write)
    │           ├── memory.rs        # 跨会话项目长期记忆读写
    │           └── webfetch.rs      # 带私有 IP/SSRF 拦截的 HTTP 页面抓取与 Markdown 转换
    ├── kudelma-core/                # Agent 循环、会话协调、MCP 与子 Agent
    │   └── src/
    │       ├── lib.rs               # AgentMode (Build/Plan), AgentInput, AgentEvent
    │       ├── scope.rs             # AppScope -> SessionScope -> AgentScope 生命周期管理
    │       ├── loop_runner.rs       # run_turns 核心步进循环、Steer 抢占、悬空工具修复
    │       ├── compaction.rs        # 自动/手动上下文压缩与 Frame 切换
    │       ├── subagent.rs          # `task` 工具实现（仅 Coder 子 Agent；共享探索见 exploration/）
    │       ├── transcript.rs        # L1-L4 增量 Transcript 状态流 (Turn/Block/Delta)
    │       ├── mcp.rs               # MCP 客户端 (stdio/SSE) 与 `tool_search` 延迟激活器
    │       ├── instructions.rs      # AGENTS.md / CLAUDE.md 层级发现与按需注入
    │       └── hooks.rs             # PreToolUse / PostToolUse / SessionEnd 生命周期钩子
    ├── kudelma-tui/                 # Ratatui 终端用户界面
    │   └── src/
    │       ├── lib.rs               # TerminalGuard, EventLoop, RunOutcome (Exit / Reload)
    │       ├── app.rs               # Elm 架构 App 状态机 (Model + update(Action) -> Command)
    │       ├── theme.rs             # 语义化主题系统与终端亮暗背景探测
    │       ├── markdown.rs          # 流式 Markdown 与 Syntect 语法高亮渲染器
    │       └── components/
    │           ├── chat_view.rs     # 消息流、Thinking 折叠块、内联 Diff、工具状态卡片
    │           ├── input_box.rs     # 多行编辑器、历史记录、@文件补全、/命令补全
    │           ├── status_bar.rs    # 模型、Build/Plan 标签、Token 仪表盘、费用、活跃任务数
    │           ├── permission_modal.rs # 权限请求交互弹窗 (Allow Once/Session/Project/Deny)
    │           ├── picker.rs        # Nucleo 模糊搜索选择器 (文件/会话/模型/Rewind 历史)
    │           └── task_switcher.rs # 子 Agent 与后台任务独立观察窗口 (Ctrl-X)
    └── kudelma-acp/                 # IDE 集成协议服务
        └── src/
            ├── lib.rs               # AcpParams, serve 入口
            ├── server.rs            # NDJSON over stdio JSON-RPC 2.0 消息循环
            └── methods.rs           # initialize, session/new, session/prompt, 权限回调映射
```

---

## 4. 核心子系统详细设计

### 4.1 存储与配置层 (`kudelma-storage` & `kudelma-config`)

1. **XDG 标准目录布局**：
   - 配置目录 (`~/.config/kudelma/`)：`config.toml`、`permissions.toml`、`commands/*.md`。
   - 状态目录 (`~/.local/state/kudelma/`)：
     - `sessions/<session_id>.json`（会话快照与消息流）
     - `sessions/<session_id>.lock`（`SessionClaim` 进程锁文件）
     - `sessions/data/<session_id>/bash/<call_id>.log`（长 Bash 输出完整日志）
     - `plans/<slug>.md`（Plan 模式生成的计划文件）
     - `trusted_folders.json`、`auth.json`（0600 权限）、`history.txt`。
2. **原子写入 (`atomic_write`)**：
   - 在目标文件同目录创建 `NamedTempFile` $\rightarrow$ 写入数据并保持原文件权限（或显式设为 `0o600`） $\rightarrow$ 执行 `sync_data()` $\rightarrow$ 原子 `rename` 覆盖目标路径 $\rightarrow$ 对父目录执行 `sync_all()`（Unix）或带退避重试（Windows），杜绝断电或崩溃产生半截损坏文件。
3. **增量路径规范化 (`incremental_canonicalize`)**：
   - 从左至右逐级解析路径组件中的符号链接（Symlink），然后再应用 `..` 父目录回退。防止攻击者通过工作区内的软链接配合 `../` 绕过词法路径检查逃逸到宿主机敏感目录。

### 4.2 多模型适配与 Token 经济层 (`kudelma-providers`)

1. **统一流式抽象 (`Provider` Trait)**：
   ```rust
   #[async_trait]
   pub trait Provider: Send + Sync {
       fn id(&self) -> &str;
       async fn stream(
           &self,
           model: &Model,
           system: &[SystemBlock],
           messages: &[Message],
           tools: &[ToolDefinition],
           options: &RequestOptions,
           cancel: &CancelToken,
       ) -> Result<StreamResponse, AgentError>;
   }
   ```
2. **模型档位路由 (`ModelTier`)**：
   - 每个 Provider 维护 `Weak`（如 `claude-haiku-4-5`, `gpt-4o-mini`, `kimi-k2-turbo`）、`Medium`（如 `claude-sonnet-4-5`, `kimi-k2`）、`Strong`（如 `claude-opus-4-5`, `o3`）三档映射。
   - 主会话默认使用用户指定的模型，`task` 子 Agent 与自动 Compaction 可按需指定档位以大幅降低成本。
3. **Prompt Cache 保护策略**：
   - 对 Anthropic 与支持前缀缓存的 OpenAI/Moonshot 接口，固定 System Prompt、Tools 定义列表及倒数第 2 条消息的缓存断点 (`cache_control: {"type": "ephemeral"}`)。

### 4.3 安全权限引擎与 Bash AST 审计 (`kudelma-permissions`)

1. **Bash 命令 AST 级作用域提取**：
   - 借助 `tree-sitter-bash` 将任意输入脚本解析为语法树，遍历所有 `command`、`pipeline`、`list`、`subshell`、`command_substitution` 节点：
     - 输入：`git status && cargo test --all || rm -rf ./target`
     - 提取出的独立权限作用域：`["git status", "cargo test --all", "rm -rf ./target"]`
   - 若语法树包含解析错误 (`has_error()`) 或复杂控制流块（`for`/`while`/`if`），则拒绝将首词泛化为 `<cmd> *` 通配符，强制以完整字面量命令向用户发起审批。
2. **Plan 模式权限闸门 (`ApprovalGate`)**：
   - 在 `AgentMode::Plan(plan_path)` 下：
     - 内置只读工具（`read`, `glob`, `grep`, `index`）直接放行。
     - 文件写入工具（`write`, `edit`）**仅当目标规范化路径等于 `plan_path` 时自动放行**，任何修改业务源码的请求直接拦截或强制弹窗。
     - 不透明的 MCP 工具调用在 Plan 模式下忽略全局 `*` 或 `--yolo` 宽泛授权，必须针对该具体工具显式确认。

### 4.4 高效代码工具链与环境隔离 (`kudelma-tools`)

| 工具名 | 权限类别 | 核心机制与 Token 优化设计 |
| :--- | :--- | :--- |
| **`index`** | `FsRead` | 使用 `tree-sitter` 解析源码文件，仅输出模块、结构体、函数签名、方法列表及其 `[start_line..end_line]`，省略函数体实现。引导模型“先 `index` 定位行号，再按 `offset/limit` 精准 `read`”。 |
| **`read`** | `FsRead` | 支持 `offset` 与 `limit`（默认最多 2000 行），输出带 `<line>: <content>` 前缀；记录每个文件读取时的 `mtime`，供后续 `edit`/`write` 校验。 |
| **`edit`** | `FsWrite` | 精确查找替换 `old_string -> new_string`；校验文件自上次 `read` 后未被外部篡改（Stale-read check）；支持行尾 CRLF/LF 自动保持与空白字符容错匹配；返回紧凑 Unified Diff。 |
| **`write`** | `FsWrite` | 创建新文件或全量覆写已有文件（已有文件同样触发 Stale-read 检查），底层调用 `atomic_write`。 |
| **`glob`** | `FsRead` | 基于 `ignore::WalkBuilder` 遍历工作区，自动忽略 `.git` 与 `.gitignore` 规则，按修改时间或路径相关性排序并截断上限（默认 100 条）。 |
| **`grep`** | `FsRead` | 基于 `grep-regex` + `grep-searcher` 高速搜索，按文件分组聚合匹配行号与预览，限制单行最大长度防 Minified JS 撑爆上下文。 |
| **`bash`** | `Run` | 在独立进程组（Unix `process_group(0)`）中启动 Shell；超时或取消时向整个进程组发送 `SIGTERM -> SIGKILL`；输出超过阈值（如 50KB / 2000 行）时**保留前 40% + 后 60%**，并将完整输出落盘至会话日志目录供 `read` 查阅；支持 `background: true` 异步执行。 |
| **`task`** | `Agent` | 派生独立上下文的 Coder 子 Agent（仅 `coder`），可选 `weak / medium / strong` 模型档位；主会话仅接收子 Agent 最终汇总报告。只读探索请使用 `explore_codebase`。 |
| **`todo_write`** | `SessionWrite` | 维护当前会话的结构化任务清单 (`pending` / `in_progress` / `completed`)，实时同步至 TUI 状态栏或侧边栏。 |
| **`memory`** | `FsWrite` | 将项目规范、常见构建命令或用户偏好持久化至项目/全局记忆库，支持 `/memory` 管理。 |
| **`webfetch`** | `Net` | 抓取指定 URL 并通过 `htmd` 转为清爽 Markdown；内置 DNS 解析后 IP 校验，默认拦截 `127.0.0.0/8`、`10.0.0.0/8`、`169.254.169.254`（云元数据端点）等私有地址以防御 SSRF（除非在 `net.allowed_private_hosts` 显式放行）。 |

### 4.5 Agent 核心循环与子 Agent 编排 (`kudelma-core`)

1. **三级作用域 (`AppScope -> SessionScope -> AgentScope`)**：
   - `AppScope`：全局单例，持有 `Config`、`StateDir`、`ProviderRegistry`、`ModelCatalog`。
   - `SessionScope`：对应一个打开的会话，持有 `SessionClaim`、`WorkspaceEnv`、`PermissionManager`、`McpSessionManager`、后台任务邮箱 (`SessionMailbox`)。
   - `AgentScope`：对应主 Agent 或某一个派生的子 Agent (`task`)，持有独立的 `History`（Append-only 消息栈）、`CancelToken`、`RunLedger`（Token 与费用账本）。
2. **步进执行与抢占循环 (`run_turns`)**：
   - 每一轮 (`Step`) 从当前 `History` 发起一次流式请求，边接收 `ProviderEvent` 边通过 `EventSender` 向 UI 推送 `TranscriptDelta`。
   - 若模型返回 `ToolUse` 块：
     1. 通过 `PermissionManager::enforce` 校验权限（必要时挂起等待 UI 的 `TaggedAnswer`）。
     2. 并发或按序执行工具，监听用户 `Esc` 取消信号或 `Ctrl-B` 转后台信号。
     3. 在进入下一轮 LLM 请求前的**安全步边界 (Safe Step Boundary)**：
        - 轮询 `InterruptSource` 是否有用户新输入的抢占消息 (`Steer`)，若有则合并入同一轮输入。
        - 检查后台任务邮箱 (`SessionMailbox`) 是否有已完成的后台 `bash` / `task` 结果，若有则作为通知注入。
        - 检查 `ContextGauge` 是否触发上下文窗口水位线（如已用 Token $\ge 80\%$ 窗口上限），若触发则自动执行 `Compaction`。

### 4.6 Ratatui 60FPS 交互式终端界面 (`kudelma-tui`)

1. **单线程 Elm 响应式架构**：
   - UI 主线程运行在单一 `crossterm::event::EventStream` 循环中，合并高频鼠标滚轮与拖拽事件以消除卡顿。
   - 所有状态变更统一收敛为 `App::update(&mut self, action: Action) -> Vec<Effect>`，渲染耗时长的语法高亮与 Markdown 排版通过后台 `render_worker` 预计算并缓存行快照。
2. **核心交互特性**：
   - **`Tab` 键**：一键在 `Build`（开发模式）与 `Plan`（只读规划模式）间切换。
   - **`Ctrl-B`**：将当前正在运行的慢速 `bash` 命令或 `task` 子 Agent 移交后台运行，立即释放主对话继续交互。
   - **`Ctrl-X` (`/tasks`)**：打开子 Agent 与后台任务切换器，实时查看任意子 Agent 的独立聊天流、向其追加指令或用 `Ctrl-S` 终止它。
   - **`Esc` / `Esc-Esc`**：单按 `Esc` 中断当前生成并保留部分输出；连按两次 `Esc` 打开历史消息时间线回滚器 (`Rewind Picker`)，支持回退对话或按 `Ctrl-F` 从指定历史节点分叉 (`Fork`) 出新会话。
   - **`!` 与 `!!` 快捷 Shell**：输入 `!cmd` 在终端直接执行命令并将输出附加到对话上下文中；输入 `!!cmd` 执行命令但不写入模型上下文（省 Token）。
   - **状态栏全透明展示**：常驻显示 `[Mode: Build/Plan]`、`[Model: provider/model]`、`[Context: 18.4k / 200k (9%)]`、`[Cost: $0.042]`，通过 `/usage` 查看按模型拆分的详细账单。

### 4.7 外部生态：MCP、Headless SDK 与 ACP 协议 (`kudelma-acp`)

1. **MCP 客户端与 `tool_search` 延迟加载**：
   - 支持标准 `stdio` 与 `Streamable HTTP / SSE` 传输协议。
   - 当连接的 MCP Servers 工具总数超过阈值（如 > 15 个）时，自动启用 `ToolDeferral`：仅向模型暴露一个轻量的 `tool_search(query)` 工具；当模型检索到所需 MCP 工具后，再将其 Schema 动态解锁以供调用。
2. **Headless 与 SDK 流式协议 (`--print`)**：
   - `kudelma -p "修复单元测试"`：非交互式运行，完成后直接打印最终回答并以退出码 `0/1` 退出。
   - `kudelma -p --input-format stream-json --output-format stream-json`：双向 NDJSON 流式协议，兼容主流自动化评测 Harness（如 FrontierHarness / SWE-bench）与外围脚本调用。
3. **ACP (Agent Client Protocol) 服务端 (`kudelma acp`)**：
   - 基于标准输入输出 (`stdio`) 的 JSON-RPC 服务，实现会话创建、流式响应推送、代码块 Diff 预览、文件读取委托与工具权限交互，可直接在 **Zed**、**JetBrains**、**Neovim** 等编辑器中配置使用：
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

---

## 5. CLI 命令体系与运行路径矩阵

```text
kudelma [OPTIONS] [PROMPT] [COMMAND]

核心选项 (Options):
  -p, --print                          非交互模式运行并退出
  -m, --model <PROVIDER/MODEL>         指定模型 (如 anthropic/claude-sonnet-4-5, moonshot/kimi-k2)
  -c, --continue                       恢复当前工作目录下最近一次会话
  -r, --resume <SESSION_ID>            恢复指定 ID 的会话 (别名: -s, --session)
      --fork-session                   基于加载的会话分叉出新会话 ID (需配合 -c 或 -r)
      --output-format <FORMAT>         输出格式: text (默认) | json | stream-json
      --input-format <FORMAT>          输入格式: text (默认) | stream-json
      --yolo                           跳过权限询问 (Explicit Deny 规则依然生效)
      --trust                          本次运行临时信任当前项目目录的 .kudelma 配置
      --allowed-tools <TOOLS>          逗号分隔的预授权工具列表
      --disallowed-tools <TOOLS>       逗号分隔的禁用工具列表
      --max-turns <N>                  最大自动连续执行轮数 (仅 --print / SDK 模式有效)
      --system-prompt <TEXT>           替换主 Agent 系统提示词
      --append-system-prompt <TEXT>    追加主 Agent 系统提示词
      --image <PATH>                   附加图片输入 (可多次指定)

子命令 (Subcommands):
  kudelma auth <login|logout|status>   管理各模型厂商 API Key 与 OAuth 认证
  kudelma models [--refresh]           列出所有可用模型、上下文窗口与价格信息
  kudelma session <list|delete|export|import>  管理、导出或迁移本地会话记录
  kudelma index <FILE_PATH>            预览 Tree-sitter 为指定文件生成的紧凑代码骨架
  kudelma mcp <list|auth|logout>       管理 MCP 服务器连接与认证
  kudelma trust <add|remove|list>      管理受信任的项目文件夹白名单
  kudelma prompt [--plan] [--tools]    预览当前渲染后的 System Prompt 或工具 JSON 定义
  kudelma acp [-m <MODEL>] [--yolo]    作为 ACP (Agent Client Protocol) 服务端启动
```

---

## 6. 分阶段实施路线图 (Phased Roadmap)

### Phase 1：Workspace 骨架、配置系统与崩溃安全存储
- [ ] 初始化 Cargo Workspace 根 `Cargo.toml` 与全部子 Crate 目录结构，配置编译优化 Profile（依赖项 `debug = false`，Release `lto = "thin"`）。
- [ ] 实现 `kudelma-storage`：
  - `StateDir` 与 XDG 标准目录解析。
  - `atomic_write` 与 `atomic_write_permissions`（含完备单元测试：权限保留、0600 强制模式、异常清理临时文件）。
  - `incremental_canonicalize` 物理路径解析与软链接防逃逸测试。
  - `KudelmaId` (UUIDv7) 与 `SessionClaim` 文件锁机制。
  - 会话持久化 (`StoredSession`) 与 `auth.json` 凭据存储。
- [ ] 实现 `kudelma-config`：
  - 全局配置与项目级 `.kudelma/config.toml`、`.kudelma/permissions.toml` 分层加载与 `Merge`。
  - `${VAR}` 环境变量安全展开（空值或未定义显式报错，拒绝发送半截 Bearer Header）。
  - `ToolKey` 严格解析校验（匹配 LLM Wire Format `^[a-zA-Z0-9_-]{1,64}$`）。
- **里程碑验收**：`cargo test -p kudelma-storage -p kudelma-config` 全部通过。

### Phase 2：多模型 LLM Provider 与 Token 经济核算层
- [ ] 实现 `kudelma-providers` 核心类型：`Message`、`Role`、`ContentBlock`（含前向兼容 `Unknown` 变体）、`ThinkingConfig`、`RequestOptions`。
- [ ] 实现 SSE 流式解析器与指数退避重试器 (`retry.rs`，支持 `Retry-After`、连接超时与流式低速超时检测)。
- [ ] 实现三大核心驱动：
  - `AnthropicDriver`（原生支持 `cache_control` 断点自动注入、Extended Thinking 块及签名保留）。
  - `OpenAiCompatibleDriver`（覆盖 OpenAI、Moonshot Kimi、DeepSeek、OpenRouter、Ollama）。
  - `GeminiDriver`（Google Generative Language 流式 API）。
- [ ] 实现 `ContextGauge` Token 估算器与模型定价账本 (`RunLedger` / `CostTally`)。
- **里程碑验收**：使用 Mock SSE Server 验证流式文本、Thinking 块、多并发 `ToolUse` JSON 拼接、断流重试与费用计算准确性。

### Phase 3：安全权限引擎与内置高效工具链
- [ ] 实现 `kudelma-permissions`：
  - 基于 `tree-sitter-bash` 的复合 Shell 命令解析器 (`bash_ast.rs`)，提取全部子命令作用域。
  - `PermissionManager::check` 与 `enforce` 异步审批流，覆盖 `Deny > YOLO > Allow > PlanWrite > Prompt` 优先级与 `TaggedAnswer` ID 校验。
  - 未信任目录 (`Untrusted Project`) 权限降级过滤（仅保留 `deny` 规则）。
- [ ] 实现 `kudelma-tools`：
  - 定义 `WorkspaceEnv` Trait 与 `LocalWorkspaceEnv` 实现。
  - 实现文件与检索工具：`read`（带时间戳登记）、`write`、`edit`（含 Stale-read 检查与 `similar` Diff 生成）、`glob`、`grep`。
  - 实现 Token 省流核心工具 `index`（集成 `tree-sitter-rust`, `tree-sitter-typescript`, `tree-sitter-python`, `tree-sitter-go`, `tree-sitter-c` 等提取符号大纲与行号）。
  - 实现进程组管理的 `bash` 工具（首尾双端保留截断、完整日志落盘、取消信号清理子进程树）。
  - 实现 `todo_write`、`memory` 与带 SSRF 内网地址过滤的 `webfetch`。
- **里程碑验收**：针对所有内置工具与权限边界编写表驱动单元测试 (`#[test_case]`)，验证软链接逃逸拦截、复合命令拆解与 Stale-read 保护。

### Phase 4：Agent 核心循环、双模式切换与子 Agent 编排
- [ ] 实现 `kudelma-core`：
  - `AppScope` / `SessionScope` / `AgentScope` 构建器。
  - `run_turns` 异步 Agent 主循环，实现 `assert_append_only` 契约校验、悬空 `ToolUse` 自动闭合 (`close_dangling_tool_calls`)、用户 `Esc` 中断部分输出保留。
  - `AgentMode::Build` 与 `AgentMode::Plan(PathBuf)` 模式切换及 `ContextUpdate` 帧注入。
  - 上下文自动与手动压缩器 (`compaction.rs`)。
  - `task` 子 Agent 调度器（仅 `coder` 角色与 `weak / medium / strong` 模型档位映射）及后台任务邮箱 (`SessionMailbox`)；共享只读探索由 `exploration/` 确定性执行器提供，不产生额外模型调用。
  - L1-L4 `Transcript` 增量事件流生成。
- **里程碑验收**：端到端集成测试验证多轮工具调用、中途 `Steer` 插队、取消恢复、自动 Compaction 及子 Agent 摘要回传。

### Phase 5：Ratatui 60FPS TUI 与 CLI 全命令打通
- [ ] 实现 `kudelma-tui`：
  - `TerminalGuard`（Raw Mode、Alternate Screen、Bracketed Paste、鼠标捕获、Panic 时自动恢复终端状态）。
  - Elm 架构 `App` 状态机、流式 Markdown + `syntect` 代码高亮渲染、内联 Diff 视图、Thinking 折叠框。
  - 多行输入框、`@` 文件模糊选择器 (`nucleo`)、`/` 斜杠命令面板、内联权限审批卡片、`Ctrl-X` 子 Agent 窗口切换器、`Esc-Esc` 历史回滚器。
  - 亮暗双色自适应主题系统与状态栏（模式、模型、Token 水位、成本统计）。
- [ ] 实现主二进制 `src/`：
  - `Cli` 参数解析与 `ScopedFlag` 路径合法性检查。
  - `--print` 文本模式与 `--output-format stream-json` SDK 模式。
  - `auth`、`models`、`session` (`list/delete/export/import`)、`index`、`trust`、`prompt` 子命令。
- **里程碑验收**：CLI 全子命令可用，TUI 交互流畅达到 60 FPS，无头模式兼容 `stream-json` 管道调用。

### Phase 6：MCP 延迟检索、ACP 编辑器集成与生命周期 Hooks
- [ ] 在 `kudelma-core` 中集成 MCP 客户端（stdio 与 HTTP SSE），实现 `tool_search` 工具延迟注入机制。
- [ ] 实现生命周期 Hooks（`PreToolUse`、`PostToolUse`、`SessionEnd` 本地命令触发与 OSC 9 / 桌面通知）。
- [ ] 实现 `kudelma-acp`：完成 `kudelma acp` stdio NDJSON 服务端，支持在 Zed 等支持 ACP 的 IDE 中直接驱动 Kudelma 会话。

---

## 7. 代码规范与测试验证体系

参照 Maki、OpenCode 与 Kimi Code 的工程标准，Kudelma 遵循以下研发规范：

1. **精炼无冗余代码 (Minimal Bloat)**：
   - 遵循 KISS、DRY、SRP 原则；不提前抽取仅单处调用的无意义包装函数。
   - 仅在解释非显而易见的系统不变量、协议边界或安全原因时编写注释，严禁无意义的废话注释。
2. **显式错误处理与类型边界**：
   - 领域层使用 `thiserror` 定义精确的枚举错误类型；应用顶层与 CLI 展示层使用 `color-eyre` 提供带因果链 (`├─` / `└─`) 的友好报错。
   - 在配置与反序列化边界一次性完成校验（如 `ToolKey::parse`、`NonZeroU16`），内部传递强类型结构，不在核心路径重复做防御性字符串检查。
3. **契约与表驱动测试**：
   - 单元测试与实现置于同一文件的 `#[cfg(test)] mod tests` 中，使用 `#[test_case]` 编写高覆盖率的表驱动测试。
   - 严禁依赖 `sleep` 的脆弱测试（Flaky Tests），异步并发与取消逻辑一律使用确定性的 Channel / Barrier / `CancelToken` 同步验证。
   - 常用验证命令：
     - 快速检查：`cargo check --workspace --all-targets`
     - 严格静态分析：`cargo clippy --workspace --all-targets -- -D warnings`
     - 运行测试套件：`cargo test --workspace`
