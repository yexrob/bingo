# bingo 技术决策记录

> 目标：Rust 实现的 agent CLI，完全对标 Claude Code。
> 决策日期：2026-08-04。事实均以 2026-08 实抓 crates.io/docs.rs/GitHub 为准。
> 参考：`~/Episodes/Resources/research/claude-code-re`（2.1.88 泄露 TS + 2.1.221 二进制逆向）。

## 架构总览

```text
┌─────────────────────────────────────────────────────────────────────┐
│ L1  CLI 入口 · clap (D8)                                             │
│  --version/--help 快路径 → 环境消毒 → settings 预读 → MCP 连接         │
│  → 分流：TUI（rsmarkdown-tui）｜ headless --print                     │
└───────────────────────────────┬─────────────────────────────────────┘
                                ▼
┌─────────────────────────────────────────────────────────────────────┐
│ L2  配置面 (D9)                                                      │
│  settings.json user/project/local · permissionMode · hooks 配置      │
│  mcpServers · feature flags（编译期 feature + 运行期开关）             │
└───────────────────────────────┬─────────────────────────────────────┘
                                ▼
┌─────────────────────────────────────────────────────────────────────┐
│ L3  交互面 (D4)                                                      │
│  Chat 组件 ← stream 事件   │  权限卡 ← canUseTool                    │
│  activities 思考/工具提示    │  SlashCommandMenu · 任务区 · AgentView │
└───────────────────────────────┬─────────────────────────────────────┘
                                ▼
┌─────────────────────────────────────────────────────────────────────┐
│ L4  Agent 核心 (D7)                                                  │
│  queryLoop:                                                         │
│    system 拼装 (D10) → Messages API stream (D1) → 收集 tool_use 块   │
│    → 并发队列 (D7) → tool_result 回填 → 再请求                       │
│    → end_turn ｜ max_tokens continuation ｜ compact (D12)            │
└───────────────┬─────────────────────────────┬───────────────────────┘
                ▼                             ▼
┌──────────────────────────────┐   ┌──────────────────────────────────┐
│ Tool Registry (D2)           │   │ MCP 适配层 (D3)                  │
│ trait Tool + schemars schema │   │ rmcp client → 同一 Tool trait    │
│ Read / Bash / Edit / ...     │   │ stdio ｜ streamable HTTP          │
│ 执行: tokio::process (D5)    │   │                                  │
└──────────────────────────────┘   └──────────────────────────────────┘
                │
                ▼
┌─────────────────────────────────────────────────────────────────────┐
│ L5  横切                                                             │
│  权限门 (D2)：模式 × 规则 × UI 审批                                   │
│  Hooks：Pre/PostToolUse · Session · Stop · Compact（shell, JSON）    │
│  Transcript 存储 (D11) · token budget (D12) · 子代理 (D14)           │
│  暂缓：Sandbox ｜ 遥测 (D13) ｜ plugins ｜ worktree/teammate (D14)    │
└─────────────────────────────────────────────────────────────────────┘
```

主循环往返：`模型只产出 tool_use；本地 harness 负责权限、并行、副作用、压缩、记忆与 UI。`

## 已定决策

### D1. Messages API 客户端：自写（reqwest + SSE）

- Anthropic 官方至今无 Rust SDK（`claude-agent-sdk-rust` 404 确认）；crates.io 社区 SDK 全部停更/玩具级。
- 自写范围：SSE 事件解析（`input_json_delta` 累积到 `content_block_stop` 再解析）、stop_reason、指数退避重试（429/529，遵守 retry-after，上限 2-3 次）。
- 2026 API 现状：prompt caching 已 GA（顶层 `cache_control`，无需 beta header）；thinking 用 `adaptive` 配置（interleaved 自动生效）；thinking 块回传必须原样含 signature。

### D2. Tool 协议：trait + schemars，权限独立成门

- Zod 等价物：`schemars` 生成 inputSchema；模型回传参数先 `jsonschema` 校验再 serde 反序列化。
- Tool 用 trait（name/aliases + inputSchema + call + isConcurrencySafe + isReadOnly/isDestructive + validateInput + interruptBehavior），默认 fail-closed（非并发安全、allow）。
- `checkPermissions` 不进 trait——权限是横切面，走统一权限门（参考 goose 2026：工具注册走 rmcp model，权限独立 `Permission` 门）。

### D3. MCP：rmcp（官方 rust-sdk）

- `modelcontextprotocol/rust-sdk` 官方，3.1.0（2026-07-31），client 能力齐备：stdio（TokioChildProcess）、streamable HTTP、OAuth。
- mcpServers 配置 → 连接 → list_tools → 适配成同一 Tool trait（isMcp + mcpInfo）。
- 不碰其它 MCP crate（mcp-server / mcplease 无 client 或初级）。

### D4. TUI：rsmarkdown-tui（自有项目，path 依赖）

- 路径依赖 `~/Episodes/Projects/rsmarkdown-tui/crates/tui`，待其发版后切 crates.io 版本。
- 现成：StreamMarkdownRenderer（流式 markdown）、activities（思考/tool 提示 + spinner）、`App::ask` 权限模态、SlashCommandMenu、任务区、AgentView、Theme。
- bingo 只做组件接线：Chat 组件接 stream 事件、权限卡接 canUseTool、TodoWrite → tasks、Agent 工具 → agents。
- 不用 ratatui/tui-markdown 直接开发；不抄 goose 的 cliclack+rustyline 路线。

### D5. 运行时与进程

- tokio 单 runtime；crossterm EventStream + `tokio::select!` 事件循环；工具执行 JoinHandle/AbortHandle；输入打断 = select + watch channel。
- Bash 工具：`tokio::process` + `shlex`（goose 同款，无 pty）；交互式 shell 再上 `portable-pty`。

### D6. Token 计数：官方 count_tokens API

- Claude tokenizer 闭源，`claude-tokenizer` 停更不准。
- 预算显示走 `POST /v1/messages/count_tokens`；本地估算用 `claude-tokenizer` 兜底不当真。

### D7. 主循环语义

- `stop_reason === 'tool_use'` 不可靠，以实际出现的 tool_use 块为准。
- 并发执行队列：safe 工具并行（上限 `CLAUDE_CODE_MAX_TOOL_USE_CONCURRENCY` 等价物，默认 10），非 safe 串行且不越过前面未完成的写。

### D8. CLI 入口：clap + 显式启动链

- 参数解析用 `clap`（derive）。
- 启动链对标 claude code：`--version`/`--help` 快路径（不加载重模块）→ 环境消毒 → settings 预读 → MCP 连接 → 交互（TUI）或 headless（--print/-p）分流。
- 初始参数面：`--model`、`--permission-mode`、`--continue`、`--add-dir`、`-p/--print`（headless）。

### D9. 配置分层：settings.json + feature flags

- 配置分层对标 claude code：user（`~/.config/bingo/settings.json`）/ project（`.bingo/settings.json`）/ local（`.bingo/local.json`），浅层合并，本地不入库。
- settings 承载：permissionMode、hooks 配置、mcpServers、主题/通知偏好。
- feature flags：编译期 `feature()`（对标 bun:bundle DCE）+ 运行期开关；新增能力默认关。不引入 GrowthBook——本地 CLI 不需要远程下发。

### D10. 系统提示拼装 + prompt caching 策略

- system prompt 分段拼装：base（角色/规则）→ 工具说明 → CLAUDE.md 记忆层（managed/user/project/--add-dir）→ 会话附加段。
- 分段顺序即缓存策略：tools → system 顺序缓存（`cache_control: ephemeral` 放在 system 与 messages 末尾断点，最小缓存量 512~4096 token），保证多轮只换尾部。

### D11. Transcript 与会话存储

- 每次会话持久化 transcript（JSON Lines，对标 claude.json 项目历史）；支持 `--continue`/`--resume` 恢复。
- Compact 以 transcript 为边界：压缩前存档、压缩后生成摘要段替换。

### D12. token budget 管理

- 输出侧：max_tokens 动态管理（对标 ESCALATED_MAX_TOKENS 升级路径）与 turn 输出预算（超限 continuation）。
- 输入侧：token 计数走 D6，达到阈值触发 autoCompact（对标 `autoCompactEnabled`/`DISABLE_AUTO_COMPACT`）。

### D13. Sandbox 与遥测：显式不做（初期）

- Sandbox（对标 sandbox_init）不做，权限门 + 模式即安全边界；记录为后续项。
- 遥测不做；仅本地 debug 日志。避免引入 analytics 依赖。

### D14. 多代理边界

- 首期只做子代理（Agent 工具递归 queryLoop，subagent 独立消息历史）。
- worktree/teammate/团队协作面不做（对标 2.1.221 的产品化面，非核心 harness）；后续按需。

## 对标清单（按实现顺序）

1. headless 最小闭环：API 客户端 + queryLoop + Read/Bash 工具 + 权限门（D1/D2/D7/D8）
2. 并发队列 + Hooks 运行时（shell hook，JSON stdin/stdout，CLAUDE_ENV_FILE）
3. 系统提示拼装 + transcript 存储 + token budget（D10/D11/D12）
4. MCP 接入（rmcp → Tool 适配）
5. TUI 接线（rsmarkdown-tui）+ slash 命令
6. Compact + CLAUDE.md/memdir 记忆 + 子代理（Agent 工具）
7. 后续：sandbox、plugins、worktree/teammate（D13/D14 暂缓项）

## 参考

- goose（aaif-goose/goose，纯 Rust agent，permission 门 + execution + agents 结构）
- rsmarkdown-tui README（组件 API、活动模型、权限模态契约）
