# bingo JSON protocol v1 × Claude Code × Codex — 对比与改进计划

> **状态（2026-08-18）**：计划部分已被 `notes/design/gui-app-server.md`（含 Amendments）+
> `notes/design/gui-app-server-plan.md` 取代——v1 将被整体删除，不再渐进扩展。
> 本文的三方对比、反超清单、差距分析与 §4.5 对齐矩阵仍是证据基础（矩阵已被
> parity ledger 吸收）。

> 日期：2026-08-18 · 方法：bingo 侧源码核对（关键断言已亲验 `src/json_events.rs:1221` / `:1355`）；
> Claude Code 侧来自官方文档（headless / agent-loop / streaming-output）；
> Codex 侧来自 `openai/codex` main 2026-08-18 快照源码 + deepwiki。只读分析，未改协议代码。

---

## 0. 一句话总判断

**bingo protocol v1 的骨架质量在三方对比中处于第一梯队**——双向、显式版本、gapless seq、
运行时权限往返、结构化错误契约、stdout 协议纯净且有防漂移测试，其中数项反超两家。
差距集中在四处：**① 观测性**（usage 恒空、无时间戳、compact/retry 不可见）；
**② 流式粒度**（只有 text.delta）；**③ 多 agent 完全不可见**（bingo 的核心卖点在协议里为零）；
**④ 契约锁定与演进**（出站形状 10/18 无测试、无 schema、无规格文档、版本策略是硬断裂无演进通道）。

**定位判断（后续取舍的锚）**：Codex 有三层事件面——核心 SQ/EQ（进程内、不稳定）、
app-server JSON-RPC（富客户端：delta + 审批往返 + 全观测）、`exec --json` JSONL（脚本档：
刻意蒸馏掉 delta 的稳定小面）。bingo `--json-events` 的消费者是 GUI，**对标的是 app-server 档，
不是 exec 档**；CC 的 stream-json 介于两档之间。因此 delta、审批、观测性都该向富客户端档看齐。

---

## 1. 三方概览

| 维度 | bingo v1 | Claude Code stream-json | Codex |
|---|---|---|---|
| 定位/传输 | GUI 双向 stdio NDJSON | headless 脚本面（单向为主，`--input-format stream-json` 细节未公开） | 三层：SQ/EQ（内）/ app-server JSON-RPC（富客户端）/ exec JSONL（脚本） |
| 版本声明 | 每消息 `protocolVersion`，严格相等 | 无版本字段，`system/init.capabilities` 特性检测 | 无版本字段，`non_exhaustive` + serde alias + 随版 schema 导出 |
| 序号/事件 ID | `seq` 单调无洞 | `uuid`（无序号） | 无全局序号（Event.id 回显 submission id，非唯一） |
| 时间戳 | 仅 `tool.done.durationMs` | 仅个别（`ttft_ms`） | `started_at`/`completed_at`/`*_ms` 广布 |
| token/成本 | 字段存在**恒 null** | `result`: total_cost_usd / usage / model_usage / num_turns | `turn.completed.usage` 5 字段 + `TokenCount`（含 rate limits、context window） |
| 增量流式 | 仅 `text.delta` | 转发完整 API 流事件（content_block_start/delta/stop） | 核心层 delta + 权威整块双发；exec 层全抑制 |
| 权限往返 | ✅ prompt.request/respond/resolved（2 选项） | ❌ CLI 面无运行时权限协议，须静态预配 | ✅ 审批往返，`ReviewDecision` 8 种决策 |
| 错误契约 | scope/code/level/recoverable | subtype 弱类型，无稳定码 | `codex_error_info` + StreamError/Warning 分层 |
| 多 agent | 完全 gated off（`--no-team`） | `parent_tool_use_id` 挂子 agent 消息 | collab items + `parent_turn_id`/`root_turn_id` 因果链 |
| schema/文档 | 无 | 文档为主，无 schema | `generate-ts` / `generate-json-schema` 随版导出（ts-rs + schemars 单源） |

---

## 2. bingo 已做对 / 反超（无需追赶）

1. **gapless seq**（`EventWriter::emit` 集中赋号，json_events.rs:598-607）：两家都没有；
   消费端可检测丢行。**反超。**
2. **显式 protocolVersion**（每事件 + 每命令）：两家都没有。**反超**（但"严格相等拒绝"策略见 §3-D4）。
3. **stdout 协议纯净 + 防漏测试**：黑盒断言 stderr 为空、人类 `[error]` 契约不泄漏到协议流
   （tests/cli_black_box.rs:236）。两家亦纯净，但 bingo 有防漂移测试锁着。
4. **运行时权限往返**：CC 的 stream-json **没有** wire 层权限协议（必须 `--allowedTools` 静态预配，
   `canUseTool` 是 SDK 进程内回调）；bingo 有完整 request/respond/resolved + 未答默认 deny +
   resolved reason。**反超 CC，接近 Codex。**
5. **结构化错误契约**（scope/code/level/recoverable，延续 bingo 错误码契约优势）：**反超 CC。**
6. **防御性与发现机制**：commandId 去重、1 MiB 命令行 / 8 MiB 事件上限、exit code 0/1/2、
   `--probe` 零副作用探测。两家无明确等价物。

---

## 3. 差距分析

### A. 观测性（改进计划主体）

| # | 差距 | bingo 现状 | 对照 |
|---|---|---|---|
| A1 | **usage 恒空** | `turn.completed.outputTokens` 唯一构造点硬编码 `None`（json_events.rs:1221）；`on_context_usage` 丢弃（:1355），代码注释自认"schema 还没长出这些字段" | CC result 报 cost/usage/model_usage/num_turns；Codex usage 5 字段 + TokenCount 带 context window——GUI 无法显示成本与上下文水位 |
| A2 | **无时间戳** | `EventBase` 只有 version/seq/sessionId | 跨进程缓冲下客户端收到时刻会失真；Codex 生命周期事件均带 at 字段 |
| A3 | **compact 不可见** | 压缩只走 `warning` 文本（D66/D77 补的） | CC 有 `system/compact_boundary`；GUI 无法结构化得知上下文被重写。与 cc-gap-analysis P1#8（CompactBoundary 持久化）同源 |
| A4 | **stream retry 不可见** | 无事件（:1352-1355 注释自认） | CC `api_retry{attempt,max_retries,retry_delay_ms,error_status}`；Codex `StreamError` |

### B. 流式粒度

- **B1 只有 text.delta**：无 thinking delta、无工具输出 live tail（`LiveBash::detached()`，:1404）、
  `tool.done` 一次性全量 output。按 §0 定位，应学 Codex 核心层的**双发模式**：delta 流直播、
  终态权威整块落账——bingo 已有权威终态（tool.done），只欠 delta。
- **B2 turn.completed 贫瘠**：只有 turnId + 恒空 outputTokens。Codex TurnComplete 有
  last_agent_message / duration / TTFT。

### C. 多 agent 不可见（最大产品面缺口）

- team 在 json 模式被硬关（main.rs:459、:577 gate on `!cli.json_events`）；他人 prose 被丢弃
  （`on_inbound: |_| {}`，:1394）。bingo 的核心卖点——Slack 式工作区、agent 互发——在协议里为零，
  GUI 只能拿到单 agent 视图。CC 用一个 `parent_tool_use_id` 就把子 agent 消息挂进同一流；
  Codex 有 collab items + parent/root_turn_id 因果链。
- **无 steering**（`no_steer()`，:1401）：CC 可经 input stream-json 中途注入消息；Codex 有 `turn/steer`。

### D. 契约锁定与演进

| # | 差距 | 说明 |
|---|---|---|
| D1 | **出站形状 10/18 无测试** | `CliEvent` 只有 Serialize；text.delta / tool.* / prompt.request / models.result / providers.result / turn.completed / *.ready 从未被断言过 JSON 形状；`--probe`/`--inspect` 零测试 |
| D2 | **无 schema 导出** | Codex ts-rs+schemars 单源随版导出；bingo `Cargo.toml:25` 已有 schemars 0.8 依赖却未用于协议 |
| D3 | **无规格文档** | 无 docs、四个实现提交无 D 记录、`AC-F2-2` 等验收文档不在仓库；最像规格的是两段 doc comment |
| D4 | **版本策略硬断裂** | 严格相等拒绝 v≠1，无演进通道。三方经验一致指向：**加法演进 + 特性检测**（CC capabilities 数组 / Codex non_exhaustive+alias），版本号只留给不兼容断裂 |

---

## 4. 改进计划

> **2026-08-18 裁决更新（用户）：GUI 能力与 CLI 能力完全对齐是产品目标。** 据此调整：
> ① 多 agent/team（原 P2#10）**升为主线**——四阶段计划见 `notes/json-events-team-design.md`，
> 阶段 0 立即做、阶段 1-2 排进主线，阶段 0 是过桥修复而非搁置；
> ② 权限三态（原 P2#11）与 steering（原 P2#12）随对齐主线升级——TUI 已有 AllowSession
> 与中途插话，wire 面必须跟上；
> ③ 验收尺从"GUI 需求驱动挑着做"改为 **§4.5 全量对齐矩阵**：UiHooks 12 回调 + UiEvent
> 27 变体，每个信号要么有 wire 对应，要么有显式"不上 wire"的裁决，不允许默认遗漏；
> ④ 该裁决顺带定了 team-design §4.4：main 唤醒**选乙案**（去抖搬进域层、服务端自发 turn，
> `origin: auto`）——两个前端同一行为正是"对齐"的定义，客户端节流留作未来 capability。

### P0 — 契约先行，小改动（先立演进机制，再加东西）

| # | 项 | 落点 | 备注 |
|---|---|---|---|
| 1 | **capabilities 机制**：三个 ready 事件的 metadata 加 `capabilities: [string]`；写明演进规则（新事件/新字段为加法、消费端忽略未知、版本号只用于断裂） | `CliSessionMetadata` / `ProbeMetadata` | 后续一切加法的门；对齐 CC v2.1.205+ 的做法 |
| 2 | **出站事件 golden 测试**：18 变体逐一 `serde_json::to_value` 对比 `json!` 字面量；补 `--probe`/`--inspect` 黑盒测试 | json_events.rs tests、tests/cli_black_box.rs | 修 D1；wire 契约从此有锁 |
| 3 | **usage 上报**：`turn.completed` 填 outputTokens 并扩成 `usage{input,output,cacheRead,cacheWrite}`；接入 `on_context_usage`，新增 `context.usage` 事件（含 contextWindow 水位） | :1221、:1355 | capability `usage-v1`；对齐 Codex Usage 形状 |
| 4 | **事件时间戳**：`EventWriter::emit` 统一注入 `ts`（unix ms），与 seq 同处一改 | `EventBase`（:516）、emit（:598-607） | 一处改动覆盖全部事件 |
| 5 | **协议规格文档**：`notes/design/json-protocol-v1.md`——wire 现状、三模式、exit code、演进规则；补 D 记录 | notes/ | 修 D3；把 §D4 规则写成契约 |

### P1 — 观测性与流式补全

| # | 项 | 备注 |
|---|---|---|
| 6 | `compact.boundary` 事件（preTokens/postTokens/replaced） | 与 cc-gap-analysis P0#4（压缩效果观测）同批做——内核先有数据，协议才有得报 |
| 7 | `stream.retry` 事件（attempt/maxRetries/delayMs/errorStatus） | 字段对齐 CC api_retry |
| 8 | `turn.completed` 扩 `durationMs` | 与 #3 usage 一起落 |
| 9 | `tool.output.delta`：有界 live tail，替换 `LiveBash::detached()` | 与 TUI 侧 live tail（cc-gap-analysis P1#10）共用同一数据源；capability 门控 |

### P2 — 原分级已被裁决更新改写（见 §4 顶部）

| # | 项 | 备注 |
|---|---|---|
| 10 | **多 agent 可见性** → **已升主线**：四阶段计划见 `notes/json-events-team-design.md`；阶段 0（关严过桥）立即，阶段 1-2 主线，含一处现存 bug 级不一致（json 模式多 agent 域"半活"）须先修 | 详见专项文档 |
| 11 | 权限选项扩展（allow / allow-session / deny）→ **随对齐主线升级** | 与 cc-gap-analysis P0#5（typed PermissionDecision）同批；capability 门控，正是 #1 机制的第一个受益者 |
| 12 | steering：`turn.steer` 入站命令 → **随对齐主线升级**（与 team 阶段 2 同期——steer 与 take_running/吸收共用 tool barrier 机制） | 对齐 Codex turn/steer |
| 13 | schema 导出：协议类型挂 schemars derive，`--json-events --schema` 随版输出 JSON Schema | 依赖已在（Cargo.toml:25）；Codex generate-json-schema 模式，承诺"schema 与二进制同版"即可，不承诺跨版冻结；仍为需求驱动 |
| 14 | 域动作命令化：`session.compact` / `session.rewind` 等 typed 入站命令（对应 TUI 斜杠命令底下的域动作） | 斜杠本身是前端 UX 不上 wire；其域动作按 GUI 需求节奏命令化 |

### 4.5 全量对齐矩阵（裁决后新增的验收尺）

内部信号（UiHooks 12 回调 + UiEvent 27 变体）逐一归位；此表是"完全对齐"的检查单，
新增信号必须同步登记。

**已有 wire 对应（8）**：TextDelta→`text.delta`；ToolReady→`tool.ready`；ToolDone→`tool.done`
（**缺 `diff` 字段，须补**——TUI 的 ToolCallDone 带 diff）；TurnStart/TurnEnd→`turn.started`/
`turn.completed`；Interrupted→`turn.cancelled`；Warning→`warning`；Error→`error`；
ModelsLoaded→`models.result`；ask_question→`prompt.request(kind:question)`；
ask→`prompt.request(kind:permission)`（**缺三态**，P2#11 补）。

**计划中已列（6）**：StreamRetry→`stream.retry`（P1#7）；ContextUsage→`context.usage`（P0#3）；
OutputTokens→`turn.completed.usage`（P0#3，authoritative 位并入）；live/BashTail→
`tool.output.delta`（P1#9）；ThinkingDelta→`thinking.delta`（P1）；steer/Steered→`turn.steer`
入站 + `turn.steered` 回执（P2#12）。

**team-v1 承载（3）**：Inbound→`message.inbound`；Mail→`mail.delivered`；
WatchEvent→`agent.state` / `room.message` 等（见专项文档 §4）。

**本次矩阵新补（4）**：ToolStart→`tool.started`（流式工具起点，GUI 即时反馈）；
ImageReady→`image.ready`（GUI 显示图片必需）；RoundEnd→`turn.round`（客户端 fold 分组边界）；
tool.done 补 `diff` 字段（同上）。

**裁为不上 wire（3 组）**：PinPanel/Unpin（TUI 视图脚手架）；SlashOutput/SlashError/SlashInfo
（斜杠是前端 UX，域动作走 P2#14 命令化）；RewindDone（归入 P2#14 的命令回执）。

### 依赖关系

`#1 capabilities` 先行 → `#3/#6/#7/#9/#11` 全部经 capability 门控加入；
`#2 golden 测试` 与 `#5 规格文档` 互为表里（测试锁形状，文档锁语义）；
`#6` 依赖内核压缩观测数据（cc-gap-analysis P0#4），`#9` 依赖 TUI live tail 数据源（P1#10）——
**json 协议是这些内核改进的出口，不要两份计划重复排期。**

---

## 5. 不照搬清单

| 项 | 为什么不照搬 |
|---|---|
| CC 转发原始 API 流事件（content_block_*） | bingo 是多 provider harness，wire 面应是 bingo 自己的抽象，不是 provider 的流事件 |
| Codex 内外双层（70+ EventMsg + 蒸馏层） | bingo 规模用单层收窄面即可；两层是 Codex 为"内层随便改"付的架构税 |
| JSON-RPC 化（app-server 式传输） | stdio NDJSON + commandId 配对已够；改传输层无增量收益 |
| 每消息 uuid（CC） | seq + sessionId 已可定位与去重；uuid 冗余 |
| 消息级版本协商 | capabilities 特性检测替代；三方实践都没有真协商 |
| U+2028/U+2029 转义 | 维持 cc-gap-analysis §3.4 结论：现代 JS 按 `\n` 拆行不受影响，出现按旧 JS 行语义的消费端再补 |

---

## 6. 证据边界

- bingo 侧：源码直查，关键两处（outputTokens 恒 None :1221、on_context_usage 丢弃 :1355）已亲验；
  事件清单与测试覆盖来自全文件核对（json_events.rs 1836 行单文件承载全协议）。
- CC 侧：官方文档（code.claude.com 的 headless / agent-loop / streaming-output）；
  "stream-json 无运行时权限协议"为文档推断（文档确认需静态预配，未见反例）；
  `--input-format stream-json` 输入格式官方未完整公开。
- Codex 侧：main 分支 2026-08-18 快照源码 + deepwiki；Codex 迭代极快（collab/realtime/guardian
  均为近期新增），对标发布版应以 `codex app-server generate-json-schema` 输出为准。
- 本报告为只读分析，未修改协议代码，所有建议未在 bingo 上验证。
