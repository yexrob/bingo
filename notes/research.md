# bingo 技术决策记录

> 目标：Rust 实现的 agent CLI。
> 决策日期：2026-08-04。事实均以 2026-08 实抓 crates.io/docs.rs/GitHub 为准。

## 架构总览

```text
┌─────────────────────────────────────────────────────────────────────┐
│ L1  CLI 入口 · clap (D8)                                             │
│  --version/--help 快路径 → 环境消毒 → settings 预读 → MCP 连接         │
│  → 分流：TUI（iocraft）｜ headless --print                            │
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

- `modelcontextprotocol/rust-sdk` 官方，3.1.0（2026-07-31），client 能力：**stdio**（TokioChildProcess）+ **streamable HTTP**（`transport-streamable-http-client-reqwest`，rmcp 3.1 强制 reqwest 0.13，故 bingo 的 reqwest 升 0.13 统一栈）；OAuth 为 SDK 能力未启用（静态 `headers` 头覆盖常见鉴权）。
- mcpServers 配置 → 连接 → list_tools → 适配成同一 Tool trait（isMcp + mcpInfo）。
- 不碰其它 MCP crate（mcp-server / mcplease 无 client 或初级）。

### D4. TUI：iocraft（声明式组件）

- `iocraft` 0.8.4（声明式 hooks + flexbox + fullscreen render loop），组件架构与 ink 同构；`rsmarkdown-core`（markdown 流式解析）保留做 AST → 行渲染。
- bingo 只做组件接线：Chat 组件接 stream 事件、权限卡接 canUseTool、Task → tasks、Agent 工具 → agents。

### D5. 运行时与进程

- tokio 单 runtime；crossterm EventStream + `tokio::select!` 事件循环；工具执行 JoinHandle/AbortHandle；输入打断 = select + watch channel。
- Bash 工具：`tokio::process` + `/bin/zsh -c`（goose 同款无 pty；shlex 未用）；交互式 shell 再上 `portable-pty`。

### D6. Token 计数：官方 count_tokens API

- Claude tokenizer 闭源，`claude-tokenizer` 停更不准。
- 预算显示走 `POST /v1/messages/count_tokens`；本地估算用 `claude-tokenizer` 兜底不当真。

### D7. 主循环语义

- `stop_reason === 'tool_use'` 不可靠，以实际出现的 tool_use 块为准。
- 并发执行队列：safe 工具并行（上限 10），非 safe 串行且不越过前面未完成的写。

### D8. CLI 入口：clap + 显式启动链

- 参数解析用 `clap`（derive）。
- 启动链：`--version`/`--help` 快路径（不加载重模块）→ 环境消毒 → settings 预读 → MCP 连接 → 交互（TUI）或 headless（--print/-p）分流。
- 初始参数面：`--model`、`--permission-mode`、`--continue`、`--add-dir`、`-p/--print`（headless）。

### D9. 配置分层：settings.json + feature flags

- 配置分层：user（`~/.config/bingo/settings.json`）/ project（`.bingo/settings.json`）/ local（`.bingo/local.json`），浅层合并，本地不入库。
- settings 承载：permissionMode、hooks 配置、mcpServers、主题/通知偏好。
- feature flags：编译期 `feature()`（bundle DCE 等价物）+ 运行期开关；新增能力默认关。不引入 GrowthBook——本地 CLI 不需要远程下发。

### D10. 系统提示拼装 + prompt caching 策略

- system prompt 分段拼装：base（角色/规则）→ 工具说明 → CLAUDE.md 记忆层（managed/user/project/--add-dir）→ 会话附加段。
- 分段顺序即缓存策略：tools → system 顺序缓存（`cache_control: ephemeral` 放在 system 与 messages 末尾断点，最小缓存量 512~4096 token），保证多轮只换尾部。

### D11. Transcript 与会话存储

- 每次会话持久化 transcript（JSON Lines）；支持 `--continue`/`--resume` 恢复。
- Compact 以 transcript 为边界：压缩前存档、压缩后生成摘要段替换。

### D12. token budget 管理

- 输出侧：max_tokens 动态管理（升级路径）与 turn 输出预算（超限 continuation）。
- 输入侧：token 计数走 D6，达到阈值触发 autoCompact。

### D13. Sandbox 与遥测：显式不做（初期）

- Sandbox 不做，权限门 + 模式即安全边界；记录为后续项。
- 遥测不做；仅本地 debug 日志。避免引入 analytics 依赖。

### D14. 多代理边界

- 首期只做子代理（Agent 工具递归 queryLoop，subagent 独立消息历史）。
- worktree/teammate/团队协作面不做（产品化面，非核心 harness）；后续按需。

### D15. 任务追踪：v2 Task 工具族

- **工具面**：`TaskCreate`（subject/description/activeForm?/metadata?，一次一任务，输出 `{task:{id,subject}}`）、`TaskUpdate`（taskId + 可选字段，**增量 patch 语义**，status 增 `deleted` 永久删除并清理他任务引用）、`TaskGet`、`TaskList`（过滤 `metadata._internal`，completed 从 blockedBy 剔除）。共同属性：shouldDefer、无权限检查、renderToolUseMessage null（UI 走任务区）、调工具时展开任务区。
- **存储**：磁盘 `~/.local/share/bingo/tasks/<listId>/<taskId>.json`，每任务一文件，**跨会话持久化**；数字 id 递增（max+1）；读时逐条容错解析；进程内互斥（bingo 单进程，无跨进程并发）。
- **输入修复层**：把近似的 key 名修复（title/name→subject、content→description、active_form→activeForm、task 包裹拆包、缺 description 时 backfill），误用（tasks/todos 数组参数、Agent 参数）返回引导文案。
- **Hooks**：新增 `TaskCreated` / `TaskCompleted` 事件；TaskCompleted 的 blockingError 可**拒绝** completed 状态。
- **提醒注入**：`task_reminder` 消息，阈值 `TURNS_SINCE_WRITE=10` / `TURNS_BETWEEN_REMINDERS=10`；工具表含 TaskUpdate 时注入；meta user message 注入 + "NEVER mention this reminder"。
- **bingo 取法**：v2 增量语义（v1 全列表覆盖写在并发下是丢失更新温床）+ 磁盘持久化 + 单文件锁（与 transcript 文件习惯同构）；**砍** owner/swarm 分配（D14 已定不做 teammate）与 metadata merge（首版支持即可）；TaskCompleted 阻断 hook 对齐现有 hooks 语义；reminder 阈值直接取 10/10。

## 实现清单（按实现顺序）

1. headless 最小闭环：API 客户端 + queryLoop + Read/Bash 工具 + 权限门（D1/D2/D7/D8）
2. 并发队列 + Hooks 运行时（shell hook，JSON stdin/stdout）
3. 系统提示拼装 + transcript 存储 + token budget（D10/D11/D12）
4. MCP 接入（rmcp → Tool 适配）
5. TUI 接线（iocraft）+ slash 命令
6. Compact + CLAUDE.md/memdir 记忆 + 子代理（Agent 工具）
7. 后续：sandbox、plugins、worktree/teammate（D13/D14 暂缓项）

## 参考

- goose（aaif-goose/goose，纯 Rust agent，permission 门 + execution + agents 结构）
- iocraft README（组件 API、渲染循环、hooks 语义）
- [`notes/design/feedback-states.md`](./design/feedback-states.md)（反馈状态规范：用户可见反馈态的统一设计约定，含 GUI/CLI 两侧与错误码契约）

## 已定决策（续）

### D16. TUI 渲染层：iocraft 声明式组件（迁移自 ratatui 系）

- **动因**：声明式组件（hooks + flexbox + fullscreen render loop）是终端 UI 的高效形态；迁移后 bingo 的 UI 架构与主流 agent TUI 同构，布局可 1:1 对标参考实现。
- **取舍**：`rsmarkdown-tui`（App/Component 框架、~12k 行）整体弃用；`rsmarkdown-core`（markdown 流式解析，显示无关）保留。渲染层全部重写为 iocraft 元素（不保留 ratatui Line 适配桥）。
- **新结构**：`src/tui/` — `chat.rs`（状态机 + 文档行构建，原样保留事件语义与折叠逻辑）、`line.rs`（样式化行模型）、`theme.rs`（dark 令牌）、`markdown.rs`（AST → 行，renderer.rs 移植）、`activities.rs`（活动数据 + 头部/折叠布局移植）、`components.rs`（iocraft 根组件 + transcript）。
- **布局对标点**：单列布局（无 sidebar）＝ sticky header + 滚动 transcript + 任务列表 + 通知行 + 输入行 + `╰──╯` 边框 + 1 行 footer；权限请求渲染在 transcript 底部（非模态）；消息块 marginTop=1；footer 左＝模式徽标（⏸ plan / ⏵⏵ accept edits）+ 快捷键 byline，右＝模型名。
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

### D18. 主题配置（`theme` 设置）

- **配置**：`settings.json` 新增 `"theme": "auto" | "dark" | "light"`（缺省 auto）。
- **auto 检测**：fullscreen 前临时进 raw mode 发 OSC 11 查询终端真实背景色（`ESC ] 11 ; ? ESC \`），按 BT.709 相对亮度判断深浅；其次 `$COLORFGBG` 种子；都无则回落 dark。坑：OSC 回复不带换行，规范模式行缓冲会吞掉，必须 raw mode 下读。
- **令牌**：dark/light 两套语义令牌（light 正文黑 `rgb(0,0,0)`、`userMessageBackground` 240、主强调橙两主题相同）。不支持 truecolor 时 RGB 降级 256 色（AnsiValue cube 近似）。
- **欢迎页标题**：`Welcome back` 用主强调橙，非白色。

### D19. 流式残影根治：事件级强制全清（diff 路径残留）

- **症状（用户真实终端 Ghostty，tmux 不可复现）**：流式正文增长时出现"半截覆盖"——新内容覆盖旧行部分区域，行尾残留旧字符；TurnEnd 全清后恢复。
- **排查**：trace 证实 FORCE 全清链路本身正确（每次 doc 行数变化后 hook 都消费并全量重写，0 失配）；tmux/pty/模拟终端均无法复现 → 问题在 **diff 路径**：内容在行内增长（行数不变）时走 iocraft 行 diff，真实终端下残留旧行。DeepWiki 协查：`write_ansi_row_without_newline` 理论总会清行尾，row_eq 裁剪比较在背景色/填充场景可能误判相等（issue #142 族）。
- **修复**：**任何事件处理（`drain_all` 返回 true，覆盖 TextDelta/ThinkingDelta/ToolStart 等）→ 立即置 `FORCE_FULL_REDRAW`** → 内容变化的帧全部走全量清除重绘，绕开行 diff。synchronized update（2026）下同帧原子完成，无闪烁；DeepWiki 确认该模式为 iocraft 惯用法（`use_output` 内部同款）。

### D20. 交互默认改 REPL inline 模式（非全屏，iocraft use_output）

- **背景**：原默认全屏 iocraft canvas（alt screen + app 内视口滚动 + 输入吸底）被用户感知为"大的应用壳子、强制刷屏"；参考实现默认（用户环境）是非全屏：定稿内容像普通终端输出一样落 scrollback，滚轮翻历史，prompt 在对话末尾而非钉屏底。
- **机制**：非全屏 = print-and-forget（定稿消息渲染一次写入 stdout，永不复写）+ 动态尾部（流式/spinner/输入行）原地重绘（相对光标行 diff）；滚动离开视口的行冻结，避免全量重刷。
- **选型**：先手写 REPL 驱动（ANSI 序列化 + 回卷/落盘记账 + 自管 crossterm 事件循环，~500 行，存于 `git stash` 可找回）验证机制；后确认 **iocraft 0.8.4 原生 `use_output` 即等价物**（`hooks/use_output.rs:74` exec：`clear_terminal_output` → 写 stdout → 渲染循环重画）→ 弃手写，组件层复用（markdown/主题/活动布局/`row_element` + `to_string()` 离屏渲染）。
- **实现**：`Bingo` 组件新增 `inline` 模式——`doc.settled`（定稿边界，D19 前已有机制）前进时逐行 `println(row_element(row).to_string())` 落盘（多行块必须逐行 println：raw mode OPOST 关，块内 `\n` 不回列首会阶梯错位，rsink demo 实证）；Transcript 渲染 `rows[tail_start..]` 尾部切片（落盘边界 = max(printed, settled, len−max_live)，canvas 高度恒 ≤ 终端，inline 擦除不污染 scrollback）；按键 gate：空闲 Esc 忽略 / ctrl+o 只放行未定稿消息 / 空闲 Ctrl+C → `system.exit()`（须 `ignore_ctrl_c()` 让事件派发给组件，busy 时 Ctrl+C 走 on_key 取消）；`--fullscreen` 保留原 canvas 路径。
- **连带修复**：`detect_system_theme` 原用 `tokio::fs` 读 `/dev/tty`——timeout 后 blocking 线程 read() 不可取消，会吞掉之后第一个输入且 tokio 关停 join 挂起（进程退不出）；改 `std::fs` + `O_NONBLOCK` + 轮询（libc 依赖，已锁定）。
- **验证**：PTY 冒烟（python pty + winsize 100x24）：欢迎卡片落盘、无 alt screen、输入逐键反映、thinking spinner 动态区回卷重绘、Ctrl+C 退出 6/6 稳定；测试 178 全过。

### D20b. inline 模式两处修正（启动清屏 + 输入吸底）

- **启动清屏**：`ForceRedrawOnResize` 首次检测（`last=None` 视为尺寸变化）+ 首帧 `FORCE_FULL_REDRAW`（doc 行数 0→N）→ 启动第一帧 `clear_terminal_output()` 清掉 shell 残留。修复：首次 poll 只记录基准尺寸不算变化；`post_component_update` 首帧跳过清屏（`first` 标志）。全屏模式不受影响（进 alt screen 本就空屏）。
- **输入吸底**：inline 模式根 View 固定 `height: height`（全屏设计）→ canvas 恒占满终端、输入行钉在屏幕最底行。修复：inline 根 View 不固定高度（内容自然高度）、Transcript `flex_grow: 0`——canvas 高度 = 尾部 + chrome 实际行数（空闲时 3 行），输入行随内容流走，与非全屏一致。
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

### D20e. resize 视口重绘（fullReset / OffscreenFreeze 语义）

- **症状**：窗口 resize 后样式乱——落盘行按旧宽度折行残留，动态区重排后错位。
- **语义实证**（用户指出的关键点）：**视口内的内容不是 Static 的**——内容只在滚出视口后才冻结；消息重渲染依赖 resize；resize 走 fullReset：clearTerminal + 按新宽度全量重写（含内存累积的 fullStaticOutput）。
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

- **诉求**：输入时终端真实光标应停在输入文本后（参考体验），而非 `▋` 假光标"在最后飘"。
- **机制**：渲染循环输出后把真实光标定位到组件声明位置（输入框的 cursorOffset）；视口外才冻结。
- **iocraft 0.8.4 无此 API**（渲染后光标固定停在 canvas 末行末尾；TextInput 也是假光标——absolute 定位色块）。等价实现：**输入行做成 canvas 最后一行**——iocraft 渲染后真实光标自然停在输入文本末尾。inline 模式 chrome 顺序调整：tasks/warn/waiting/footer/上边框/**输入行（最后）**；输入行去掉 `▋` 假光标。全屏模式保持原布局（▋ 假光标）。
- **连带排查（未修，环境特异）**：pty 测试环境下键盘事件偶发不达——iocraft `Terminal::new` 同步调用 `crossterm::supports_keyboard_enhancement()`（发 `\x1b[?u` 等响应，~2.5s 超时），无响应的 pty 里该查询可能吞掉窗口期输入；HEAD 基线（全屏）同样复现 → 非本改动引入；真实终端有响应、无感（用户实际打字正常）。

### D20h. 输入框回归完整框 + 真实光标方案否决

- **症状**：D20g 的"输入行最后"布局（无下边框、footer 上移）被用户否——"输入框不对 并且无法输入"。
- **复盘**：用户截图 ❯ 后文字可见（输入实际工作）——"无法输入"实为光标感知问题：D20g 去掉 ▋ 假光标后，iocraft 渲染完的真实光标停在 canvas 末行（不跟随输入），用户看不到输入位置。真实光标跟随（ink frame.cursor）在 iocraft 0.8.4 不可实现（无组件声明光标 API）。
- **修复**：恢复完整输入框（上边框 + `❯ {input}▋` + 下边框 + footer 下方）；inline 模式输出 `\x1b[?25l` 隐藏真实终端光标（避免 footer 行尾真光标与 ▋ 混淆），退出时 iocraft 自动 Show。验证：上下边框/▋/光标隐藏序列齐全，178 测试全过。

### D20i. 事件全断根因：渲染期间写 State 的渲染风暴（clamp 每帧 write）

- **症状**：真实终端（用户 + tmux 复现）打字/Ctrl+C/resize 全部失效；pty 环境显示启动输出膨胀到 122KB（每帧全量重画）。
- **根因**：D20f 的落盘游标 clamp `*p = (*p).min(doc.rows.len())` **每帧执行**——iocraft `State::write`（DerefMut）**无条件**标记 `did_change` 并 `waker.wake()`（use_state.rs:149-163，即使值相同）——组件**渲染期间写 state → 唤醒 → 立即再渲染** → 渲染风暴 → 渲染循环的 `select(root.wait(), term.wait())` 里 `root.wait()` 永远 ready → **`term.wait()` 饿死 → 终端事件（键盘/resize）全部不达**。Ctrl+C 退出、resize 重播（依赖 Resize 事件触发 replay）随之失效——用户"resize 功能没了"实为同一根因。
- **修复**：clamp 只在值变化时写（`if clamped != *p { *p = clamped; }`）。排查了全部渲染期 state 写点：落盘/replay/cursor_hidden/last_size 均有条件守卫，唯 clamp 遗漏。
- **验证**（tmux 真实终端，查询有响应可完整复现）：打字 `❯ zz▋` ✓；Ctrl+C 退出（pgrep 0）✓；resize 100→60 视口重播（welcome 60 宽重排）✓。178 测试全过。

### D21. `!` 命令（bash 模式）

- **输入面**：输入为空时按 `!` 进入 shell 模式（`!` 本身不插入输入）；bash 模式下空输入按退格退出；输入非空时 `!` 正常插入。模式是**粘性**的（提交后保留）。UI：输入前缀 `!` + 输入框边框换 `bashBorder` 色；footer 提示 `! for shell mode`。
- **执行面**：**不经模型、不经 UserPromptSubmit hooks**——命令经统一权限门（PreToolUse hook + canUseTool + 用户确认）+ Bash 工具 + PostToolUse hook 执行；UI 复用现有工具活动行（ToolUseStart/ToolReady/ToolDone）。历史按真实工具轮形状写入（`<bash-input>` 用户文本 → 合成 assistant ToolUse → 用户 ToolResult——**API 要求 tool_result 必须与同请求内 tool_use 配对**），输出 HTML 实体转义（`& < >`）后包裹 `<bash-stdout>`。
- **模型回应**（`respondToBashCommands`，settings 键同名，默认 true）：true → 执行后照常进入 queryLoop（模型可见输出并可继续）；false → 纯执行，并在历史前注入 caveat（`<local-command-caveat>` "DO NOT respond to these messages…"）防模型把输出当指令。中断/PostToolUse 阻断同样走"不查模型 + caveat"路径。
- **实现落点**：`run_query` 拆出 `query_loop`（循环体）+ `tool_context`，`run_bash_command` 与 `run_query` 共用；TUI `Chat` 新增 `bash_mode` 状态与 `start_bash_turn`。
- **取舍**：stdout/stderr 不再分离（双标签）——bingo 的 Bash 工具本就合并输出且含 `$ cmd` 回显与退出码，模型信息不缺失；周期/后台命令（`!watch …`）直接复用工具的后台化语义。权限拒绝也回填模型（与 run_query 的 `<permission_error>` 惯例一致，失败也查模型）。
- **验证**：184 测试全过（新增：`!` 切换、bash 提交收尾、执行消息形状与转义、组件渲染前缀/边框/提示、settings 合并）；PTY 冒烟：`!` 前缀 + `! for shell mode` 提示 + `!echo hello` 直接执行（`✓ Bash $ echo hello · 9ms … +3 lines`）后保持 bash 模式。

### D21b. 交互式/TTY 命令拒绝（`!` 与 Bash 工具共用）

- **动机**：bingo 子进程 stdin/stdout 均为管道但**继承控制终端**——全屏 TUI（top/htop/vim）输出乱码、ssh/fzf/sudo 直连 `/dev/tty` 抢占终端（raw mode 下画面被撕毁）、裸 shell/REPL 无输入即退出（无意义）。参考实现侧只做提示（"Interactive terminal apps can't be driven by an agent's bash tool" + tmux 包装惯例），bingo 直接执行前拒绝。
- **实现**：`tool/bash.rs::interactive_command_reason`（`!` 与 Bash 工具共用）——解包 sudo/env/nohup/command/exec/doas 包装后按命令名与参数判定：
  - **恒拒**：系统监控（top/htop/btop…，`-b/--batch` 快照放行）、编辑器（vim/nano/emacs…）、文件管理器（ranger/yazi/mc…）、TUI 工具（lazygit/tig/fzf/k9s/screen…）、`docker/kubectl attach` 与 `exec/run -it`、`tmux` 前台（`new -d`/send-keys/capture-pane 等脚本用法放行）、gdb（`-batch` 放行）。
  - **裸拒**：shell/REPL（bash/python/node…，带参数放行）；DB 客户端（sqlite3/psql/mysql/mongosh/redis-cli——无 `-c/-e/--eval` 等执行旗标、无 `<` 重定向、无 SQL/脚本位置参数即交互提示符；`--version/-l` 等非交互用法放行）。
  - **ssh**：`-t` 强制 tty 或"仅主机无远程命令"（口令/远程 shell 占 `/dev/tty`）→ 拒；`ssh host 'cmd'` 与 `-N/-f`（端口转发/后台）放行。`sudo -i/-s` 与裸 sudo → 拒。
- **落点**：`BashTool::call` 顶部（模型路径生效，`<tool_use_error>` 回填）；`run_bash_command` 在**权限门之前**预检（`!top` 不弹无意义的权限询问；respond 开启时模型可见拒绝原因并可提示替代，如 `top -b -n 1`）并直接发 `Warning` 事件——折叠行在 inline 模式落盘后无法展开，拒绝原因必须以警告行呈现。工具描述同步声明交互式命令被拒。
- **验证**：187 测试全过（新增拒绝/放行正反例 ~60 条 + 工具层与 `!` 路径拒绝断言）；PTY 冒烟：`!top` → `⚠ interactive command not allowed: top 是全屏交互监控程序（需要 TTY），已拒绝。一次性快照可用 \`top -b -n 1\``，命令未执行、回到 bash 模式提示。

### D21c. `!` 命令输出预览（BashModeProgress）

- **症状**：`!pwd`/`!ls` 执行后输出被折叠组吞掉——折叠摘要行（"Ran 1 bash command"）不含输出，且 inline 模式落盘后无法展开，用户看不到命令结果。
- **机制**：bash 模式进度 = `<bash-input>` 行 + 与普通工具结果同款渲染器的 fullOutput——输出直接展示在命令下方（长输出折叠 "+N lines"）。
- **实现**：`UiHooks.on_tool_ready` 与 `UiEvent::ToolReady` 增 `standalone: bool`——`!` 命令的 Bash 活动标记 standalone：只设摘要、**不参与折叠组**（模型驱动路径传 false，折叠行为不变）；ToolDone 时非折叠 Bash 活动**默认展开**，内容 = 输出去掉 `$ cmd` 回显与 `[Exited with code N]` 尾注（`bash_output_preview`），复用 layout_activity 既有的 `⎿` 连接行渲染。
- **验证**：189 测试全过（新增 standalone 折叠判定正反例、预览展开与剥离断言）；PTY 冒烟：`!pwd` → `⏺ ✓ Bash $ pwd · 8ms` + `⎿ /Users/yexrob/Episodes/Projects/bingo`；`!ls` → `⎿ AGENTS.md` + 缩进续行，多行输出完整可见。

### D22. AskUserQuestion 工具（问用户选择题）

- **契约**：`questions[1..=4]`，每题 `{question, header?（≤12 字符）, options[2..=4] {label, description?}, multiSelect?}`；问题文本与 option label 各自唯一。v1 实现：`multiSelect: true` 报错引导改单选；`preview` 字段不纳入 schema（UI 无预览面板）；header 缺失时以「问题 N」为题。
- **执行**：`src/tool/ask.rs`——`is_concurrency_safe=false`（阻塞等待回答，串行）；逐题调用 `ToolContext.ask_question`（None = Esc 跳过，后续问题不再问）；结果 `The user answered: "q"="a", ...` 或 `The user did not answer the questions.`；模型端输入校验失败（数量/唯一性）以 `tool_use_error` 回填。
- **UI 复用**：AskUserQuestion 走既有权限模态（`PermissionRequest` + 1-9 键 + Esc）——`UiHooks.ask_question` 与 `ask` 同通道，`Confirm(i) → Some(i)`、`Cancel → None`。**不经权限门**（对话框本身即审批；run_query 门内按名短路）。工具行摘要显示问题文本。子代理（Agent 工具）的 ask_question 恒 None（无 UI 可问）。
- **连带修复**：`schema_for` 此前丢弃 schemars generator 的 definitions——嵌套类型（`Vec<AskQuestion>` 经 `$ref` 引用）发给模型的 schema 悬空。现把 `generator.definitions()` 合并进根 schema（对扁平工具无影响）。
- **取舍**：多选/Other 自由文本/预览面板为后续项（多选组件与文本输入需扩展模态协议）；超时不做（默认 never，无限等待）。
- **验证**：196 测试全过（新增：schema 形状含 definitions、输入校验反例、回答/跳过/多选拒绝、执行队列串行化、TUI 挂钩 Confirm/Cancel 映射）。

### D24. MCP 管理：McpManager 连接缓存 + /mcp 命令

- **机制**：启动并行连接全部 server（batch，失败不阻塞）；连接状态 connected/failed/needs-auth/pending/disabled 进 AppState；SSE 断线指数退避重连（5 次，1s→30s）；`mcp__{server}__{tool}` 前缀进 ToolRegistry，ToolListChangedNotification 动态刷新；enable/disable 持久化 `disabledMcpServers`/`enabledMcpServers` 名单（project config）；`/mcp` immediate 命令 = 交互 UI（状态徽标 + fork/重连/日志/删除菜单）+ `/mcp enable|disable [name|all]` + `/mcp reconnect <name>` 快速路径。
- **bingo 现状改造**：原 `connect_servers` 每次回合 spawn 子进程重连（浪费）。新增 **`McpManager`**（挂 `Session.runtime.mcp`）：**懒连接**（首个回合 `connect_all`，之后复用缓存）、失败记录不自动重试（`/mcp reconnect` 手动——stdio 子进程退出即彻底失败，自动重连无意义）、`disconnect`/`set_enabled` 立即生效。
- **/mcp 命令**（argumentHint `[enable|disable [server-name]]`）：无参数列出（✓ connected · N tools / ✗ failed: 详情 / ○ disabled / · not connected）；`enable|disable [name|all]` 更新名单并**持久化 `.bingo/settings.json` 顶层 `disabledMcpServers`**（同名机制）；`reconnect <name>`（disabled 时拦截提示先启用）。
- **配置契约**：`McpServerConfig` 增 `type` 字段（TransportSchema）；**stdio**（`command`/`args`/`env`）与 **http**（`url` + 可选 `headers`，streamable HTTP）落地；sse/ws 连接时报错提示（rmcp 3.1 无 legacy SSE；OAuth 未做，静态头先覆盖）。`command` 改可选（http 无命令）。
- **权限**：MCP 工具复用统一权限门（Box<dyn Tool> 已有）；is_concurrency_safe=false（串行，保守策略）。
- **验证**：244 测试全过（新增 McpManager 状态矩阵/失败不重试/reconnect 清失败 + /mcp 列表/enable-disable 持久化/reconnect 拦截）；tmux 实测（无依赖 Node stdio server）：懒连接 2 tools、badsrv failed + 警告行、disable 断开+持久化跨会话、disabled reconnect 拦截、enable 后下回合自动连接。

### D25. 运行状态行（ActivityIndicator）

- **机制全景**（让用户知道 agent 在运行）：
  1. **状态行（ActivityIndicator）**：transcript 底部、输入框上方一行——spinner（100ms 帧）+ 动词消息（`{verb}…`）+ thinking 计时（`(thinking for 12s)`）+ 工具计时（`running tool for 3.2s`）+ 输出 token 计数（`↓ N tokens`）+ 总耗时。动词 = 运行中工具的 activeForm/subject > thinking 俏皮词 > 兜底 "Working"。**无论模型在想、在等、在跑工具，这行永远存在**。
  2. **thinking 占位**：`⠋ Mulling for 1.4s`（~150 俏皮词表）。
  3. **工具行**：输出预览——无输出时 `Running…` + elapsed，有输出时尾 5 行 + `~N lines`/`+N lines` + `(timeout 2m)`。
  4. **工具心跳**：30s 无输出心跳 progress，长任务 elapsed 持续刷新。
  5. **stall 检测**：距上次 token 10s/45s/300s 阈值，spinner 降强度/变 warning 色；429 显示 `Waiting for API response · will retry in X · check your network`。
  6. **spinner tips**：运行 >30s 提示 `/btw`、>30min 提示 `/clear`；有任务时 `Next: {subject}`。
- **bingo 实现**：`Chat::running_status()`（busy 时返回 `(动词, 耗时)`——运行中工具 summary > thinking 俏皮词 > "Working"；`turn_started` Instant 由 TurnStart/TurnEnd 设置）+ `status_row` 渲染在输入框上方（chrome 一行，inline/全屏均可见；任务区与警告行之间）。动词优先工具 summary（`$ sleep 2`），与 activeForm 语义一致。
- **验证**：198 测试全过（新增 `running_status` 动词优先级、状态行渲染断言）；PTY 实测 `!sleep 2`：`⠼ $ sleep 2 for 0.2s → … → 2.0s` 逐帧跳动，回合结束消失。
- **遗留（上游 iocraft 问题，非本改动引入）**：API 完全挂起（无任何事件）时，tick 驱动的渲染链会在 ~1s 内饿死——spinner/计时冻结在提交瞬间（基线同现；探针时序可复现/可绕过）。事件流正常时（含真实 API 的流式往返）无此问题。状态行至少在冻结前给出"Working"可见提示；彻底修复需在 iocraft 渲染循环的唤醒链上动手（`select(root.wait(), term.wait())` 对自驱动动画的唤醒竞态）。（已随 D26 重写消亡）

### D26. TUI 渲染层重写：iocraft → ratatui 0.30 + 自研 inline 驱动

- **动因**：iocraft inline 模式每帧重画整个 canvas（含永不变化的内容）、每行空格补齐到满终端宽、光标相对 diff 在终端 resize reflow 下失同步、canvas ≥ 终端高触发 `Clear(All)+Purge` 清空 scrollback。多轮补偿（chrome 记账、shrink_deficit、reflow 白名单）治标不治本——resize 楼梯残骸的根源是「重画已定稿内容」这个前提本身。
- **新架构**（codex-rs / Claude Code 同款）：定稿行经滚动区域**一次性写入终端 scrollback，永不重绘**；只有底部视口（未定稿尾部 + chrome）被重画。resize 时旧内容由终端自然 rewrap（与普通 shell 输出一致），残骸从结构上不可能累积。
- **分层**：`src/ui.rs` 渲染无关契约（UiEvent/AskRequest/PermissionRequest/DialogAction/tui_hooks，零渲染依赖——未来 GUI 对同一契约实现另一 `run_*_session`）；`tui/term.rs` 全 crate 唯一碰终端的模块（视口双缓冲 diff + insert_history + CSI ?2026 同步更新包裹，镜像 ratatui `insert_before` scrolling-regions 路径与 codex `custom_terminal`）；`tui/app.rs` 显式 `select!` 事件循环 + Frame 组装（帧高 = 实测行数，无第二套 chrome 公式可漂移）；`tui/view.rs` 纯转换（crate `Line` → ratatui text）。
- **迁移面**：chat.rs 4,100 行逻辑 + 3,300 行测试仅换 import 全量存活（iocraft 的 KeyCode/KeyModifiers 本就是 crossterm 再导出）；line/theme 换 Color 类型；components.rs（2,122 行）整体删除；iocraft 依赖移除，新增仅 ratatui（`scrolling-regions` feature 需显式开启，0.30.2 非默认；crossterm 加 `event-stream`）。
- **驱动语义要点**：视口未贴底先 `scroll_region_down` 下推（不耗 scrollback），贴底后对上方区域 `scroll_region_up` 分块入 scrollback；行尾 `Clear(UntilNewLine)` 不填空格（resize 无折行垃圾的关键）；判空用 `Cell::EMPTY` 全等——带背景色的空格是内容（用户气泡尾巴靠此保活）；`HistoryItem::Raw`（kitty 图片字节）按 rows 记账、其行永不清除；视口增高在物理底行写真实换行（唯一全终端保 scrollback 的滚动）。
- **顺带改进**：bracketed paste 真事件（突发启发式降为兜底）；删除线真实渲染（CROSSED_OUT）；终端硬光标落在 `▋` 位（D20g 的意图，iocraft 无光标 API 的约束消失）；极矮终端从顶部丢行保住输入框 + footer（旧行为触发 Purge）；D25 遗留的 tick 饿死随渲染循环消亡（tokio interval 无唤醒竞态）。
- **验证**：468 测试全绿（chat.rs 115 个既有测试零改动通过；驱动 23 个 TestBackend 场景测试，含「50 行历史 + 缩窄 resize 后无任何重复行」回归；app/view 移植 25 个策略测试——tail window、chrome 完整性、flush 跨宽度不双打、ctrl+o 门控、建议行数同源）；`cargo clippy -- -D warnings` 干净。真机烟囱测试（Ghostty 长回复 + 拖拽 resize、tmux+Ghostty 图片、Terminal.app、BCE 终端的 EL 清行）待实测。
- **首轮真机回修**（2026-08-06 Ghostty 实测反馈四项）：
  1. 回合后大片空白——settle 顺序错了：先把定稿行滚进 scrollback（视口还高），再收缩视口，收缩腾出的行成了永久空白带（高度=刚才回复行数）。改为驱动 `gap_above` 银行 + `frame()` 单批次：先收缩（腾出的行记账入银行）、定稿行直接写进这些行、再 diff 视口——settle 零空白且常态零滚动（测试断言 `scrolled_up` 为空）；银行未耗尽的部分由下一次增高回收（grow 先收银行再滚动）。
  2. MCP stderr 写穿 TUI——scrollback 永不重绘后，stdio 子进程日志一旦落屏就固定（旧架构被每帧全量重画掩盖）。spawn 时 stderr 重定向 `~/.local/share/bingo/logs/mcp-<名>.log`（每次连接截断重写；开不了文件则丢弃，绝不继承终端）。
  3. tmux 图片从未显示过的真因——raw mode 下 `placeholder_rows` 每行以裸 `\n` 结尾：LF 只下移不回车，占位符第二行起从上一行末列开始，网格整体斜切，kitty 按占位单元格放像素自然全乱（iocraft 时代同病，故「一直没显示过」）。`kitty_image_bytes` 尾部推进同改 `\r\n`。
  4. 拖拽 resize 边框残影——chrome 的满宽行（边框/气泡）被终端 reflow 折行、逃出视口清理区。加 120ms 防抖：风暴中只记录最新尺寸、不作画（旧宽作画只会叠更多错宽行），静默后一次应用+重画——残留从每步一组降到每次拖拽至多一组（Ctrl+L 可清）。彻底解法见 D27。
  验证：473 测试全绿（驱动 +4：settle 零滚动、银行分块滚动、grow 回收、resize/clear 重置银行），clippy 干净。

### D27. 懒落盘 + resize 回灌：大活视口

- **需求**（用户明确提出）：视口范围内的内容不要冻结进终端——窗口内一律保持可重排；容量变大（拉高窗口、缩小字体）时，连**已经落盘**的内容也要取回重新渲染填满屏幕，「我可以接受这种情况上滑的时候有重复」。
- **策略**：
  1. **懒落盘**：定稿段只要仍完整落在可见窗口（终端高 − 1 − chrome）内就留在活文档里，每帧参与 diff 重画（宽度变化随 `build_rows` 重排、ctrl+o 折叠照常可用）。段的起始行越过窗口顶端才冻结，且**整段冻结**——半冻结会让隐藏部分既不在屏上也不在 scrollback，无处翻看；整段冻结时其可见尾部从视口绘制转为 scrollback，逐像素不动（gap 银行吸收，见 D26 回修 1）。
  2. **回灌（rehydrate）**：resize 防抖静默后，若窗口容量大于活文档行数，按检查点从最近落盘的段往回取（先问答块行、再消息段），`build_rows` 试渲染，超出预算即回退——保证回灌完不存在越过窗口顶的定稿段，与懒落盘互不打架。回灌是**纯记账**（flushed 游标回退，不写终端）；scrollback 里的旧拷贝物理上收不回，上滑会看到一份旧几何的重复——用户明确认可的取舍。
- **机制**：`Doc.settled_marks: Vec<SettledMark{row_end, segments, ask_rows}>`——build_rows 在每个定稿点（欢迎卡/每条定稿消息/问答块）记检查点（构建内累计值，`Chat::mark_base` 消化跨次推进的增量）；app 层 `pick_flush_mark` 选「所属段起始行 < 窗口顶」的最远检查点，`advance_flushed_upto` 部分推进。旧聚合字段 `settled_segments/settled_ask_rows` 删除（检查点的纯重复）。
- **副作用剖析**：resize 后首帧视口大幅增高会把屏上旧几何残留推进 scrollback（append_lines），新渲染的拷贝随后填满屏幕——正是「接受重复」的语义；图片块回灌后再冻结会重发 kitty 传输，U=1 同 id 更新既有放置、Direct 无 id 累积新实例但旧放置像素不动，均无害。
- **验证**：476 测试全绿（新增：pick_flush_mark 策略矩阵、装得下不冻结、回灌填满/超预算回退；9 处既有断言改读检查点）；clippy -D warnings 干净。
- **真机回修二（/resume 大列表触发逐行重复 + 拼接行）**：三个叠加成因，一并根治——
  1. **DECSTBM 单行区域非法**（元凶）：懒落盘后视口高达 H−1、`vp.top==1` 成常态，make_room 逐行发 `CSI 1;1r`+`CSI S`——DECSTBM 要求区域 ≥2 行，非法参数被终端忽略、区域退回**全屏**，每写一行全屏滚一次，视口内容与写入交错上移 → 逐行相邻重复 + 新行盖旧行的拼接尾巴（TestBackend 对单行区域语义正确，故 27 个驱动测试全绿而真机全错）。修复：视口上限统一改 **H−2**（driver clamp / Frame 预算 / tail_window 三处同源），`vp.top ≥ 2` 恒成立、区域恒合法。
  2. **`CSI S` 不进 scrollback**（kitty/Ghostty 明确语义：SU 滚出的行进 bit bucket）：落盘滚动换成 codex 同款原语——`RawWrite::scroll_into_scrollback`：DECSTBM 顶锚区域 + 光标停区域底行 + n 个 LF + 复位（LF 是唯一全终端进 scrollback 的滚动）；测试端 Recorder 映射回 `scroll_region_up` 保持 TestBackend 语义等价，27 个驱动测试零改动。
  3. **瞬态 slash 输出驱逐活内容**（策略错误）：/resume 无参列表是文档尾部的 TTL 瞬态行，把窗口挤小导致活对话被误冻结（且 2s 后窗口空半截）。`Doc.transient_rows` 标记瞬态行数，懒落盘的窗口计算剔除之——临时列表只是暂时盖住，不是驱逐。
  验证：477 测试全绿（新增瞬态不冻结回归测试；3 个 H−1 期望值随上限改 H−2），clippy 干净。
- **真机回修三（resize 后视口跳底、旧画面成多宽度残骸堆叠）**：`term.resize()` 原本无条件把视口重锚到屏幕底部——内容未占满屏时（如仅欢迎卡在屏幕上部），每次宽度拖拽都把视口甩到底部重画，旧位置的画面留在上方永不清除，一次拖拽堆出一摞不同宽度的卡片。改为**内容锚定**（CC 同款行为，用户点名）：resize 保持视口原行、仅在新屏装不下时上移到恰好容纳；清除从视口原位到屏末，把终端 reflow 从旧视口折出的碎片一并抹掉。视口从内容起点开始渲染，与刚启动的会话一致。477 测试全绿（`resize_taller_keeps_the_viewport_content_anchored` 取代旧的跳底断言）。
- **真机回修四（缩宽后视口原点之上仍残留一行折行碎片）**：宽度缩小时终端 reflow 先于 resize 事件发生，旧满宽行折行使内容整体下移、顶到屏底还会整屏上滚——我们记录的视口原点与物理现实的位移**协议上不可知**，「从原点向下清」必然漏掉被推到原点之上的碎片。终极策略=不猜几何：resize 静默后走 Ctrl+L 通道（`force_redraw` → `clear_visible`）清掉整个可见屏、按新宽从头重画窗口；回灌已把内容拉回填满屏幕，画面无损，旧几何拷贝只留在 scrollback（用户接受的取舍）。自此 resize 的屏上表现完全确定性，不依赖任何终端 reflow 行为。
- **真机回修五（Ctrl+C 后整屏错位重影 + MCP stderr 仍写穿）**：两个独立成因——
  1. **reclaim 增长清空 diff buffer**（重影元凶）：回合结束 status 行消失 → 视口收缩 1 行（底锚，origin 下移、顶行清空进银行）；Ctrl+C notice 行出现 → 增长走 reclaim 路径，该路径把两个 buffer 清空「强制全量重画」——但物理屏只有银行那几行是空白，其余仍是旧帧。prev buffer 谎报全空后，diff 认定屏幕已空：新帧的**空行**不清底下的旧字（`❯ Hi`/回复相邻重复、卡片底边框碎片存活）、**变短的行**不清行尾（notice 文本右侧残留输入框边框 `────`）。修复：reclaim 不清 buffer，`retarget_buffers` 加带符号 offset（收缩 +shift 底锚、下扩/滚动增长 0 顶锚、reclaim 增长 −reclaimed 底锚下移、顶部补空行——恰好镜像物理上真空白的银行行），**prev buffer 任何时刻如实镜像物理屏**成为驱动不变量（resize/clear_visible 清 buffer 是因为它们同时物理清屏，不破此律）。回归测试 `grow_after_shrink_repaints_over_every_stale_row` 在旧实现下精确复现截图 artifact（`["eeee","bbbb","ffff","ggdd"]`）。触发面不止 Ctrl+C：任何「收缩→增长」序列（每个回合结束后再发消息/开 help/出建议）都踩中。
  2. **rmcp builder 覆盖 stderr**（上次回修一的重定向从未生效）：rmcp 3.1.0 `TokioChildProcess::new` 内部走 builder，builder 默认 `stderr: Stdio::inherit()` 且 `spawn()` 无条件 `.stderr(self.stderr)` **覆盖** Command 上已设置的值——我们在 Command 上设的日志文件 sink 被静默丢弃。修复：显式走 `TokioChildProcess::builder(command).stderr(stderr_sink(name)).spawn()`。触发时机与「发消息后稳定出现」吻合：MCP 懒连接在首条消息的 assemble_tools 时 spawn，banner 恰好打在光标（输入框）处。
  验证：478 测试全绿，clippy 干净。
- **inline ctrl+o = 展开/闭合切换，展开即整卷 transcript 重放**（CC 非全屏 ctrl+o，用户点名）：scrollback write-once 决定了 inline 下不存在「原地展开」——旧方案只对未落盘的最后一条消息放行折叠切换（`last_message_dynamic` 门），已落盘内容按不动。改为 CC 语义，两个方向：
  - **展开**（存在折叠项或已落盘内容）：`Chat::expand_transcript` 把**全历史**所有可折叠项（活动 + 折叠组）展开、`reset_flushed` 回卷落盘游标、置 `dump_transcript` + `force_redraw`；app 层重放帧**先清可见屏**（与 resize 同款——不清屏的话旧画面与重放行的相对位置取决于视口历史，短内容会同屏重复），再从欢迎卡全量重建文档，取**最后一个**定稿检查点一次 `flush_items` + `advance_flushed_upto` 整卷冻结进 scrollback：重放内容从屏幕顶部铺起、chrome 紧随其下，超屏部分自然滚入 scrollback，用户上滑翻看全貌。动态尾部（流式消息/权限对话/瞬态 slash 行，均在检查点之外）照常留在视口。屏上已是全貌且无可展开项时 no-op。
  - **闭合**（`transcript_fully_expanded`：存在可折叠项且全部展开）：`Chat::collapse_transcript` 全历史折回聚合态，随后走 **resize 同款收拢**——撤销未渲染的重放、`force_redraw` 清可见屏、`rehydrate` 按折叠后的高度回灌填满窗口。屏上的展开重放行只留在 scrollback（write-once，不清屏就会与折叠窗口同屏并存）。无可折叠项时判定恒为假，ctrl+o 退化为纯重放。
  折叠旧拷贝留在 scrollback 上方，接受重复（与回灌同一取舍）。零新驱动原语：回卷（/clear、/resume 同款）+ 全量冻结（懒落盘同款）+ 清屏回灌（resize 同款）的组合。`last_message_dynamic` 门随之删除；fullscreen 的 ctrl+o 仍是就地折叠切换（那里可以重绘）。478 测试全绿（重写 2 个门测试为重放/闭合语义）。

### D28. 活动行图标词汇表：形状表类别，颜色表状态

- **问题**（用户点名「太丑」）：所有活动行统一 `⏺`，MCP 工具裸露全名 `mcp__server__tool(...)`，Skill 显示 k=v 兜底 `args="doc.md"`——类别不可辨、噪声重。
- **词汇表**：`⏺` 内建工具（CC 锚点不动，组行/回复点/Update 同族）· `◆` MCP（外接件，显示名 `server:tool`，权限规则仍用 `mcp__` 全名）· `✦` Skill（与 ✢✻✽ 星芒 spinner 同族，摘要改 `技能名 参数`）· `◉` 子代理 Watch 行（环中有核=会话套会话；Agent 是隐藏工具，唯一可见行是 watch）。颜色继续只表状态（dim 运行/绿成/红败），一职一色。
- **实现**：`activities.rs tool_glyph`/`display_tool_name` + `watch_header` label 前缀判定；`summarize_input` 加 Skill 臂。全部显示层，零行为变化。四个字形均为 unicode_width 单宽（◆/◉ EA=Ambiguous，与既有 ○/◇ 同类已被接受）。480 测试全绿。（label 前缀判定随 D29 换成 WatchKind 契约字段。）

### D29. 具名 agent + 实例续话（第一刀：hub-and-spoke）

三轮设计讨论（用户逐轮删概念）收敛出的多 agent 路线图：第一步 = 本条（非实验）；第二步 = 频道互发（实验开关，见下方设计冻结）。

- **具名定义**（`src/agents.rs`）：`~/.config/bingo/agents/*.md` + `.bingo/agents/*.md` 平铺文件（近 cwd 项目层同名覆盖用户层），frontmatter `name/description/model/provider/thinking`、正文 = 子代理 system prompt（替换父 system，空则继承）；优先级 显式参数 > 定义 > 继承（模型/provider/思考各自独立）。**跨 provider 边界（#19 修复）**：fork 到与父会话当前 provider 不同的端点时，父模型与思考级别都不继承——model 必须显式（缺省早失败「provider X 需要 model」，避免父模型名发到错误端点 "model not found"），thinking 缺省 off（不带参数，兼容 DeepSeek/Ollama 端点）；provider `"default"`/未指定 = 共享父端点（跟随父切换），同 provider（含未指定）thinking 继承父会话当前级别快照。thinking 显式/定义值校验（off + THINKING_LEVELS，非法报错而非静默降级）。frontmatter 解析泛化为 `skills::parse_frontmatter_pairs`（任意键 + 折叠/字面标量），技能与 agent 定义共用。定义个位数，不做 mtime 缓存。
- **实例注册表**（`AgentRegistry`，Session 级共享）：每次 Agent 派生登记一个具名实例（`name` 参数缺省取定义名/agent，重名自动 -2/-3），状态机 Running/Idle/Stopped。回合完成把 **run_query 返回的完整消息历史**存进条目——续话 = 旧历史 + 新指令再进 run_query，上下文零丢失。
- **续话与生命周期**（仅 depth==0 装配，hub-and-spoke）：`SendMessage(agent, message)` 忙碌排队（回合结束由同一后台任务链自动续跑下一回合）、空闲带历史唤醒（新 spawn）；`AgentControl(list|stop|delete)` 列表/停止（abort 当前回合 + watch 行置 Cancelled，历史保留）/删除（移除条目，名字释放）。多条排队指令按序并成一个提示。子代理仍可继续派生（深度上限 3）但不管理兄弟。
- **展示**：`WatchKind` 贯穿 Watchable → WatchEvent → UiEvent → WatchCall（契约字段，替代 D28 的 label 前缀判定），子代理 watch 行 `◉ 名字 · 任务`，续跑回合 `◉ 名字 #N · 指令摘要`（每回合独立行，label 唯一避免 TUI 按 label 撞行）。
- **遗留**：同步（background:false）子代理若整回合被用户中断，条目可能停留 Running（无驱动方）——AgentControl stop/delete 可清理；与旧版 watch 行孤儿同类，未新增失败面。
- **第二步（频道互发，实验开关 `experimental.agentChannels`）已落地**：设计原则——能力普遍（人人可听可说）、选择自主（沉默是 agent 醒后的决定，不调用 Post 即沉默、零成本不传播唤醒）；引擎零游戏/场景知识，点名纪律等全在提示词。实现：
  1. **`src/channels.rs`（纯状态）**：频道 = 成员名单 + serial|free + 单调 seq + 全量 log + 每成员 seen 游标 + 发言计数；`post()` 只做三件事——盖戳（from 由调用方从 `Session.instance` 取，模型无法指定）、serial 陈旧校验（seen < seq → `Stale{missed}` 弹回并把增量计入已读）、预算闸（每 agent 每频道 50 / 每频道 500，超限冻结 + hub_mail 一次警示）。hub 成员名恒 `main`（claim_name 保留）。
  2. **投递唤醒（工具层编排）**：`AgentRegistry` 信箱泛化为 `InboxItem::Direct|Channel`，单锁原子"投递+认领"无丢失唤醒；Post 的 Sent 分支对每个成员 `deposit`——Idle 立即 `spawn_agent_loop`（`absorb_inbox` 格式化 `[#频道 第N条] 发件人: 内容` 并推进 seen 游标）、Running 信箱累积回合边界批量注入、Stopped 静默丢弃。hub 走 `hub_mail`：query loop 每轮推理前 `<channel-messages>` 注入；TUI 空闲时由 ◇ 行的 WatchEvent 触发 submit_auto（TurnEnd 检查兜底）。
  3. **serial 弹回是工具结果不是错误**：`Post` 返回"未送出+增量+请重新决定"，模型在同回合 tool loop 内阅读增量后自判照发/改发/放弃——报数式顺序从竞争+重试中涌现（每轮竞争必有一人落地，收敛有保证）。
  4. **工具面**：hub（depth 0 且开关开）得 `Channel`（create/invite/kick/list，成员限 depth==1 直接子代理，迟入无 backlog、seen 置当前头）+ `Post`；cohort 成员（depth 1 有实例名）只得 `Post`；更深层与关着开关时不装配。AgentControl delete 顺带清出全部频道。
  5. **观战**：每频道一条 `◇ #名字` watch 行（WatchKind::Channel），post 时更新 detail（N 条 · 最近发言）与 payload（日志尾 50 条，ctrl+o 展开看全群聊）；Running 态更新不产生通知垃圾。
  6. **v1 无引擎等待**：round_robin/gather 均已在讨论中删除（顺序=调度/协议，非传输），故无需超时机制；预算是唯一止损。settings 逐字段合并（开关任一层开启即开）。

### D30. 实体视图：底部选择器 + 交替屏模态（agent 对话 / 微信式频道房间）

- **需求**（用户点名，参照 CC 多 agent 展现）：agent 在底部展示，↑↓ 选择、回车进入该 agent 的对话；频道也在列表里，打开时强制全屏、微信式布局（他人靠左、我发的靠右）。
- **选择器**（`chat.rs`）：底部实体区（chrome，输入框上方）——收起态一行 dim 摘要（`◉ scout(running) · ◇ #table(3) — ctrl+g 查看`），ctrl+g 聚焦后逐行 `❯` 选择（窗口滑动、上限 6 行）、Enter 置 `open_entity`、Esc 收起；快照经 `refresh_entities` 在 tick（每 15 tick）与 WatchEvent 时刷新，内容变化才 dirty。选择器按键先于全局 Esc 语义。
- **模态**（`src/tui/entity.rs`）：write-once scrollback 决定 inline 没有"就地换内容"——两个视图都跑**交替屏**（fullscreen 宿主本就在交替屏则不嵌套进出）。自绘循环（ratatui Terminal 每帧全画）、贴底滚动 + ↑↓ 偏移、Esc/ctrl+c 返回；模态期间照常 `chat.tick + drain_all`（后台 agent 事件不丢，hub 的自动回合照常拉起）。**返回走确定性重画**：inline 置 pending_resize（清屏 + 回灌 + force_redraw 的既有 resize 管道，不赌交替屏恢复的保真度），fullscreen 置 force_redraw。
- **agent 对话视图**：注册表历史（❯ 指令 subtle / 正文纯文本折行 / `⏺ 工具(摘要)` dim；tool_result 与 thinking 不进视图）+ **流式活尾**——`AgentRegistry.live` 持每回合与 subagent_hooks 共享的输出 Arc（回合始挂终摘），运行中打开能看到正在生成的文本 + `✻ 生成中…`。
- **频道房间**：`user` 成为第三个保留成员（与 main 同样自动入席、不可移出、预算豁免、claim_name 保留）；气泡布局——他人靠左（名签 + code_block_bg，连续同发件人合并名签），user 靠右（user_message_bg，右对齐 pad；SegStyle 分段 bg，不用整行 Row.bg）；底部输入行 Enter 经 `deliver_post`（Post 工具同一投递/唤醒路径）以 user 身份发言；**渲染即已读**（每帧 mark_seen 到日志尾，serial 校验对着屏幕的人恒为最新，不弹回）。
- 506 测试全绿（气泡布局/agent 视图/选择器状态机纯函数测试；模态循环与宿主接线为薄覆盖，与 fullscreen 同层）。

### D31. agent team：项目级编队 + 跨会话记忆（键 = 项目路径 + 分支）

需求（用户点名）：① team 固定到项目（可入库），项目启动默认读取并拉起；② team 保留记忆，记忆与「当前项目路径 + 当前分支」绑定。`#dev-room` 全员两轮头脑风暴收敛（dev-ex/ui-ux/dev/qa，28 条）。

- **心智模型**：team 是图纸（持久定义），room 是工地（易失运行态）。team = 声明式编队层，复用一切既有原语，跑完回到 hub-and-spoke 控制面。
- **配置** `.bingo/team.json`（camelCase、进版本库、与 settings.json 同层）：`name` + `channel{ mode: serial|free, messageLimit: 正整数 }` + `members[{ name, agent }]`。成员引用 AgentDef 而非内联人格——人格单一事实来源仍在 `.bingo/agents/<名>.md`，一人格可入多 team。职责分离：settings 管「要不要拉起」（`team.autoStart`），team 文件管「拉起什么」。
- **启动默认加载**：`autoStart` 缺省 true（用户需求字面「默认读取」），双 opt-out：settings 关 + `--no-team` CLI。**拉起 ≠ 唤醒**：只派生成员 + 建房间，走现有 Idle 待命态（零 token、零回合），等 SendMessage/频道消息才开跑；验收断言「未收任务前 token=0、无回合日志」。拉起完成一行提示 `[team] dev-room 就绪 · 3/3 待命（/team status · /team stop）`，异常升级警示。
- **实现为三块薄层，不引入新运行时**：team.json 解析（校验函数 validate 与 start 同源：validate 能过 start 必成）→ `spawn_team` 编排（现有 Agent spawn + `ChannelRegistry` create-if-not-exists + 成员去重）→ `/team` 命令族。幂等键 = 实例名：重复 start 复用（事件措辞 `spawned ×3` vs `reused ×3`，同 `[team]` 前缀不同动词）。
- **命令一条线**：`/team list`（定义区/运行区两屏）→ `start` → `status`（待命/忙碌/异常/离线四态，字符+颜色双编码 ●绿 ◐黄 ✗红 ○灰）→ `assign`（= SendMessage）→ `stop`；`/team new` 交互脚手架（产物必过 validate，选成员时对引用不到的 AgentDef 即时拦截）→ `/team validate`。
- **AgentDef 加 `source: AgentDefSource`**（Project/User/Unknown）：`load_agent_defs` 加载时记第一出处（项目层先加载、first-wins 去重 → 跨层同名覆盖 source=项目层）；无 source 的旧数据缺省 `Unknown` 不报错，UI 静默省略徽标。AgentDef 当前无序列化路径，加字段零破坏。
- **边界（qa 定稿）**：缺失 AgentDef 报「缺失名 + 查找路径 + 字段路径」三段式；配置内成员重名=拒绝；空成员=配置错误、单成员合法；成员级失败隔离（继续拉起其余，失败者标异常可单独 re-spawn）；半启动态保留可辨认（`部分拉起 · 2/3`），不自动回滚；三层 settings 合并加 autoStart 的覆盖顺序测试；未知字段忽略（旧版本兼容）。
- **记忆持久化**（第二轮收敛）：
  - **键 = 项目路径（project_hash）+ 分支**：worktree 场景天然成立（主仓库 main 与 `.bingo/worktrees/agent-team` 路径+分支不同，记忆互不污染）；避免嵌入绝对路径，project_hash 校验断言防拷贝到异机误挂。
  - **内容分层**：完整历史落盘（恢复保真）+ 决策记录（append-only、零模型成本，`sources` 管道分隔字段复用 `parse_frontmatter_pairs`，`type` 下沉条目级）。
  - **存储**：user 层按项目+分支分目录（默认不进版本库）；协作导出 `export` 零转换（frontmatter 即 schema 字段）；`project_hash` 含于导出文件头。
  - **碎片清理**：`/team memory list|show|gc|merge|export` 命令族，gc 带 TTL；损坏/孤儿文件与配置错误同一视觉语言（不另造样式）。
  - **恢复时机**：启动拉起时自动恢复，一行摘要不打扰；恢复与拉起同走 spawn_team，缺文件静默回落空历史。
- **验收断言链**：脚手架产物 → validate 通过 → start 不因配置失败；记忆 roundtrip（存→重启→恢复等值）；source 跨层覆盖；无 team 段旧项目行为完全不变。
