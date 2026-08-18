# bingo 多 agent / team / room × JSON 协议 — 专项分析与事件设计

> **状态（2026-08-18）**：事件设计部分已被 `notes/design/gui-app-server.md`（含 Amendments）
> 取代（对等模型→ConversationKind、agent.history→conversation/read、乙案、逐成员元数据
> 均已并入）。本文的内部信号盘点（§3）、四个硬缺口（§3.2）、"半活"bug（§2）与
> 会话模型映射（§4.5）仍是实施时的一手证据。

> 日期：2026-08-18 · 母报告：`notes/json-events-gap-analysis.md`（本文展开其 §3-C 与 P2#10）
> bingo 侧来自源码深查（UiHooks/UiEvent/ConvKey/AgentRegistry/ChannelRegistry 全量核对）；
> CC/Codex 侧沿用母报告证据。只读分析，未动码。

---

## 0. 一句话总判断

**结构性事实：CC 与 Codex 的多 agent 都是层级式（派生者→子 agent 树），bingo 是对等式
（Slack 房间里的同事互发）——两家的事件模型都不能整体照搬**，能借的是管线件（寻址字段、
关联三元组、快照+增量），域事件必须从 bingo 自己的模型里长出来。而 bingo 内部其实
**已经备好了大半管线**：每个 UiEvent 都包在 `Addressed { to: ConvKey }` 里（`ConvKey =
Main | Agent(name) | Room(name)`，src/ui.rs:120/146），协议要做的本质上是**把 ConvKey 搬上 wire**。
真正的硬缺口有四个：EventSink 黑洞、AskFn auto-deny、JsonSession 单 turn 槽位、turnId 归属。
另有**一处现在就存在的 bug 级不一致**（见 §2），与是否解除硬关无关。

---

## 1. 三方多 agent 事件模型对比

| 维度 | bingo（内部） | Claude Code | Codex |
|---|---|---|---|
| 拓扑 | **对等**：注册表里有名字的可写给任何有名字的（D137，address.rs:62 只查发送方） | **树**：Task 工具派生子 agent | **树**：spawn_agent/send_input/wait/close_agent |
| 事件寻址 | `Addressed{to: ConvKey}`（Main/Agent；Room 永不上信封——房间是日志不是 turn loop） | 单流 + `parent_tool_use_id` 一个字段 | 每 agent 一个 thread；delta 带 `(thread_id, turn_id, item_id)` 三元组 |
| 生命周期事件 | 无事件，`AgentState{Running,Idle,Stopped}` 靠轮询 | 无（子 agent 只是被标记的消息） | Collab 五对 Begin/End + `agents_states` 快照 |
| agent 间通信 | inbox/ack/mention 账本（一等公民） | 无此概念 | `InterAgentCommunication` op + `parent_turn_id`/`root_turn_id` 因果链 |
| 房间/频道 | 一等公民（seq 全序日志、@ 债务、serial staleness） | 无 | 无 |
| usage 归属 | 每 agent 自己的 query loop 各自计 | `usage`(主循环) vs `model_usage`(全树) | 每 thread 各自 TokenCount |

**借鉴判断**：学 CC 的"单流 + 一个寻址字段"（最小加法）；学 Codex 的"关联三元组"
（bingo 对应 `(agent, turnId, toolCallId)`）与"快照+增量"（`agents_states` → `team.snapshot`）；
**不学**两家的树形因果链——bingo 的 from/to 消息语义天然承载因果，房间与 ack 是它独有的、
两家都没有的领域，事件必须自己设计。

---

## 2. 现状：json 模式下多 agent 域是"半活"的（bug 级不一致，现在就该修）

探查发现硬关只关了三处（auto-spawn @ main.rs:459、记忆持久化 @ :577、share store @ :434），
但**工具装配与 system prompt 没有任何 json gate**（src/tools.rs:71-98、main.rs:278/291）：

- JSON 模式下 main 的 system prompt 照常注入 `crew_note`（全树名册）与 `MAIN_CHANNEL_NOTE`
  （房间礼仪）——**模型对着一份永远起不来的名册**，`SendMessage(to:"@Linh")` 得到
  `no subagent named Linh`；
- 模型可以 `Team(action=Start)` **手动把队伍起起来，绕过硬关**——然后：
  - `AgentRegistry.events` 只在 tui/mod.rs:326 被设置 → JSON 模式 `sink_for()` 返回 None →
    **子 agent 的全部 TurnStart/TextDelta/ToolDone/Mail/Inbound 进黑洞**（tool/agent.rs:154
    `EventSink::detached`）；
  - `AgentRegistry.ask` 只在 tui/mod.rs:324 与 `--print` 分支被 attach →
    **子 agent 的权限请求一律 auto-deny**（tool/agent.rs:341）；
  - mail 静躺 `main_mail` 直到客户端下一次 `turn.start`（`digest_mail` 去抖只活在
    chat_tail.rs:503，JSON 模式无人驱动）；`main_arrivals` 无人 drain，堆到 256 丢最旧。

**阶段 0 修复**（独立于 team-v1，小改动）：json 模式下不注入 crew_note/MAIN_CHANNEL_NOTE；
`Team(Start)` 返回明确错误（如 `TEAM_UNAVAILABLE`，recoverable）。让"关"关严，直到阶段 1-2 打开。

---

## 3. 内部信号源盘点（协议的原材料）

### 3.1 已经存在、只欠翻译的信号

| 内部信号 | 位置 | 语义 |
|---|---|---|
| `Addressed{to: ConvKey}` 信封 | ui.rs:146 | 每个 UiEvent 已带归属——寻址维度现成 |
| `UiEvent::Mail{from,text}` | agents.rs:1195 发 | **送达时刻**（deliver 即发） |
| `UiEvent::Inbound(String)` | query.rs:947/:1484 经 on_inbound | **阅读时刻**（吸收进 prompt 的逐字文本，可能晚几分钟） |
| `UiEvent::TurnStart/TurnEnd` | TurnBrackets（tool/agent.rs:105，Drop 保证配对） | agent run 的 turn 边界 |
| `WatchEvent{kind:Agent, dispatch, notifies_main}` | 运行行 | run 生死 + dispatch 位（TUI flow 白名单） |
| `ChannelMessage{seq,from,text,at,kind}` | channels.rs:94 | 房间日志行（Said/Membership） |
| `AgentStatus` 快照 | agents.rs:211 | roster 全字段（state/pending/unacked/tokens/…） |
| `Mention`/`MemberStanding`/`Ack` | channels.rs:270/301、agents.rs:284 | @ 债务与送达账本（轮询读） |

`Mail` vs `Inbound` 的两时刻区分（D135）是 bingo 独有的好设计，协议必须保留，不能合并成一个事件。

### 3.2 四个硬缺口（解除硬关的前置）

1. **EventSink 未接线**：需要一个 `Addressed{to} → CliEvent` 适配器并在 JSON 分支
   `session.agents.set_events(...)`（对标 tui/mod.rs:316-326）。`AdapterEvent` 的 mpsc
   已是多生产者汇聚点、`EventWriter.seq` 在汇聚后单点赋号——**并发写序架构已备好**。
2. **AskFn 未 attach**：`attach_ask(json_ask(event_tx))`，prompt.request 需能表达"这是谁的请求"。
3. **`active: Option<ActiveTurn>` 单槽**（json_events.rs:704）→ `HashMap<ConvKey, ActiveTurn>`；
   "a turn is already active"（:859）按 key 判定。
4. **turnId 归属**：现在必须由客户端提供；agent 的 run 是运行时自发的（唤醒/续跑），
   需要服务端生成 id + `turn.started.commandId` 变为可选。

---

## 4. 事件设计提案（全部经 capability `team-v1` 门控，加法演进）

### 4.1 寻址维度

- `EventBase` 加 `agent?: string`（缺省 = main）——一处改动覆盖全部现有事件
  （text.delta / tool.* / prompt.request / turn.* 即刻获得归属），忠实于内部
  `ConvKey::Main|Agent`。
- **`room` 不进 EventBase**：内部 Room 永不出现在 Addressed 信封上（房间是日志不是
  turn loop），wire 上同样由专门事件携带 `room` 字段——协议形状忠实于域模型。

### 4.2 新出站事件

| 事件 | 字段 | 发射点 |
|---|---|---|
| `agent.state` | `agent, state: idle\|running\|stopped, kind: crew\|hire, def?, reason?` | registry 状态迁移处（spawn/mark_idle/finish/stop/复活/hire 释放）——替代轮询 |
| `room.message` | `room, seq, from, at, kind: said\|membership, text, mentions: [name]` | `deliver_post` 出口（channels.post 之后）——user/agent/main 发言同一事件 |
| `room.updated` | `room, members, mode, frozen` | 建房/invite/kick/freeze |
| `mail.delivered` | `from, to, text` | `agents.deliver` + main_mail push——**送达时刻** |
| `message.inbound` | `agent, text`（模型所见逐字，含 marker） | `on_inbound`——**阅读时刻** |
| `mail.waiting` | `count, urgent` | main_mail 去抖窗口后发一次（去抖逻辑从 chat_tail 提到域层） |

现有事件的加法：`turn.started` 加 `agent?` 与 `origin: client|dispatch|delivery|continuation`
（对应内部 dispatch 位与 continuation 三态——GUI 用它决定什么钉进 main 的流，
与 TUI 的 flow 白名单同一判据）；`turn.completed/cancelled`、`prompt.request` 同获 `agent?`。

### 4.3 新入站命令

| 命令 | 字段 | 语义 |
|---|---|---|
| `message.send` | `commandId, to: "@name"\|"#room", text` | 复用 TUI 同一条路径（`deliver` / `deliver_post`，与 parse_direct_send 同源）；serial bounce 返回 recoverable error（对应 `Delivery::Rejected`）；成功回 `message.sent{commandId, seq?}` |
| `team.snapshot` | `commandId` | 返回 roster（AgentStatus 全字段 + thinking/cwd，见 §4.5）+ rooms（members/mode/seq/tail N 条）+ standings（read_to/owes）——**快照+增量**：连接时全量，此后靠 `room.message`/`agent.state` 增量 |
| `agent.history` | `commandId, agent` | 成员自己的历史（`view_of` → 域层化的 `perspective::walk` 走查后下发，见 §4.5）——GUI 重连后渲染成员页的依据 |

**未读不上 wire**：协议只给 `seq` 与 `read_to`，unread 由客户端推导——与 TUI 同一哲学
（buffer.rs:80 "Unread is derived, not counted"），badge 是视图层的事。

### 4.4 main 唤醒的驱动模型（已裁决：乙案）

team 打破了 json_hooks 注释里"a JSON host drives one turn at a time"的旧前提。曾有两案：
甲（main 客户端驱动，服务端只发 `mail.waiting`）与乙（去抖搬进域层，main 服务端自发，
`origin: auto`）。**2026-08-18 用户裁定"GUI 与 CLI 能力完全对齐"，据此定乙案**——
两个前端同一行为正是对齐的定义；digest_mail 的去抖从 chat_tail.rs:503 提进域层，
TUI 与 JSON 共用同一唤醒逻辑（顺带消除一处双实现漂移风险）。`mail.waiting` 事件保留
（GUI 显示"收信中"状态用），客户端节流留作未来 capability（如 `mail.hold`），有需求再加。

### 4.5 会话模型的映射（2026-08-18 核对补充）

内部事实（已核对）：**多 agent = 多个独立 Session**。每个成员是自己的 `Arc<Session>`——
自己的 `Runtime`（model/provider/thinking watch 通道 + transcript 槽，query_session.rs:16-28）、
自己的 system blocks 与 history、`instance: Some(name)`；模型/provider/thinking 逐实例可覆盖
（build_member，team.rs:1242），子团队 node 可在别的目录/仓库。共享的只有注册表
（agents/channels/watch/tasks/attachments）。

**wire 映射原则**：`sessionId` = **workspace**（main transcript stem，rename/delete/close
都是 workspace 操作）；成员会话的 wire 身份 = **稳定的实例名**（claim_name 全树唯一），
即 `agent` 字段——不给成员发 wire sessionId（成员的持久身份是
project+branch+team+member 键的记忆文件，不是可 resume 的 transcript stem）。
寻址上忠实；但"成员是独立会话"还要求三处补充，否则 GUI 会把 main 的元数据错套在成员头上：

1. **逐成员元数据上 wire**：`session.ready.metadata` 只描述 main；成员各有自己的
   model/provider/thinking/cwd。`team.snapshot` 的 roster 与 `agent.state` 须带
   `model, provider, thinking, cwd, def`（`AgentStatus` 已有 model/provider，缺 thinking/cwd，
   agents.rs:211——域层同步补）。
2. **`agent.history` 入站命令**：GUI 重连后要能取成员自己的历史（数据源 = `view_of`，
   agents.rs:803）。归属走查 `perspective::walk` 须从 tui 层提升到域层、wire 下发走查后的
   posts——两个前端共用同一个 walker，正是 D130/D132"归属走查只能有一份"的教训，
   与 §4.4 乙案同一原则。
3. **归账按成员**：`turn.completed.usage` 与 `context.usage` 经 `EventBase.agent` 逐成员归属
   ——每个成员自己的模型有自己的上下文窗口，不存在全局水位。

另两条显式规则：**`turn.start` 只对 main**——客户端不能直接起成员的 turn，成员只经
`message.send` 唤醒（与 TUI 一致：用户只能靠发消息/assign 触达成员）；workspace resume
（`--session`）重派 crew、成员记忆走指针（D51），房间历史空缺到阶段 3。

### 4.6 排序与关联

- 全局 `seq`（EventWriter 单点赋号）给跨 agent 的全序；`room.message.seq` 给房间内全序——
  两层本就存在，不新造。
- 关联三元组 `(agent, turnId, toolCallId)` 对齐 Codex 的 `(thread_id, turn_id, item_id)` 经验；
  `prompt.request` 靠 `agent + turnId` 表达"这是 @scout 的 turn 在要权限"。

---

## 5. 分阶段计划

> **定位（2026-08-18 用户裁决）**：GUI 与 CLI 能力完全对齐是产品目标——team 是主线，
> 不是需求驱动的可选项。阶段 0 是**过桥修复**（堵住"半活"bug 直到阶段 2 落地），
> 不是搁置；阶段 2 落地即撤销 `TEAM_UNAVAILABLE`。对齐验收尺见母报告 §4.5 全量对齐矩阵。

| 阶段 | 内容 | 前置 |
|---|---|---|
| **0. 关严过桥**（bug 修复，立即可做；阶段 2 落地即撤销） | json 模式不注入 crew_note/MAIN_CHANNEL_NOTE；`Team(Start)` 返回 `TEAM_UNAVAILABLE`（recoverable）；顺手修 main_arrivals 无人 drain | 无 |
| **1. 接线**（协议面小改） | `EventBase.agent` 寻址字段（capability `team-v1` 声明）；`Addressed→CliEvent` 适配器 + `set_events`；`attach_ask`（子 agent 权限走 prompt.request）；`active` 多槽化；服务端 turnId + `turn.started.origin` | 母报告 P0#1 capabilities 机制 |
| **2. 事件族**（GUI 可做完整 Slack 界面） | `agent.state` / `room.message` / `room.updated` / `mail.delivered` / `message.inbound` / `mail.waiting`；入站 `message.send` / `team.snapshot` / `agent.history`；逐成员元数据（AgentStatus 补 thinking/cwd）；`perspective::walk` 提升到域层；解除 main.rs:459/:577/:434 三处硬关 | 阶段 1；§4.4 已裁决（乙案） |
| **3. 持久化**（需求驱动） | 房间日志持久化（ShareStore 的 `ChannelShare` 已在写、从不读回——最近的砖）→ resume/重连补齐房间历史；ack/mention 债务增量事件（若 snapshot 轮询不够再加） | 阶段 2 |

每阶段的测试跟母报告 P0#2 的 golden 测试同一形制：新事件先锁 JSON 形状再接线。

---

## 6. 不照搬清单

| 项 | 为什么 |
|---|---|
| Codex Collab 五对 Begin/End 事件 | bingo agent 生命周期是三态状态机，一个 `agent.state` 事件够；五对事件是树形派生模型的形状 |
| CC `parent_tool_use_id` 的树语义 | bingo 是对等网络，`agent` 字段是"栏目归属"不是"父子"；hire（Agent 工具）的 run 同样用 `agent` 字段即可，树关系由 `origin: dispatch` 表达 |
| 树形因果链（parent/root_turn_id） | bingo 的 from/to + ack 账本已承载因果，再加一套是重复记账 |
| ack/mention 债务的逐迁移事件 | 先走 `team.snapshot` 查询；GUI 真需要实时债务提醒再加增量事件（YAGNI） |
| 每 agent 独立事件流/订阅过滤 | 单流 + `agent` 字段起步；delta 流量成为问题时再谈订阅（先测量） |

---

## 7. 证据边界

- bingo 侧：源码深查（UiHooks 12 字段、UiEvent 27 变体、ConvKey 信封、EventSink/AskFn
  接线点、JsonSession 单槽、三处硬关与工具装配无 gate）均有 文件:行 依据；
  "半活"结论由 tools.rs:71-98 与 main.rs:278/291 无 gate 直接推得，未实跑复现。
- 房间日志不持久、成员记忆是"指针不预载"（D51）为源码事实——阶段 3 的持久化是新域工程，
  不只是协议工程。
- CC/Codex 侧沿用母报告（2026-08-18 快照）。本文未修改任何代码，设计未经实现验证。
