# Kudelma

> 一款用 Rust 编写的高性能终端 AI 编程智能体（AI Coding Agent）。单二进制、快速启动、低 Token 消耗，并带有安全的多级权限、可恢复会话、子 Agent、MCP 与 ACP 集成。

Kudelma 的架构参考了 [tontinton/maki](https://github.com/tontinton/maki)、[anomalyco/opencode](https://github.com/anomalyco/opencode) 与 [MoonshotAI/kimi-code](https://github.com/MoonshotAI/kimi-code)，完整设计见 [`PLAN.md`](./PLAN.md)。

---

## 特性

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
  - Tree-sitter 拆解复合 Shell 命令（`git status && rm -rf /` → 两个作用域），重定向/替换等高风险形式要求精确批准。
  - Plan 模式只允许写计划文件，命令必须确认。
  - 未信任目录只生效 `deny` 规则，不加载项目 MCP / hooks。
- **会话与上下文**：
  - 会话自动保存到 XDG state 目录，支持 `-c` / `-r` 恢复、导出/导入、跨进程锁。
  - 自动上下文压缩（阈值可配），超长工具输出首尾保留。
  - `Esc` 中断保留已生成内容；`Esc Esc` 回滚到历史 Prompt。
- **Agent 编排**：
  - `task` 子 Agent：`explore`（只读检索）、`plan`（只读规划）、`coder`（实现），支持 `weak` / `medium` / `strong` 模型档位。
  - 后台任务完成通知，下一轮自动唤醒模型。
- **MCP**：stdio 与 Streamable HTTP MCP 客户端；工具过多时通过 `tool_search` 延迟注入 Schema。
- **ACP**：`kudelma acp` 提供 Agent Client Protocol stdio 服务，可在 Zed 等编辑器中直接使用。
- **Headless / SDK**：
  - `kudelma -p "..."` 单轮执行。
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

# 在当前项目启动 TUI
cd your-project
kudelma

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

> Headless 模式默认**拒绝**权限询问（避免无人值守时静默放行）。需要执行命令或写工作区外文件时加 `--yolo`，或用 `--allowed-tools` 精确放行。

---

## 配置

全局配置：`~/.config/kudelma/config.toml`（`KUDELMA_CONFIG_DIR` 可覆盖）。
项目配置：`<project>/.kudelma/config.toml`；项目权限：`<project>/.kudelma/permissions.toml`。

`kudelma config init` 会生成模板。

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

内置命令：`/help`、`/new`、`/sessions`、`/resume`、`/model`、`/mode`、`/compact`、`/usage`、`/tasks`、`/rewind`、`/clear`、`/cd`、`/init`、`/quit`。

---

## CLI

```text
kudelma [OPTIONS] [PROMPT]
kudelma <COMMAND>

Commands:
  auth     管理凭据 (login/logout/status)
  models   列出模型、上下文与价格
  session  会话列表/删除/导出/导入
  index    Tree-sitter 符号骨架
  trust    管理受信任目录
  prompt   打印 System Prompt 或工具定义
  acp      ACP stdio 服务
  config   path/show/init
  mcp      列出 MCP Server
  doctor   环境与配置诊断
```

常用参数：`-p`、`-m`、`-c`、`-r`、`--yolo`、`--trust`、`--plan`、`--output-format`、`--input-format`、`--max-turns`、`--system-prompt`、`--append-system-prompt`、`--allowed-tools`、`--disallowed-tools`、`--verbose`。

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
     ├── kudelma-config      分层配置、权限规则、Provider/MCP/Hook 配置
     ├── kudelma-storage     XDG 路径、原子写入、会话、凭据、信任、记忆
     ├── kudelma-providers   Anthropic / OpenAI-compatible / Gemini 流式客户端
     ├── kudelma-permissions 权限决策、Tree-sitter Bash 拆解、路径边界
     ├── kudelma-tools       WorkspaceEnv + 内置工具（index/read/edit/bash/...）
     ├── kudelma-core        Agent 循环、会话、压缩、子 Agent、MCP、Hooks
     ├── kudelma-tui         ratatui TUI（Markdown、权限弹窗、选择器）
     └── kudelma-acp         Agent Client Protocol stdio 服务
```

核心不变量：

- **Append-only**：对话只追加，不重写历史前缀，保护 Prompt Cache。
- **环境隔离**：模型可见文件操作经 `WorkspaceEnv`，宿主状态只落 `StateDir`。
- **Fail-closed**：拒绝优先；未信任目录只能收紧权限；带标签的权限响应防止迟到审批误用。
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
- MCP OAuth、Lua 插件热加载与 Web 搜索尚未内置；MCP stdio/HTTP 与 hooks 已可用。
- 模型价格与上下文窗口为内置目录数据，可通过 `[providers.*.models]` 覆盖。

欢迎提交 Issue 与 PR。

## License

MIT
