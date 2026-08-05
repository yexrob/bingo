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

- Zod 等价物：`schemars` 生成 inputSchema（`schema_for::<T>()`，input 结构体单一来源；模型回传参数 serde 反序列化即校验，错误以 is_error 回填模型。jsonschema 校验器不引入——9 个静态简单 schema 无收益，serde 已覆盖类型错误）。
- Tool 用 trait（name/aliases + inputSchema + call + isConcurrencySafe + isReadOnly/isDestructive + validateInput + interruptBehavior），默认 fail-closed（非并发安全、allow）。
- `checkPermissions` 不进 trait——权限是横切面，走统一权限门（参考 goose 2026：工具注册走 rmcp model，权限独立 `Permission` 门）。

### D3. MCP：rmcp（官方 rust-sdk）

- `modelcontextprotocol/rust-sdk` 官方，3.1.0（2026-07-31），client 能力：**落地仅 stdio**（TokioChildProcess）；streamable HTTP / OAuth 为 SDK 能力未启用。
- mcpServers 配置 → 连接 → list_tools → 适配成同一 Tool trait（isMcp + mcpInfo）。
- 不碰其它 MCP crate（mcp-server / mcplease 无 client 或初级）。

### D4. TUI：rsmarkdown-tui（自有项目，path 依赖）

- 路径依赖 `~/Episodes/Projects/rsmarkdown-tui/crates/tui`，待其发版后切 crates.io 版本。
- 现成：StreamMarkdownRenderer（流式 markdown）、activities（思考/tool 提示 + spinner）、`App::ask` 权限模态、SlashCommandMenu、任务区、AgentView、Theme。
- bingo 只做组件接线：Chat 组件接 stream 事件、权限卡接 canUseTool、TodoWrite → tasks、Agent 工具 → agents。
- 不直接用 tui-markdown 开发；rsmarkdown-tui 底层基于 ratatui（经自有封装，不直接面向 ratatui 编程）；不抄 goose 的 cliclack+rustyline 路线。

### D5. 运行时与进程

- tokio 单 runtime；crossterm EventStream + `tokio::select!` 事件循环；工具执行 JoinHandle/AbortHandle；输入打断 = select + watch channel。
- Bash 工具：`tokio::process` + `/bin/zsh -c`（goose 同款无 pty；shlex 未用）；交互式 shell 再上 `portable-pty`。

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

### D15. 任务追踪：v2 Task 工具族（对标 2.1.221 实证）

> 来源：2.1.221 二进制 `cli.bundle.min.js` 逆向（`~/Episodes/Resources/research/claude-code-re/output/clean/`），行号见 runtime-diff 表。v1 TodoWrite 是 v2 的语义前身，bingo 直接取 v2。

- **工具面**：`TaskCreate`（subject/description/activeForm?/metadata?，一次一任务，输出 `{task:{id,subject}}`）、`TaskUpdate`（taskId + 可选字段，**增量 patch 语义**，status 增 `deleted` 永久删除并清理他任务引用）、`TaskGet`、`TaskList`（过滤 `metadata._internal`，completed 从 blockedBy 剔除）。共同属性：shouldDefer、无权限检查、renderToolUseMessage null（UI 走任务区）、调工具时 `set_expanded_view: tasks`。
- **存储**：磁盘 `~/.claude/tasks/<listId>/<taskId>.json`（listId = `CLAUDE_CODE_TASK_LIST_ID` env > teamName > sessionId），每任务一文件，**跨会话持久化**；数字 id 递增（max+1）；读时逐条 safeParse；**文件锁**（withLock / v5 乐观锁 ifMatch+version，Bun 的 LSP kv 后端可选）。
- **输入修复层**：coerceInput 把近似的 key 名修复（title/name→subject、content→description、active_form→activeForm、task 包裹拆包、缺 description 时 backfill），validationErrorSteer 给误用（tasks/todos 数组参数、Agent 参数）返回引导文案。
- **Hooks**：新增 `TaskCreated` / `TaskCompleted` 事件；TaskCompleted 的 blockingError 可**拒绝** completed 状态。
- **提醒注入**：`task_reminder` attachment，阈值 `TURNS_SINCE_WRITE=10` / `TURNS_BETWEEN_REMINDERS=10`；v2 额外要求工具表含 TaskUpdate；开关 `CLAUDE_CODE_TODO_REMINDER_MODE` / feature `tengu_soft_slate_nudge`；meta user message 注入 + "NEVER mention this reminder"。
- **bingo 取法**：v2 增量语义（v1 全列表覆盖写在并发下是丢失更新温床）+ 磁盘持久化 + 单文件锁（与 transcript 文件习惯同构）；**砍** owner/swarm 分配（D14 已定不做 teammate）与 metadata merge（首版支持即可）；TaskCompleted 阻断 hook 对齐现有 hooks 语义；reminder 阈值直接取 10/10。实现归"对标清单"第 6 项后。

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
- [runtime 三方 diff（CC 2.1.88 leak / Codex / bingo 现状与差距分级）](./runtime-diff.md)

## 已定决策（续）

### D16. TUI 渲染层迁移：ratatui + rsmarkdown-tui → iocraft（对标 CC 的 ink 架构）

- **动因**：CC 的 TUI 是 ink（React 式声明组件）构建的；iocraft 是 Rust 端最接近 ink 的声明式组件库（hooks + flexbox + fullscreen render loop）。迁移后 bingo 的 UI 架构与 CC 同构，布局可 1:1 对标。
- **取舍**：`rsmarkdown-tui`（App/Component 框架、~12k 行）整体弃用；`rsmarkdown-core`（markdown 流式解析，显示无关）保留。渲染层全部重写为 iocraft 元素（不保留 ratatui Line 适配桥）。
- **新结构**：`src/tui/` — `chat.rs`（状态机 + 文档行构建，原样保留事件语义与折叠逻辑）、`line.rs`（样式化行模型）、`theme.rs`（CC 2.1.88 dark 令牌 1:1）、`markdown.rs`（AST → 行，renderer.rs 移植）、`activities.rs`（活动数据 + 头部/折叠布局移植）、`components.rs`（iocraft 根组件 + transcript）。
- **布局对标点**（CC FullscreenLayout/REPL）：单列布局（无 sidebar）＝ sticky header + 滚动 transcript + 任务列表 + 通知行 + 输入行 + `╰──╯` 边框 + 1 行 footer；权限请求渲染在 transcript 底部（非模态）；消息块 marginTop=1；footer 左＝模式徽标（⏸ plan / ⏵⏵ accept edits）+ 快捷键 byline，右＝模型名。
- **关键坑（已验证）**：iocraft `State::write()` 每次 deref 都标记 dirty → 组件 body 里无条件 write() 会导致无限重渲（mock 终端可观测为 ~350 帧/秒空转）。必须"受守卫的写入"：布局尺寸或文档 dirty 才写，dirty 由事件/tick 消费方置位。
- **交互等价**：鼠标点击折叠/展开（`use_local_terminal_events` 本地坐标 → doc 行号）、滚轮、ctrl+o 全局展开、j/k/G/g/PageUp/Down 滚动、busy 时 Esc/Ctrl+C 中断、权限数字键选择。测试经 `mock_terminal_render_loop` + 行级 `build_rows` 双通道。

### D17. TUI 渲染修复：diff 残留与小窗口错位（iocraft 0.8.4 实测）

- **症状**：真实终端出现残影/重复行——thinking 运行态"卡住"后下一行重渲染、输入行 `❯ ▋` 多处残留、长回复正文短版/长版并存；窗口变小时更明显。
- **根因（实测三链）**：
  1. iocraft 全屏行 diff（`write_canvas` 逐行 MoveTo + 重写）在**行号位移**（markdown wrap 行数变化、消息增删、sticky 出现）时残留旧行——写入序列本身正确（逐帧解码验证），但终端状态与内存 prev 脱节。
  2. `use_terminal_size` 依赖 Resize 事件，事件丢失/滞后时 canvas 高度与终端实际不符（tmux winsize 滞后：pane 16 行而 bingo 读到 24）→ MoveTo 越界 → 终端滚动 → 错位。
  3. sticky header 占布局 1 行 → 出现/消失时内容整体位移。
- **修复（bingo 侧，不改 iocraft）**：
  1. sticky 改为**绝对定位 overlay**（不占布局，Transcript 内部 `Position::Absolute`）。
  2. **doc 行数变化或 TurnEnd 时置 `FORCE_FULL_REDRAW` 全局标志**，自定义 hook（`use_force_redraw_on_resize`）在 `post_component_update` 消费 → `updater.clear_terminal_output()` 强制整屏清除重绘（绕开行 diff）。
  3. 终端尺寸轮询（hook poll_change 读 crossterm size）→ 尺寸变化同样全清。
- **验证**：真实 API（DeepSeek）tmux 多轮对话——流式正文、工具轮、thinking 块交替、小窗口（16 行）resize 往返，全部无残影。mock 回归 171 测试全绿。

### D18. 主题配置（CC `theme` 设置 1:1）

- **配置**：`settings.json` 新增 `"theme": "auto" | "dark" | "light"`（缺省 auto，对标 CC `ThemeSetting`）。
- **auto 检测**（对标 CC `systemThemeWatcher`）：fullscreen 前临时进 raw mode 发 OSC 11 查询终端真实背景色（`ESC ] 11 ; ? ESC \`），按 BT.709 相对亮度判断深浅；其次 `$COLORFGBG` 种子；都无则回落 dark。坑：OSC 回复不带换行，规范模式行缓冲会吞掉，必须 raw mode 下读。
- **令牌**：dark/light 两套均 1:1 对齐 CC 2.1.88 `theme.ts`（light 正文黑 `rgb(0,0,0)`、`userMessageBackground` 240、claude 橙两主题相同）。不支持 truecolor 时 RGB 降级 256 色（AnsiValue cube 近似）。
- **欢迎页标题**：`Welcome back` 用 claude 橙（CC `color="claude"`），非白色。

### D19. 流式残影根治：事件级强制全清（diff 路径残留）

- **症状（用户真实终端 Ghostty，tmux 不可复现）**：流式正文增长时出现"半截覆盖"——新内容覆盖旧行部分区域，行尾残留旧字符；TurnEnd 全清后恢复。
- **排查**：trace 证实 FORCE 全清链路本身正确（每次 doc 行数变化后 hook 都消费并全量重写，0 失配）；tmux/pty/模拟终端均无法复现 → 问题在 **diff 路径**：内容在行内增长（行数不变）时走 iocraft 行 diff，真实终端下残留旧行。DeepWiki 协查：`write_ansi_row_without_newline` 理论总会清行尾，row_eq 裁剪比较在背景色/填充场景可能误判相等（issue #142 族）。
- **修复**：**任何事件处理（`drain_all` 返回 true，覆盖 TextDelta/ThinkingDelta/ToolStart 等）→ 立即置 `FORCE_FULL_REDRAW`** → 内容变化的帧全部走全量清除重绘，绕开行 diff。synchronized update（2026）下同帧原子完成，无闪烁；DeepWiki 确认该模式为 iocraft 惯用法（`use_output` 内部同款）。
