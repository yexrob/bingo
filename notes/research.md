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

### D20. 交互默认改 REPL inline 模式（对标 CC 非全屏，iocraft use_output）

- **背景**：原默认全屏 iocraft canvas（alt screen + app 内视口滚动 + 输入吸底）被用户感知为"大的应用壳子、强制刷屏"；CC 默认（用户环境）是非全屏：定稿内容像普通终端输出一样落 scrollback，滚轮翻历史，prompt 在对话末尾而非钉屏底。
- **CC 机制实证**（2.1.88 泄露源码）：非全屏 = `<Static>` print-and-forget（定稿消息渲染一次写入 stdout，永不复写，`UserPromptMessage.tsx:25` 注释原文）+ 动态尾部（流式/spinner/输入行）原地重绘（`log-update.ts` 相对光标行 diff）；滚动离开视口的行冻结（`OffscreenFreeze`）避免全量重刷。
- **选型**：先手写 REPL 驱动（ANSI 序列化 + 回卷/落盘记账 + 自管 crossterm 事件循环，~500 行，存于 `git stash` 可找回）验证机制；后确认 **iocraft 0.8.4 原生 `use_output` 即等价物**（`hooks/use_output.rs:74` exec：`clear_terminal_output` → 写 stdout → 渲染循环重画；与 ink Static 三步舞同构）→ 弃手写，组件层复用（markdown/主题/活动布局/`row_element` + `to_string()` 离屏渲染）。
- **实现**：`Bingo` 组件新增 `inline` 模式——`doc.settled`（定稿边界，D19 前已有机制）前进时逐行 `println(row_element(row).to_string())` 落盘（多行块必须逐行 println：raw mode OPOST 关，块内 `\n` 不回列首会阶梯错位，rsink demo 实证）；Transcript 渲染 `rows[tail_start..]` 尾部切片（落盘边界 = max(printed, settled, len−max_live)，canvas 高度恒 ≤ 终端，inline 擦除不污染 scrollback）；按键 gate：空闲 Esc 忽略 / ctrl+o 只放行未定稿消息 / 空闲 Ctrl+C → `system.exit()`（须 `ignore_ctrl_c()` 让事件派发给组件，busy 时 Ctrl+C 走 on_key 取消）；`--fullscreen` 保留原 canvas 路径。
- **连带修复**：`detect_system_theme` 原用 `tokio::fs` 读 `/dev/tty`——timeout 后 blocking 线程 read() 不可取消，会吞掉之后第一个输入且 tokio 关停 join 挂起（进程退不出）；改 `std::fs` + `O_NONBLOCK` + 轮询（libc 依赖，已锁定）。
- **验证**：PTY 冒烟（python pty + winsize 100x24）：欢迎卡片落盘、无 alt screen、输入逐键反映、thinking spinner 动态区回卷重绘、Ctrl+C 退出 6/6 稳定；测试 178 全过。

### D20b. inline 模式两处修正（启动清屏 + 输入吸底）

- **启动清屏**：`ForceRedrawOnResize` 首次检测（`last=None` 视为尺寸变化）+ 首帧 `FORCE_FULL_REDRAW`（doc 行数 0→N）→ 启动第一帧 `clear_terminal_output()` 清掉 shell 残留。修复：首次 poll 只记录基准尺寸不算变化；`post_component_update` 首帧跳过清屏（`first` 标志）。全屏模式不受影响（进 alt screen 本就空屏）。
- **输入吸底**：inline 模式根 View 固定 `height: height`（全屏设计）→ canvas 恒占满终端、输入行钉在屏幕最底行。修复：inline 根 View 不固定高度（内容自然高度）、Transcript `flex_grow: 0`——canvas 高度 = 尾部 + chrome 实际行数（空闲时 3 行），输入行随内容流走，与 CC 非全屏一致。
- 验证（PTY + winsize 100x24）：启动无 Clear-All/清屏序列；输入后重画回卷距离 2（canvas 高 3，此前为 23）；Ctrl+C 退出 ~0.3s。

### D20c. 落盘双换行修复（println + to_string 行尾 \n）

- **症状**：同一段对话两种格式——动态区紧凑（`Mulling for 1.4s …` 行间无空行）、落盘后每行之间多出空行（scrollback 里行距变松）。
- **根因**：`ElementExt::to_string()` 输出的 ANSI 字符串行尾必带 `\n`（iocraft canvas Display，测试断言 `"hello!\n"`）；`StdoutHandle::println` 再补 `\r\n` → 每个落盘行变成 `行\n\r\n`，屏幕渲染为"行 + 空行"。
- **修复**：落盘前 `trim_end_matches(['\n', '\r'])` 再 println。PTY 验证：落盘序列 `❯ hello` → margin 空行 → 下一消息，无每行穿插空行。
- **顺带排查（未修，既有行为）**：busy 时 Ctrl+C 取消——`client.stream()` 连接/建流阶段不在 `select!` 内（query.rs:234 在 select 前 await），连接挂起时（如本地 discard 端口）取消无法打断；真实 API 流式响应下无影响。

### D20d. 落盘颜色丢失修复（to_string 无 ANSI）

- **症状**：D20c 修复双换行后，scrollback 里落盘内容全部无色（动态区正常）。
- **根因**：`Canvas::write()`（无样式路径，"as unstyled text, without ANSI escape codes"）是 `Display` 的实现；`ElementExt::to_string()` = `render(None).to_string()` 走 Display → **落盘行全是纯文本**。`write_ansi()`（pub）才是带色的输出路径。
- **修复**：落盘改 `row_element(...).render(None).write_ansi(&mut buf)` + trim 行尾 `\r\n` 再 println。PTY 验证：落盘字节含 truecolor/256 色码。

### D20e. resize 视口重绘（对标 CC log-update fullReset / OffscreenFreeze 语义）

- **症状**：窗口 resize 后样式乱——落盘行按旧宽度折行残留，动态区重排后错位。
- **CC 语义实证**（用户指出的关键点）：**视口内的内容不是 Static 的**——`OffscreenFreeze`（components/OffscreenFreeze.tsx）只在内容滚出视口后才冻结；`Message` memo 注释 "Static messages must re-render on terminal resize"；resize 走 `fullResetSequence_CAUSES_FLICKER`（log-update.ts）：clearTerminal + 按新宽度全量重写（含内存累积的 fullStaticOutput）。
- **实现**：inline 模式尺寸变化（last_size 检测，首帧除外）→ 置 `chat.dirty`（强制按新宽度重建 doc——tick 未跑时 dirty 未置位，否则重播用旧宽度文档）→ 置 replay 标志 → 落盘阶段执行：`\x1b[2J\x1b[H`（清可见区 + 回顶）+ 按新宽度重绘"视口内的落盘行"（`rows[printed-N..printed]`，N ≈ 屏高−动态区高，视口外 scrollback 保持原样）；printed 不重复计数。
- **连带**：`reply_cache`（markdown 渲染缓存）不区分宽度 → 宽度变化时清空（`prev_build_width` 跟踪），否则消息文本沿用旧宽度折行。
- **排坑**：初版用 `width_changed` 强制 build_rows 导致 mock 渲染循环停滞（帧调度破坏），改"尺寸变化 → 置 dirty"走正常重建路径。
- 验证：178 测试全过；PTY 100→60 resize：2J 恰 1 次、重播行按 60 折行（welcome 窄栏文本溢出与用户消息不折行为既有布局行为）。

### D20f. 频繁 resize panic（落盘游标越界）

- **症状**：连续 resize 时 `thread 'main' panicked at components.rs: range end index 28 out of range for slice of length 26`。
- **根因**：resize 宽度变化 → `build_rows` 重排后 `doc.rows` 行数收缩（窄宽度折行更少），但落盘游标 `printed` 仍是旧值（> 新 `doc.rows.len()`）；落盘 slice `rows[*p..live_start]` 与重播 slice `rows[*p-replay_n..*p]` 越界。`live_start = max(printed, ...)` 也依赖未 clamp 的 printed。
- **修复**：inline 分支开头（`live_start` 计算**之前**）clamp 落盘游标 `*p = min(*p, doc.rows.len())`——全部 slice 恢复安全。
- 验证：随机宽度（40-120）快速切换 25 次 × 5 轮 + 期间发消息，无 panic；178 测试全过。

### D20g. 输入真实光标（iocraft 无光标 API 的布局等价）

- **诉求**：输入时终端真实光标应停在输入文本后（CC 体验），而非 `▋` 假光标"在最后飘"。
- **CC 机制**：ink 的 `frame.cursor`——渲染循环输出后把真实光标定位到组件声明位置（PromptInput 的 cursorOffset）；视口外才冻结（OffscreenFreeze）。
- **iocraft 0.8.4 无此 API**（渲染后光标固定停在 canvas 末行末尾；TextInput 也是假光标——absolute 定位色块）。等价实现：**输入行做成 canvas 最后一行**——iocraft 渲染后真实光标自然停在输入文本末尾。inline 模式 chrome 顺序调整：tasks/warn/waiting/footer/上边框/**输入行（最后）**；输入行去掉 `▋` 假光标。全屏模式保持原布局（▋ 假光标）。
- **连带排查（未修，环境特异）**：pty 测试环境下键盘事件偶发不达——iocraft `Terminal::new` 同步调用 `crossterm::supports_keyboard_enhancement()`（发 `\x1b[?u` 等响应，~2.5s 超时），无响应的 pty 里该查询可能吞掉窗口期输入；HEAD 基线（全屏）同样复现 → 非本改动引入；真实终端有响应、无感（用户实际打字正常）。

### D20h. 输入框回归 CC 完整框 + 真实光标方案否决

- **症状**：D20g 的"输入行最后"布局（无下边框、footer 上移）被用户否——"输入框不对 并且无法输入"。
- **复盘**：用户截图 ❯ 后文字可见（输入实际工作）——"无法输入"实为光标感知问题：D20g 去掉 ▋ 假光标后，iocraft 渲染完的真实光标停在 canvas 末行（不跟随输入），用户看不到输入位置。真实光标跟随（ink frame.cursor）在 iocraft 0.8.4 不可实现（无组件声明光标 API）。
- **修复**：恢复 CC 完整输入框（上边框 + `❯ {input}▋` + 下边框 + footer 下方）；inline 模式输出 `\x1b[?25l` 隐藏真实终端光标（避免 footer 行尾真光标与 ▋ 混淆），退出时 iocraft 自动 Show。验证：上下边框/▋/光标隐藏序列齐全，178 测试全过。

### D20i. 事件全断根因：渲染期间写 State 的渲染风暴（clamp 每帧 write）

- **症状**：真实终端（用户 + tmux 复现）打字/Ctrl+C/resize 全部失效；pty 环境显示启动输出膨胀到 122KB（每帧全量重画）。
- **根因**：D20f 的落盘游标 clamp `*p = (*p).min(doc.rows.len())` **每帧执行**——iocraft `State::write`（DerefMut）**无条件**标记 `did_change` 并 `waker.wake()`（use_state.rs:149-163，即使值相同）——组件**渲染期间写 state → 唤醒 → 立即再渲染** → 渲染风暴 → 渲染循环的 `select(root.wait(), term.wait())` 里 `root.wait()` 永远 ready → **`term.wait()` 饿死 → 终端事件（键盘/resize）全部不达**。Ctrl+C 退出、resize 重播（依赖 Resize 事件触发 replay）随之失效——用户"resize 功能没了"实为同一根因。
- **修复**：clamp 只在值变化时写（`if clamped != *p { *p = clamped; }`）。排查了全部渲染期 state 写点：落盘/replay/cursor_hidden/last_size 均有条件守卫，唯 clamp 遗漏。
- **验证**（tmux 真实终端，查询有响应可完整复现）：打字 `❯ zz▋` ✓；Ctrl+C 退出（pgrep 0）✓；resize 100→60 视口重播（welcome 60 宽重排）✓。178 测试全过。

### D21. `!` 命令（bash 模式，对标 CC processBashCommand）

> 来源：2.1.221 二进制 `processBashCommand`（`output/clean/cli.bundle.min.js` 16570112 段）+ 输入侧 `Rhn`/`c1`/`K7`（8573686 段）与 REPL 按键（21337083 段）。行号见 runtime-diff 语义。

- **输入面**（对标 CC）：输入为空时按 `!` 进入 shell 模式（`!` 本身不插入输入，`Rhn` 判定）；bash 模式下空输入按退格退出；输入非空时 `!` 正常插入。模式是**粘性**的（提交后保留）。UI：输入前缀 `!` + 输入框边框换 `bashBorder` 色（CC `prefixColor/bashBorder`）；footer 提示 `! for shell mode`。
- **执行面**（对标 CC processBashCommand）：**不经模型、不经 UserPromptSubmit hooks**——命令经统一权限门（PreToolUse hook + canUseTool + 用户确认）+ Bash 工具 + PostToolUse hook 执行；UI 复用现有工具活动行（ToolUseStart/ToolReady/ToolDone）。历史按真实工具轮形状写入（`<bash-input>` 用户文本 → 合成 assistant ToolUse → 用户 ToolResult——**API 要求 tool_result 必须与同请求内 tool_use 配对**），输出 HTML 实体转义（`& < >`，对标 CC `Qa`）后包裹 `<bash-stdout>`。
- **模型回应**（对标 CC `respondToBashCommands`，settings 键同名，默认 true）：true → 执行后照常进入 queryLoop（模型可见输出并可继续）；false → 纯执行，并在历史前注入 caveat（CC `VDe`：`<local-command-caveat>` "DO NOT respond to these messages…"）防模型把输出当指令。中断/PostToolUse 阻断同样走"不查模型 + caveat"路径。
- **实现落点**：`run_query` 拆出 `query_loop`（循环体）+ `tool_context`，`run_bash_command` 与 `run_query` 共用；TUI `Chat` 新增 `bash_mode` 状态与 `start_bash_turn`。
- **取舍**：stdout/stderr 不再分离（CC `<bash-stdout>`/`<bash-stderr>` 双标签）——bingo 的 Bash 工具本就合并输出且含 `$ cmd` 回显与退出码，模型信息不缺失；周期/后台命令（`!watch …`）直接复用工具的后台化语义。权限拒绝也回填模型（与 run_query 的 `<permission_error>` 惯例一致，CC 同为"失败也查模型"）。
- **验证**：184 测试全过（新增：`!` 切换、bash 提交收尾、执行消息形状与转义、组件渲染前缀/边框/提示、settings 合并）；PTY 冒烟：`!` 前缀 + `! for shell mode` 提示 + `!echo hello` 直接执行（`✓ Bash $ echo hello · 9ms … +3 lines`）后保持 bash 模式。

### D21b. 交互式/TTY 命令拒绝（`!` 与 Bash 工具共用）

- **动机**：bingo 子进程 stdin/stdout 均为管道但**继承控制终端**——全屏 TUI（top/htop/vim）输出乱码、ssh/fzf/sudo 直连 `/dev/tty` 抢占终端（raw mode 下画面被撕毁）、裸 shell/REPL 无输入即退出（无意义）。CC 侧只做提示（"Interactive terminal apps can't be driven by an agent's bash tool" + tmux 包装惯例），bingo 直接执行前拒绝。
- **实现**：`tool/bash.rs::interactive_command_reason`（`!` 与 Bash 工具共用）——解包 sudo/env/nohup/command/exec/doas 包装后按命令名与参数判定：
  - **恒拒**：系统监控（top/htop/btop…，`-b/--batch` 快照放行）、编辑器（vim/nano/emacs…）、文件管理器（ranger/yazi/mc…）、TUI 工具（lazygit/tig/fzf/k9s/screen…）、`docker/kubectl attach` 与 `exec/run -it`、`tmux` 前台（`new -d`/send-keys/capture-pane 等脚本用法放行）、gdb（`-batch` 放行）。
  - **裸拒**：shell/REPL（bash/python/node…，带参数放行）；DB 客户端（sqlite3/psql/mysql/mongosh/redis-cli——无 `-c/-e/--eval` 等执行旗标、无 `<` 重定向、无 SQL/脚本位置参数即交互提示符；`--version/-l` 等非交互用法放行）。
  - **ssh**：`-t` 强制 tty 或"仅主机无远程命令"（口令/远程 shell 占 `/dev/tty`）→ 拒；`ssh host 'cmd'` 与 `-N/-f`（端口转发/后台）放行。`sudo -i/-s` 与裸 sudo → 拒。
- **落点**：`BashTool::call` 顶部（模型路径生效，`<tool_use_error>` 回填）；`run_bash_command` 在**权限门之前**预检（`!top` 不弹无意义的权限询问；respond 开启时模型可见拒绝原因并可提示替代，如 `top -b -n 1`）并直接发 `Warning` 事件——折叠行在 inline 模式落盘后无法展开，拒绝原因必须以警告行呈现。工具描述同步声明交互式命令被拒。
- **验证**：187 测试全过（新增拒绝/放行正反例 ~60 条 + 工具层与 `!` 路径拒绝断言）；PTY 冒烟：`!top` → `⚠ interactive command not allowed: top 是全屏交互监控程序（需要 TTY），已拒绝。一次性快照可用 \`top -b -n 1\``，命令未执行、回到 bash 模式提示。

### D21c. `!` 命令输出预览（对标 CC BashModeProgress）

- **症状**：`!pwd`/`!ls` 执行后输出被折叠组吞掉——折叠摘要行（"Ran 1 bash command"）不含输出，且 inline 模式落盘后无法展开，用户看不到命令结果。
- **CC 机制**：BashModeProgress = `<bash-input>` 行 + `kEt`（与普通工具结果同款渲染器）的 fullOutput——输出直接展示在命令下方（长输出折叠 "+N lines"）。
- **实现**：`UiHooks.on_tool_ready` 与 `UiEvent::ToolReady` 增 `standalone: bool`——`!` 命令的 Bash 活动标记 standalone：只设摘要、**不参与折叠组**（模型驱动路径传 false，折叠行为不变）；ToolDone 时非折叠 Bash 活动**默认展开**，内容 = 输出去掉 `$ cmd` 回显与 `[Exited with code N]` 尾注（`bash_output_preview`），复用 layout_activity 既有的 `⎿` 连接行渲染。
- **验证**：189 测试全过（新增 standalone 折叠判定正反例、预览展开与剥离断言）；PTY 冒烟：`!pwd` → `⏺ ✓ Bash $ pwd · 8ms` + `⎿ /Users/yexrob/Episodes/Projects/bingo`；`!ls` → `⎿ AGENTS.md` + 缩进续行，多行输出完整可见。

### D22. AskUserQuestion 工具（问用户选择题，对标 CC）

> 来源：2.1.221 二进制 schema（8272642 段，`c3d`/`u3d`/`NAy`）+ 模态 UI（18940365 段）。CC 侧 `askUserQuestionTimeout` 设置默认 "never"（无限等待）。

- **契约**：`questions[1..=4]`，每题 `{question, header?（≤12 字符）, options[2..=4] {label, description?}, multiSelect?}`；问题文本与 option label 各自唯一（`a3d` 校验）。v1 实现：`multiSelect: true` 报错引导改单选；`preview` 字段不纳入 schema（UI 无预览面板）；header 缺失时以「问题 N」为题。
- **执行**：`src/tool/ask.rs`——`is_concurrency_safe=false`（阻塞等待回答，串行）；逐题调用 `ToolContext.ask_question`（None = Esc 跳过，后续问题不再问）；结果 `The user answered: "q"="a", ...`（CC 格式）或 `The user did not answer the questions.`；模型端输入校验失败（数量/唯一性）以 `tool_use_error` 回填。
- **UI 复用**：AskUserQuestion 走既有权限模态（`PermissionRequest` + 1-9 键 + Esc）——`UiHooks.ask_question` 与 `ask` 同通道，`Confirm(i) → Some(i)`、`Cancel → None`。**不经权限门**（对话框本身即审批；run_query 门内按名短路）。工具行摘要显示问题文本。子代理（Agent 工具）的 ask_question 恒 None（无 UI 可问）。
- **连带修复**：`schema_for` 此前丢弃 schemars generator 的 definitions——嵌套类型（`Vec<AskQuestion>` 经 `$ref` 引用）发给模型的 schema 悬空。现把 `generator.definitions()` 合并进根 schema（对扁平工具无影响）。
- **取舍**：多选/Other 自由文本/预览面板为后续项（CC 的 `Wr` 多选组件与文本输入需扩展模态协议）；超时不做（对齐 CC 默认 never）。
- **验证**：196 测试全过（新增：schema 形状含 definitions、输入校验反例、回答/跳过/多选拒绝、执行队列串行化、TUI 挂钩 Confirm/Cancel 映射）。

### D24. MCP 管理：McpManager 连接缓存 + /mcp 命令（对标 CC McpHub）

> 来源：2.1.88 泄露源码 `services/mcp/{config,types,useManageMCPConnections}.ts` + `commands/mcp/mcp.tsx`（`claude mcp` CLI 子命令见 `cli/handlers/mcp.tsx`）。

- **CC 机制**：启动并行连接全部 server（batch，失败不阻塞）；连接状态 connected/failed/needs-auth/pending/disabled 进 AppState；SSE 断线指数退避重连（5 次，1s→30s）；`mcp__{server}__{tool}` 前缀进 ToolRegistry，ToolListChangedNotification 动态刷新；enable/disable 持久化 `disabledMcpServers`/`enabledMcpServers` 名单（project config）；`/mcp` immediate 命令 = MCPSettings 交互 UI（状态徽标 + fork/重连/日志/删除菜单）+ `/mcp enable|disable [name|all]` + `/mcp reconnect <name>` 快速路径。
- **bingo 现状改造**：原 `connect_servers` 每次回合 spawn 子进程重连（浪费）。新增 **`McpManager`**（挂 `Session.runtime.mcp`）：**懒连接**（首个回合 `connect_all`，之后复用缓存）、失败记录不自动重试（`/mcp reconnect` 手动，对比 CC 仅 SSE 自动重连——stdio 子进程退出即彻底失败，自动重连无意义）、`disconnect`/`set_enabled` 立即生效。
- **/mcp 命令**（对齐 CC argumentHint `[enable|disable [server-name]]`）：无参数列出（✓ connected · N tools / ✗ failed: 详情 / ○ disabled / · not connected）；`enable|disable [name|all]` 更新名单并**持久化 `.bingo/settings.json` 顶层 `disabledMcpServers`**（CC 同名机制）；`reconnect <name>`（disabled 时拦截提示先启用）。
- **配置契约**：`McpServerConfig` 增 `type` 字段（对齐 CC TransportSchema）；仅 stdio 落地，sse/http/ws 连接时报错提示（rmcp 3.1 client 仅 stdio，D3）。
- **权限**：MCP 工具复用统一权限门（Box<dyn Tool> 已有）；is_concurrency_safe=false（串行，CC 有并发标记但 bingo 保守）。
- **验证**：244 测试全过（新增 McpManager 状态矩阵/失败不重试/reconnect 清失败 + /mcp 列表/enable-disable 持久化/reconnect 拦截）；tmux 实测（无依赖 Node stdio server）：懒连接 2 tools、badsrv failed + 警告行、disable 断开+持久化跨会话、disabled reconnect 拦截、enable 后下回合自动连接。



> 来源：2.1.221 二进制 `Psf`/`fsf`（14780240 段）、`tAa` 消费点（21698383 段，REPL 布局中位于工具行之后、输入框之前）、`dkn` 状态机（14768125 段）、thinking 词表（14752000 段，~150 词）。

- **CC 机制全景**（让用户知道 agent 在运行）：
  1. **状态行（ActivityIndicator）**：transcript 底部、输入框上方一行——spinner（100ms 帧）+ 动词消息（`{verb}…`）+ thinking 计时（`(thinking for 12s)`）+ 工具计时（`running tool for 3.2s`）+ 输出 token 计数（`↓ N tokens`）+ 总耗时。动词 = 运行中工具的 activeForm/subject > thinking 俏皮词 > 兜底 "Working"。**无论模型在想、在等、在跑工具，这行永远存在**。
  2. **thinking 占位**：`⠋ Mulling for 1.4s`（~150 俏皮词表）。
  3. **工具行**：`kEt` 输出预览——无输出时 `Running…` + elapsed，有输出时尾 5 行 + `~N lines`/`+N lines` + `(timeout 2m)`。
  4. **工具心跳**：30s 无输出心跳 progress，长任务 elapsed 持续刷新。
  5. **stall 检测**：距上次 token 10s/45s/300s 阈值，spinner 降强度/变 warning 色；429 显示 `Waiting for API response · will retry in X · check your network`。
  6. **spinner tips**：运行 >30s 提示 `/btw`、>30min 提示 `/clear`；有任务时 `Next: {subject}`。
- **bingo 实现**：`Chat::running_status()`（busy 时返回 `(动词, 耗时)`——运行中工具 summary > thinking 俏皮词 > "Working"；`turn_started` Instant 由 TurnStart/TurnEnd 设置）+ `status_row` 渲染在输入框上方（chrome 一行，inline/全屏均可见；任务区与警告行之间）。动词优先工具 summary（`$ sleep 2`），与 CC activeForm 语义一致。
- **验证**：198 测试全过（新增 `running_status` 动词优先级、状态行渲染断言）；PTY 实测 `!sleep 2`：`⠼ $ sleep 2 for 0.2s → … → 2.0s` 逐帧跳动，回合结束消失。
- **遗留（上游 iocraft 问题，非本改动引入）**：API 完全挂起（无任何事件）时，tick 驱动的渲染链会在 ~1s 内饿死——spinner/计时冻结在提交瞬间（基线同现；探针时序可复现/可绕过）。事件流正常时（含真实 API 的流式往返）无此问题。状态行至少在冻结前给出"Working"可见提示；彻底修复需在 iocraft 渲染循环的唤醒链上动手（`select(root.wait(), term.wait())` 对自驱动动画的唤醒竞态）。

