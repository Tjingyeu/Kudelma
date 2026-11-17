# 修改计划：多供应商连接与 OpenCode Go 订阅

**目标：** 让 Kudelma 的 CLI/TUI 可以连接并选择多个模型供应商；优先打通 OpenCode Go API-key 订阅，便于快速测试。

**参考：** OpenCode V2 Providers、CLI Providers、Models 与 OpenCode Go 文档。OpenCode Go 使用订阅 API key；模型走 OpenAI-compatible Chat Completions 端点，并要求客户端发送自有 User-Agent 和稳定的 `x-opencode-session`。

## 已完成的实现

- [x] 增加 `opencode-go` Provider preset，指向 `https://opencode.ai/inference/go/openai/v1`，读取 `OPENCODE_API_KEY`，使用现有 OpenAI-compatible 驱动。
- [x] 加入首批 Go 模型目录：`kimi-k2.7-code`、`glm-5.3-flash`、`gpt-6-luna`、`claude-haiku-5-5`，包含展示用的上下文、能力档位与计价数据。
- [x] 在 `ChatRequest` 中增加可选会话 ID；Lead、Agency、Coder 请求使用稳定 ID，Go 请求附带 `x-opencode-session` 与 `kudelma/<version>` User-Agent。
- [x] 把 `opencode-go` 加入 OpenAI-compatible stream usage 支持。
- [x] TUI 增加 `/connect` 供应商选择及隐藏 API key 输入；凭据通过 `AuthStore` 存储，不写入 TOML。
- [x] TUI `/models` 可选模型，模型列表显示供应商连接状态；按 `c` 可直接进入供应商连接流程。
- [x] 连接成功后刷新当前模型目录并把新凭据发送给当前会话；Agency / Lead 可分别切换模型。
- [x] 连接后重新解析当前 Agency/Lead/Coder 的 `Model` 快照，修复旧 key 仍为空导致首次请求失败的问题。
- [x] CLI 增加 `kudelma models --connect` 交互式连接，以及 `kudelma auth login opencode-go` 凭据入口。
- [x] Session metadata 增加 Agency / Lead 独立模型字段，并为旧 session 提供兼容默认值。

## 尚未完成

- [x] 运行 `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`cargo build --release`，修复本轮新改动导致的问题。
- [x] 增加 Provider 单元测试：Go catalog、请求 endpoint、User-Agent 与 `x-opencode-session` header。
- [x] 增加 session/model 测试：连接凭据后，当前 Agency/Lead 能解析并切换到新供应商模型；旧 session 可加载新模型字段。
- [x] 用本地 Mock OpenAI-compatible SSE 服务端做端到端测试：连接 API key、列出 OpenCode Go 模型、选模型、发送工具调用请求、确认稳定 session header。
- [x] 验证 TUI 键盘输入和粘贴 API key 时不回显 key，连接失败也不把 key 写进日志或 session transcript。
- [x] 更新 README：记录 `/connect`、`/models`、`kudelma models --connect`、`kudelma auth login opencode-go` 和快速测试命令。
- [x] 最终审查连接 UI 是否支持多个已连接供应商、重新录入 key、切换模型与自定义 provider 配置。
- [ ] （需要用户自己的订阅 key）进行真实 OpenCode Go endpoint smoke test；没有有效订阅 key 时仅报告 Mock 测试结果，不伪称真实 API 已验证。

## 验证记录

- `cargo fmt --all -- --check` ✅
- `cargo clippy --workspace --all-targets -- -D warnings` ✅
- `cargo test --workspace`：**75 个测试全部通过** ✅
- `cargo build --release` ✅
- Mock OpenAI-compatible SSE：
  - `opencode-go/glm-5.3-flash` 请求成功，`model` 为 `glm-5.3-flash`，
    `user-agent` 为 `kudelma/0.1.0`，`x-opencode-session` 在工具调用的两轮请求中保持一致 ✅
  - Agency 使用 `[agency]` 模型、Lead 使用 `[lead]` 模型；Coder 使用 `[agents.coder]` 模型 ✅
  - TUI `/connect` 粘贴 `sk-tui-secret` 后输出中不出现 key，`auth.json` 正确保存 key ✅
  - TUI 连接后无需重启即可用当前 OpenCode Go 模型完成一次 `read` 工具调用并返回结果 ✅
  - `kudelma models --connect` 交互输入同样不回显 key ✅
  - 恢复 Agency/Lead 会话时分别沿用对应的 role model ✅
- 真实 endpoint 探测（无有效订阅 key）：不带 `x-opencode-session` 返回
  `400 MissingSessionID`；带该 header 和 dummy key 返回 `401 Invalid credential`，
  说明 endpoint 可达且 session header 被接受；**未进行真实模型调用** ❌

## 快速测试路径（实现验证后）

```bash
kudelma auth login opencode-go
kudelma models
kudelma -p --model opencode-go/kimi-k2.7-code "列出当前项目的主要模块，并给出文件路径"
```

在 TUI 中也可执行 `/connect` 输入 OpenCode Go API key，然后执行 `/models` 选择 `opencode-go/kimi-k2.7-code`；在模型选择器中按 `c` 可直接进入连接流程。

## 边界与注意事项

- 第一阶段只做 API key 连接；OAuth/device-code 等登录方式不在本次范围内。
- 每个 provider 目前只保存一个 API key；同一供应商的多账号切换尚未实现。
- Go 模型目录先采用内置 curated 子集，不在启动时请求远程模型目录；其他 Go 模型 ID 仍可通过 `--model opencode-go/<id>` 或 `/model` 显式选择。
- Go endpoint 可通过控制台翻译不同文本模型 API；本实现优先复用当前 OpenAI Chat Completions 驱动，暂不实现独立 Responses / Anthropic / Gemini Go transport。
- OpenCode Go 当前按公开文档中的 token 单价展示估算成本；订阅额度与实际账单以 OpenCode Console 为准。
- 当前 `Model` 只记录 input/output 单价，尚未单独计算 cache read/write 成本；Go 的部分模型有缓存差异。
- 真实 endpoint 的订阅鉴权与模型响应仍需有效 `OPENCODE_API_KEY` 才能完成最终 smoke test。
