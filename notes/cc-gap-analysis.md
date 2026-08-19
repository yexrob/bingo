# bingo × Claude Code CLI — 系统设计与实现差距调研报告

> 日期：2026-08-13 · 团队：dev-team（dev / devex / ui-ux / qa / deploy 五路并行调研）
> 对照材料：`/Users/yexrob/Episodes/Resources/research/claude-code-re`（Claude Code 2.1.88 泄露 TS 源码 + 2.1.221 二进制逆向）
> 方法：只读分析，未修改任何 bingo 源码、未跑构建测试。成员报告原始文件见 `/tmp/{dev,devex,uiux}-report.md`。

---

## 0. 一句话总判断

**bingo 与 Claude Code 的核心 harness 架构同构度极高**（流式主循环、统一权限闸门、safe 并行/unsafe 屏障、tool_result 回灌、主动压缩、memdir 记忆、双路径 TUI 都有）；差距**不在架构骨架，而在三层**：① 若干窄而高价值的正确性/持久化缺口（memory 生命周期、中断策略、compact 边界持久化）；② 权限/Hooks 的**契约深度**（typed decision、事件覆盖、来源溯源）；③ 产品面（通知、工具语义状态、分发渠道）。另有几处 bingo 已反超 CC。

---

## 1. 总体架构同构度

| 层 | bingo 现状 | CC 做法 | 同构度 |
|---|---|---|---|
| 流式主循环 | query.rs / query_session / query_turn：流式 loop、tool_use 驱动、`BlockStop` 收集 | queryLoop + StreamingToolExecutor，以实际 tool_use 块为准（非 stop_reason） | ✅ 同构 |
| Tool 契约 | `src/tool/mod.rs:93-130`：name/description/input_schema/is_concurrency_safe/is_read_only/is_destructive/call | Tool.ts：同字段 + interruptBehavior/maxResultSizeChars/renderToolUse*/aliases | ✅ 骨架同构，缺 4 个字段 |
| 并发队列 | `src/tool/executor.rs:6` MAX_CONCURRENCY=10；连续 safe 并行（FuturesUnordered）+ unsafe 串行屏障 | partitionToolCalls：连续 safe 一批（默认 10），非 safe 独占串行 | ✅ 算法同构，bingo 实现更简单 |
| 权限闸门 | `src/permission.rs:284-407` can_use_tool 统一闸门 + 5 模式 | canUseTool：工具 checkPermissions → hooks → mode → UI | ✅ 同构，缺 typed 溯源 |
| 压缩 | `src/compact.rs` autoCompact + overflow 恢复 + token 估算 + 熔断 | micro/auto/reactive/precompute 家族 | 🟡 有 auto+reactive，缺 micro/precompute |
| 记忆 | memdir + 项目事实提取 | CLAUDE.md 分层 + MEMORY.md 索引 + topic 文件 | 🟡 单文件模型，有正确性 bug |
| TUI | 双宿主：alternate screen 全屏 + inline main-screen scrollback | 同样双路径 + 虚拟列表 | ✅ 宿主同构，缺虚拟化 |
| 错误契约 | `[error] code= msg=` 稳定码 + 双出口 + 防漂移测试 | 无稳定码，XML 标记 + is_error flag | 🟢 bingo 反超 |

---

## 2. bingo 已反超 / 已对齐（无需追赶）

1. **错误码契约（qa）**：CC 错误是"无码"三层结构（XML 标记 + is_error + 纯文本 subtype），无稳定码、无防漂移机制；bingo 有 `[error] code= msg=` 单行 key=value + TUI/CLI 双出口统一映射 + 每 variant 防漂移单测。**已反超。**
2. **并发调度（dev）**：bingo 不是全串行——safe 工具（Read/Glob/Grep/WebFetch/WebSearch）已真实并行（上限 10），unsafe 形成有序屏障；`FuturesUnordered` 实现比 CC 的 streaming executor 更简单且无 detached task。**已对齐，无需修改。**
3. **启动链（deploy）**：bingo 全链约 75ms（含 crew 派生），比 CC 的 Bun 冷启动快一个量级；CC 的密钥/MCP 并行预取是在补偿运行时启动成本。**bingo 反超。**
4. **TUI 双路径宿主（ui-ux）**：bingo 已有 alternate-screen 全屏 + inline scrollback 双路径，inline 把"静态内容只写一次"设为正式不变量（statics.rs），并用 DECSTBM 把 settled 行可靠推入 scrollback（刻意不用 kitty 会丢弃的 CSI S）。**宿主模型已对齐。**
5. **未知工具/中断结果配对（dev）**：未知工具立即生成 is_error tool_result、中断后 fill_missing_tool_results 补孤儿——与 CC 核心语义一致。**已对齐。**

---

## 3. 按领域差距分析

### 3.1 核心架构（dev 报告）

**高 ROI：**

| 差距 | bingo 现状 | CC 做法 | 可借鉴度 |
|---|---|---|---|
| 中断无 typed reason / 无 per-tool 策略 | `watch<bool>` 布尔信号（query_turn.rs:174-240）；所有工具共享 cancel 行为 | abortController 区分 interrupt/end_conversation/reject；`interruptBehavior(): cancel\|block` 默认 block | **高**：远端写入/发送被 drop 后处于"未知是否执行"状态，是安全缺口 |
| CompactBoundary 不持久化 | 自动 compact 只改内存 messages（compact.rs:279-284）；resume 后重载完整旧历史、再次压缩、摘要漂移 | 持久化 compact boundary，resume 回放边界 | **高**：在线压缩的 session 恢复后行为不一致 |
| 压缩效果不可观测 | 只报替换消息数，无 before/after token、无 rapid-refill 检测 | preCompactTokenCount/postCompactTokenCount/turnsSincePreviousCompact | **高**：可能"API 成功但摘要+tail 仍近阈值"每轮重复付费 |
| 无 microcompact | 首次回灌截断单结果（query.rs:529-550），旧历史中的低价值 tool_result 永远保留 | microcompact 在完整摘要前清理旧 tool results | **高**：工具密集会话过早触发有损全摘要 |
| 无 turn 上限 | 无 LoopBudget | max_turns 等运行护栏 | **高**：防模型无限工具循环 |

**中/低 ROI：** 工具启动时机（bingo 等 stream 结束才执行 vs CC 边收边跑 StreamingToolExecutor——**低到中，先测量重叠收益**，会显著增加 retry/fallback 副作用复杂度）；non-streaming fallback（需求驱动，中）；precompute compaction（**低，先观测**）；ToolResultPolicy 全局截断 vs per-tool maxResultSizeChars（中，建议先加 `ToolResultPolicy::{GlobalLimit, SelfBounded}`）。

### 3.2 权限 / Hooks / Memory / Settings（devex 报告）

**高 ROI：**

| 差距 | bingo 现状 | CC 做法 | 可借鉴度 |
|---|---|---|---|
| PermissionDecision 非 typed | 只有 behavior + String reason，updatedInput 走独立 tuple；Hook ask 路径存在 `unreachable!` panic 风险（gate_tool + 两个调用方写死 Ask 分支） | `PermissionDecision { behavior, updatedInput, decisionReason{type}}`，passthrough/allow/ask/deny 不同状态 | **高**：panic 风险必须先修；typed reason 支撑溯源 |
| 权限 UI 只 allow/deny | AskFn 返回 bool；无"本次/会话/持久化"三态 | PermissionResponse 可携带 updatedInput/updatedPermissions，"always allow" 转 PermissionUpdate | **高** |
| Hooks 事件覆盖 ~37% | 实际支持 10 事件（Pre/PostToolUse、Pre/PostCompact、UserPromptSubmit、Stop、SessionStart/End、TaskCreated/Completed） | 28 事件权威表；P0 缺：PermissionRequest、PostToolUseFailure、CwdChanged | **高（分批）** |
| 无 session env 继承 | Hook 进程独立，SessionStart 无法影响后续 Bash | `CLAUDE_ENV_FILE`：Hook 写环境脚本，Bash 前 source | **高** |
| Memory 生命周期 bug | ① tail truncation 注释与实际相反（从消息头向后读 60k，保留的是前缀）；② 200 行后新事实静默丢弃；③ 无字节上限；④ worktree 记忆按 cwd hash 分裂 | 最近优先截取 + 行/字节双限 + MEMORY.md 索引 + git common root | **高**：①②是正确性 bug，③④是结构问题 |
| 三层配置无 tri-state | `Option<bool>` 缺位：后层 `false` 无法关闭前层 true；`Option<Vec>` 缺位：后层 `[]` 无法清空继承 hook | 来源一等概念，可覆盖/清空/溯源 | **高** |
| Slash 三表漂移 | COMMANDS / INSTANT_COMMANDS / dispatch match 三处独立 | 统一 Command registry（name/alias/hint/immediate…） | **高** |

**中/低：** watchPaths→FileChanged（中，先 mtime 轮询不引 notify 依赖）；团队记忆同步（中，先共享 MEMORY.md 文件，不先做分布式）；system prompt sections 缓存（中，先 Static/Session/Dynamic 三态标注，配合 /cd 失效）；Slash 诊断命令 `/hooks /memory /doctor`（高价值但归入体验层）；GrowthBook/tengu_* 远程 flags（**低，过度设计**，本地 typed registry 即可）；managed/policy 层（低，无企业需求不引入）。

### 3.3 TUI 渲染与交互（ui-ux 报告）

**高 ROI：**

| 差距 | bingo 现状 | CC 做法 | 可借鉴度 |
|---|---|---|---|
| fullscreen 无布局虚拟化 | `Chat` 持全部 messages + 完整 `Doc.rows`，每次 dirty 全量 `build_rows(width)`；`.skip(scroll).take(rows)` 只是输出层裁剪 | useVirtualScroll：只挂载 [start,end)，spacer 保逻辑高度，MAX_MOUNTED 300 条 + overscan 80 | **高**：长会话每帧扫描全部历史是结构性瓶颈 |
| 工具状态文案非语义化 | 独立工具行/全局状态行显示 `Running…`（折叠组已有 Reading/Searching 语义） | FileRead→Reading / FileWrite→Writing / FileEdit→Editing / Bash→Running | **高**：单表 `active_verb(tool_name)` 复用两处，成本低 |
| 大结果一次性物化 | ToolDone 后全量拆 `Vec<Line>`；Bash 默认自动展开 | 工具层预算 + 预览，大输出渐进展示 | **高**：需要 UI 行预算（非只字符预算）+ 分块展开 |
| 长 Bash 无 live tail | 结束前只见 `Running…`，无法区分"有输出/卡住" | 前台输出流式呈现 | **高**：有界 live tail（最后 3-8 行） |
| 无终端/桌面通知 | 无 `auto\|iterm2\|terminal_bell\|kitty\|ghostty\|disabled` 渠道 | 多渠道通知（回合完成/等权限） | **高**：长 agent turn 是核心场景 |
| 滚动位置感弱 | PgUp/PgDn 固定 10 行；无离底提示、无 jump-to-bottom | 页/半页/滚轮 + sticky bottom + 状态反馈 | **高**：viewport 页 + Ctrl+End + 离底提示 |

**中/低：** 慢 hook 500ms 可见阈值（高价值但频率低，优先级次之）；error code 默认暴露违反自身 progressive disclosure 规范（应折叠进 details）；`motion:"off"` 未覆盖全部 spinner；vim 模式（**中，Esc/modal 状态成本高，独立需求**）；fullscreen 硬件滚动 CSI S/T（**低，先 profile**，且需同步 ratatui 后缓冲）；外部 pager/vim 接管 raw terminal（**不照搬**）。

### 3.4 错误与质量（qa 报告）

- 已反超（见 §2.1）。可借鉴三点：
  1. **运行期护栏**：max_turns / max_budget / retry 上限三档（与 dev 的 turn 上限同源，合并）。
  2. **失败原因机器可读**：CC 挂前写 `result/error_during_execution/errors:[...]`；bingo 可考虑同形态的诊断出口。
  3. **U+2028/U+2029 序列化防御**（低优先级）：CC 的 `ndjsonSafeStringify` 教训（gh-28405）。已查证 bingo 用 serde_json 默认不转义；当前无 JS 消费端，仅在出现按旧 JS 行语义切 NDJSON 的外部程序时补。

### 3.5 分发与启动（deploy 报告）

- **分发是最大产品面差距**：CC 有 npm/curl/原生三渠道 + macOS 签名公证；bingo 只有 GitHub release + 自更新。建议：一键安装脚本（curl | sh）+ macOS 签名/公证（或 quarantine 处理）。
- **启动链**：bingo 已反超（75ms vs bun 冷启动）。两个可控优化：`storage::cleanup` 同步扫盘可能卡启动（可后台化）；team 派生可后台化。
- **体积**：release profile 加 `strip = true`（20MB→17MB），零成本。
- 发布流程（dev→PR→main→tag→CI）维持现状即可。

---

## 4. 合并去重后的高 ROI 借鉴清单（P0 / P1 / P2）

### P0 — 正确性 / 安全，改动小，立即做

| # | 借鉴项 | 来源 | 关键落点 |
|---|---|---|---|
| 1 | **Memory 生命周期修正**：最近优先截取 + 200 行后不静默丢 + 字节上限 + git common root 记忆键 | devex | src/memory.rs |
| 2 | **每工具 InterruptBehavior + typed interrupt reason**：`InterruptReason::{NewInput,UserCancel,Shutdown}` + `InterruptBehavior::{Cancel,Block}`；远端写/发/删默认 Block | dev | src/tool/mod.rs、src/tool/executor.rs |
| 3 | **修 Hook ask 的 `unreachable!` panic 路径** + 补回归测试 | devex | src/hooks.rs、src/query.rs gate_tool |
| 4 | **压缩效果观测 + rapid-refill breaker**：`CompactOutcome{before,after,replaced,duration}` + 压缩后仍越阈/1-2 轮内再触发则停止并 warning | dev | src/compact.rs |
| 5 | **typed PermissionDecision**：behavior + updatedInput + decisionReason{rule/mode/hook/safety} | devex | src/query.rs、src/permission.rs |

### P1 — 结构性改进，中等成本

| # | 借鉴项 | 来源 | 关键落点 |
|---|---|---|---|
| 6 | **fullscreen 消息/block 虚拟化 + 高度缓存**（ratatui 能力边界内：block 级 + virtual_top/total_rows） | ui-ux | src/tui/chat_tail.rs build_rows、src/tui/app.rs |
| 7 | **microcompact 请求投影**：旧且完整配对的 tool_result 换占位，保留 ID + 最近 N 组；完整 transcript 不动；投影后仍越阈才 summary | dev | src/compact.rs、query_loop 请求前构造 request view |
| 8 | **CompactBoundary 持久化**：`TranscriptRecord::{Message, CompactBoundary}`，resume 回放 | dev | src/transcript.rs |
| 9 | **统一工具 active verb 表**：Reading/Writing/Editing/Searching/Running，复用 tool_result 与 running_status 两处 | ui-ux | src/tui/activities.rs |
| 10 | **大结果分块展开 + 长 Bash 有界 live tail**（UI 行预算与字符预算分开） | ui-ux | src/tui/chat.rs ToolDone 物化路径 |

### P2 — 体验 / 流程，低成本或需求驱动

| # | 借鉴项 | 来源 | 关键落点 |
|---|---|---|---|
| 11 | 终端通知 `auto/bell/disabled` → iTerm2/kitty/Ghostty adapter | ui-ux | src/tui 新 notifier 层 |
| 12 | 滚动位置反馈：viewport 页 + Ctrl+End + 离底提示 | ui-ux | src/tui/chat_tail.rs |
| 13 | Hooks P0 扩展：PermissionRequest + PostToolUseFailure + CwdChanged + `BINGO_ENV_FILE`/`CLAUDE_ENV_FILE` | devex | src/hooks.rs、src/settings.rs |
| 14 | 配置单一来源：schemars JSON Schema（已有依赖）+ `Option<bool>`/`Option<Vec>` tri-state + CommandSpec 单 registry | devex | src/settings.rs、src/tui/slash.rs |
| 15 | 诊断命令 `/hooks` `/memory` `/doctor` | devex | src/tui/chat.rs |
| 16 | 分发：一键安装脚本 + macOS 公证；release `strip=true`；storage::cleanup 后台化 | deploy | CI、Cargo.toml |

---

## 5. 不应照搬清单（防过度工程）

| 项 | 为什么不照搬 |
|---|---|
| StreamingToolExecutor（边流边跑工具） | 先测量流尾与工具 I/O 重叠收益；会显著增加 retry/fallback/权限/副作用复杂度 |
| precompute compaction 后台预计算 | 先补效果观测与失效规则；当前压缩点等待一次模型调用未必是瓶颈 |
| GrowthBook + 1700 个 tengu_* flags | 本地 typed registry 足够；bingo 是单机 harness |
| agent/http/prompt 类型 hooks | 安全面与实现复杂度不成比例；command hooks 优先 |
| 完整 memdir / dream / teamMemorySync 分布式 | 先修单文件正确性；topic 化第二期；团队同步先共享 MEMORY.md 文件 |
| React/Yoga/Offscreen API 移植 | ratatui 不需要；block 级虚拟化即可 |
| Anthropic cache-edit microcompact | provider 专属优化，不适合多 provider harness |
| 外部 vim/pager 接管 raw terminal | 与 raw mode/alternate screen 冲突 |
| 默认自动备用模型 | 模型/成本/thinking 变化应由用户显式配置 |

---

## 6. 方法、证据边界与后续

- **证据边界**：CC 侧结论来自 2.1.88 泄露 TS（`leaked-src/`，行号只证明 88 版）与 2.1.221 二进制解包（`output/analysis/`）；bingo 侧来自源码直接核对（文件:行可查）。两版独立证据共同支撑的稳定不变量：实际 tool block 驱动、safe/unsafe 调度、统一权限闸门、tool result 配对、压缩家族。
- **未验证项**：本报告为只读分析，未跑构建/测试；所有"建议"未在 bingo 上验证过。U+2028/2029 属低优先级防御项，当前无 JS 消费端。
- **成员报告原始文件**：`/tmp/dev-report.md`（282 行）、`/tmp/devex-report.md`（1061 行）、`/tmp/uiux-report.md`。
- **建议下一步**：从 P0 #1（Memory 正确性）与 #3（Hook panic）两个纯正确性修复起步，先落测试再改实现；P1 #6（fullscreen 虚拟化）前先跑一次 1k/5k 条消息的 profile 基准确认瓶颈在布局层。
