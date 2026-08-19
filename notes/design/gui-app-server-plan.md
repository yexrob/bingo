# GUI App-Server 实现方案（激进路线）

> 日期：2026-08-18 · 状态：已批准待实施
> 规范：`notes/design/gui-app-server.md`（含其 Amendments 节，本方案与之配套；冲突时以 Amendments 为准）
> 裁决链：GUI/CLI 完全对齐（用户）→ 采纳 app-server 架构（评估后用户批准）→
> **激进路线**（用户）：不走"TUI 先上 AppCore 再逐步搬"的保守分期，按最终形态直达。
> 实施：每批由 Opus 5 (max) agent 实现，Fable review 后合入 dev。D 编号预留 **D140–D149**。

---

## 0. 最终形态（目标一句话）

应用状态机全部沉入 `AppCore`（单 session actor），TUI 与 `bingo app-server`（JSON-RPC 2.0
/ NDJSON / stdio）是它的两个投影适配器，`--print` 是第三个薄客户端；`--json-events` 及其
全家（probe/inspect/json_hooks/JsonSession）删除，无兼容层。

```text
TUI adapter        JSON-RPC adapter        --print adapter
      \                  |                  /
       +------------ AppCore (session actor) ------------+
       | conversations/turns/items/queue/interactions     |
       | attention cursors · action registry · catalogs   |
       | agents/rooms/tasks registries (actor-owned)      |
       +----------------------+---------------------------+
                              |
                engine tasks（query loop / tools / agent runs）
                EngineEvent → mpsc → actor（唯一定序点）
```

### 模块布局（最终）

```text
src/app/
  mod.rs            // AppCore, AppLink, attach
  controller.rs     // actor 主循环：请求处理、定序、快照切割
  command.rs        // AppCommand / Submission / Action（统一动作枚举）
  event.rs          // AppEvent + EventMeta{seq, ts, sessionId, causedBy}
  snapshot.rs       // SessionSnapshot / ConversationSnapshot / summaries
  projection.rs     // perspective::walk + LineSource（从 tui 提升，唯一归属走查器）
  attention.rs      // 用户 read cursors / unread / mention obligations
  queue.rs          // 输入队列 + steer 仲裁（一场竞态一个赢家）
  interaction.rs    // permission/question 持久交互 + D81 守卫
  catalog.rs        // models/providers/skills/MCP 目录（session 前可用）
  ids.rs            // 服务端 opaque ID 铸造（conv_/turn_/item_/int_/op_/asset_ + epoch）
src/app_server/
  mod.rs protocol.rs stdio.rs schema.rs
src/engine/        // 原 query/tool 层的对内接口
  events.rs         // EngineEvent（原 UiHooks 回调的 typed 化，UiHooks 删除）
src/tui/            // 纯投影：渲染、按键、本地视图态（TuiEvent：PinPanel/折叠/翻页等）
```

### 不变式（验收以规范文档 "Lifecycle and ordering invariants" 15 条为准）

补充三条本仓库特有约束：
- **字节契约**：projection/walk/marker 解析绝不改动模型可见字节（summary_message 教训）；
- **错误码契约**：`bingoCode` 沿用 `src/error.rs` 稳定码 + 每 variant 防漂移测试，JSON-RPC
  `error.data` 是它的新出口；
- **stdout 纯净**：app-server 模式 stdout 只有协议帧，诊断走 stderr（沿用 v1 黑盒测试形制）。

---

## 1. 关键设计决定（实现前已裁定，不再讨论）

1. **Registry 收编**：`AgentRegistry` / `ChannelRegistry` / `WatchRegistry` 从
   `Arc<Mutex<…>>` 共享结构改为 **actor 私有状态**。engine task（agent query loop、工具执行）
   不再直接调 `deposit/deliver/post/finish`，改为向 actor 发消息（带 oneshot 回执）。
   inbox 唤醒 pulse、mention/ack 看门狗全部收进 actor。这是本次重构的心脏。
2. **主唤醒（乙案）**：`digest_mail` 去抖从 `chat_tail.rs:503` 进 `app/controller`，
   main 在所有前端自动起 turn（`origin: auto`）。
3. **房间历史持久化进 1.0**：每 session 一个 sidecar（建议 `{stem}.rooms.jsonl`，append-only：
   post / membership / seen 游标 / 用户 attention 游标），resume 时重放恢复房间与未读。
   实现细节实施者定，但格式须写进规范文档并有 golden 测试。
4. **时间戳**：`EventMeta` 含 `ts`（unix ms，actor 定序时盖章）；item/turn 终态带权威
   `startedAt/completedAt`。
5. **`turn/retrying`** 带 `attempt / maxAttempts / delayMs` + 检查点替换（removedItemIds）。
6. **agent 资源形状**显式含 `model/provider/thinking/cwd/def/kind/state`（域层 `AgentStatus`
   同步补 thinking/cwd）。
7. **`catalog/read` 在 `session/start` 之前可用**（GUI 建会话前选 provider/model，
   接替今天 `--inspect` 的职责）。
8. **`--print` 是 AppCore 薄客户端**：驱动一次 submit、顺序打印 AppEvent 文本投影。
9. **usage/compaction 观测是内核前置**：`on_context_usage`、`StopReason.output_tokens`
   接进 actor（`turn/usageUpdated`）；`compact.rs` 产出
   `CompactOutcome{before,after,replaced,duration}` 支撑 compaction item
   （即 cc-gap-analysis P0#4，在 B3 内完成）。
10. **交互经 `interaction/respond`**：engine task 的 ask/ask_question oneshot 由 actor 持有并
    解析；快照恢复后仍可回答；未答且连接关闭 → fail-closed deny（沿用 v1 语义）。

---

## 2. 激进路线的纪律（不是无秩序）

- **契约先行**：B1 先把全部 wire 类型 + AppCore 类型 + schema + fixtures 落死，
  之后所有批次向契约实现，不反向迁就。
- **每批合入 dev 且四门全绿**：`cargo fmt --all -- --check` / `check --locked --all-targets` /
  `clippy --locked --all-targets -- -D warnings` / `test --locked --all-targets`。
  合并禁 `git add -A`（逐文件 add）。每批末尾在 `notes/research.md` 追加 D 记录。
- **shim 政策**：B2–B6 期间允许 TUI 以**直接函数调用**借用已搬进 app/ 的逻辑保持编译绿，
  B7 一次性换成 AppLink 帧。shim 只是调用点，禁止复制逻辑、禁止为 shim 设计中间架构。
- **测试搬家不删失**：现有域行为单测随逻辑迁移改挂到 AppCore 接口；只有纯视图层测试可删。
  B0 记录基线测试数，战役结束数量只增不减（视图层删除数须在 D 记录中列明）。
- **review 节拍**：每批完成 → Fable review（对契约、对不变式、对本方案）→ 合入。

---

## 3. 迁移地图（从哪 → 到哪）

| 现在 | 去向 |
|---|---|
| `src/json_events.rs`（1836 行）、`--json-events/--probe/--inspect`、`main.rs` 三处 json gate（:434/:459/:577）与 json 分支、`tests/cli_black_box.rs` 7 个 json 测试 | **删除**（B0） |
| `src/query.rs` `UiHooks`（12 字段结构体）| `src/engine/events.rs` `EngineEvent` 枚举 + mpsc 入 actor；ask/ask_question/steer/live 变 actor 请求（B2） |
| `src/ui.rs` `UiEvent` 27 变体 + `Addressed{ConvKey}` | 拆分：语义部分 → `AppEvent`；`PinPanel/Unpin/Slash*` → `TuiEvent`（tui 私有）（B2） |
| `src/tui/chat.rs` `submit`/路由/队列/steer、`src/tui/buffer.rs` `deliver`/`SubmitTarget` | `app/controller` + `app/queue`（`conversation/submit` 唯一提交路径）（B3） |
| `src/tui/perspective.rs` walk、`buffer.rs` `LineSource` | `app/projection.rs`（B4） |
| `src/tui/chat_tail.rs` `digest_mail` 去抖 | `app/controller`（乙案）（B4） |
| `src/agents.rs` / `src/channels.rs` / `src/watch.rs` 的共享可变结构 | actor 私有状态；engine 经消息访问（B2b） |
| `src/tool/agent.rs` `spawn_agent_loop`/`TurnBrackets`/watch 注册 | engine task + actor turn 生命周期（服务端 turnId）（B3/B4） |
| `src/tui/slash.rs` + `src/team_cmd.rs` + `Chat` 里的命令分派 | `app/command.rs` `Action` 枚举 + 单 registry（`action/list` 元数据同源）（B5） |
| `src/error.rs` 错误码 | 保留；新增 JSON-RPC error.data 映射（B1） |
| `--print`（`main.rs:561` 一带） | AppCore 薄客户端（B8） |

---

## 4. 批次（每批 = 一个 Opus 5 max 任务，含验收）

### B0 · 清场与基线（S）
删除上表第一行全部内容；解除 share/team/persist 三处 json gate（此后唯一启动路径）；
`--session` 精确匹配逻辑一并删（session 选择将由 `session/resume` 承担，TUI `--continue` 保留）。
**验收**：四门绿；基线测试数记录在 D140；`rg json_events` 零命中；`--help` 不再含三个 flag。

### B1 · 契约（M）
`app_server/protocol.rs` 全 wire 类型（规范 §Wire + Amendments：initialize/shutdown、
session/conversation/turn/queue/interaction/action/catalog/resource/asset 方法族、通知族、
`EventMeta{seq,ts,sessionId,causedBy}`、错误映射）；`app/{command,event,snapshot}.rs` 内核类型；
schemars derive + `bingo app-server generate-schema --out` 确定性输出并提交 schema bundle；
CI 漂移测试；**每个 request/response/notification/error variant 一个 fixture 往返测试**。
**验收**：四门绿 + schema 确定性测试 + fixtures 全绿。此批不接行为。

### B2 · 内核骨架（L，可拆 B2a/B2b）
B2a：AppCore actor（attach/AppLink、seq+ts 定序、快照切割屏障、ID 铸造、epoch）；
`EngineEvent` 替换 `UiHooks`（engine 调用方全部改写）；TUI 经 shim 保持绿。
B2b：三 registry 收编为 actor 状态；agent run loop、看门狗、inbox pulse 改走 actor 消息。
**验收**：四门绿；现有 agents/channels 域测试改挂 actor 接口后全绿；
并发冒烟：N 个 agent run 并发下 seq 严格递增无洞（新测试）。

> **B2b 落地记（2026-08-18）**：三注册表已收编为 actor 私有状态（D143，三提交）。三个形状：
> report（有序发送不等回执）/ question（`Answer<T>`，`.await` 或前端同步接缝上的 `.now()`）/
> listing（`tokio::sync::watch` 替换式快照）。actor 改跑独立线程（结构性保证"处理消息期间不 await"），
> 收件箱改无界（同步调用方无法等待）。`Answer::now()` 的阻塞半边标记为 shim，B7 移除。

> **B2a review 裁决（2026-08-18，Fable）**：① "首次快照切割前不投事件"（抑制而非缓冲）
> 接受——是对规范的相容收紧，防止 actor 内存随慢前端增长；B6 须把"attachment 的通知流
> 始于它的第一次快照读"写进协议文档。② "帧写失败即断开 attachment、actor 永不等待前端"
> 接受为 actor 层最后手段；B6 的传输适配器负责规范要求的有界缓冲 + 写超时 + 尽力
> CLIENT_TOO_SLOW，帧通道容量要给足。③ 流帧脚手架（MessageStart/BlockStop 等六变体）
> 不跨入 EngineEvent 接受——framing 是 engine 内务，语义面（ToolUseStarted/StopReason）
> 已全量过桥。附：`&EngineHost` 把"一 run 一 host"降为约定，B3 由 actor 落为强制；
> `AppError::Unserved` 须于 B5 清零。

> **B2b review 裁决（2026-08-18，Fable）**：三个偏离全部核准——① actor 跑独立 OS 线程 +
> 无界收件箱：同步调用方无法等待、死锁类整体消除，进程内生产者受 turn 并发度约束；
> B6 的 wire 入口仍按规范有界。② `Answer::now()`（自写 parker，96 个调用点）核准为 shim，
> **B7 必须清零**。③ 读模型携带活句柄（`Arc<Session>`、`Arc<Mutex<AgentProgress>>`）是
> "一切变更经 actor"的**特许例外**，边界：只许单调进度计数与现场采样走这条缝，
> 任何承载不变式的状态不得经活句柄变更——B4/B7 不得误读扩用。
> ④ 已知债务转 B3：D29 循环现在停一根线程（AgentRegistry 持 Session、Session 持句柄）——
> B3 须落 AppCore/session 关闭路径（actor 收摊、线程退出），wire 的 session/close 在 B6 接。

### B3 · 会话与 turn 行为（L）
`conversation/submit` 全路由（composer 解析从 chat/bufferview 迁入）；queue/steer 仲裁
（FIFO 前缀、吸收/回收一场竞态）；turn 生命周期**恰好一个终态**（error 不再替代终态——
修 v1 遗留 bug）；retry 检查点替换（决定 5）；interactions（D81 守卫、advertised decisions、
fail-closed）；usage/context 接线 + `CompactOutcome`（决定 9）。
**验收**：四门绿 + 规范 §Verification "Core behavior tests" 前六族场景测试绿。

> **B1 review 裁决（2026-08-18，Fable）**：① 后台命令的 watch 迁移须有 typed 通知——B4 给契约
> 增补 `command/changed`（parity ledger "typed resource updates" 行要求之，resource/read 轮询
> 不满足）；契约扩展流程 = 改类型 + 重生成 bundle + 补 fixtures，一个提交内完成。
> ② ResponseFrame 的 "exactly one of result/error" 在 Rust 构造器强制、schema 只表达
> "at most one"——接受为已知限界（D141 已记）。③ `TurnOrigin{user|queue|peer|auto|shell}`
> 与 `OperationKind` 是 B1 的自裁词表，B3/B5 需要时按加法扩展。

> **B3 review 裁决（2026-08-19，Fable）**：① shift+tab 纳入 D81 守卫**保留**——键盘核准且授最宽
> 权限，旧面只守数字与 Enter 是漏洞不是行为。② `SubmitRequest.main_busy: Option<bool>`（TUI 路径
> 旁通）与 ~30 个直接设 conv.busy 的测试，B7 清零。③ `Route::Deliver` 回调用方执行、
> `EngineEvent::{Warning,Inbound}` 被丢弃——归 B4 接管。④ "读者视图先于回执发布"的普遍化
> 修复核准。附注：B3 顺手修了"shell 行排队后按散文 drain"的潜在缺陷；StreamRetry 扩展
> 确认仅 engine 层，schema bundle 零改动（已亲验）。

### B4 · 协作域（L）
agent/room conversations；walk/LineSource 提升（决定：单走查器）；房间消息 = message item；
mail digest 入核（乙案）；delivery/ack 结构化状态（D137 语义：同事散文不结算 ack）；
attention（markRead/unread/mention obligations）；**房间+attention 持久化 sidecar + resume 重放**
（决定 3，含 golden 测试）；team 启动为 operation；agent 资源形状（决定 6）。
**验收**：四门绿 + 协作场景测试（DM 三声部 marker、@债务、serial bounce、resume 恢复房间）。

> **B4 review 裁决（2026-08-19，Fable）**：① 投递状态映射**核准**（域 Queued→wire delivered、
> 域 Delivered{run}→wire read）——正确对应 D135 两时刻（入箱=送达、入 run=已读），恒等映射
> 反而会把已入箱的信显示成"未送达"；wire `queued` 保留给未来异步投递间隙。
> ② `Route::Deliver` 的唤醒半边留在调用方，B7 收；由此 B2a 裁决"Unserved 于 B5 清零"**修订**为：
> 除 submit 的 Deliver disposition（B7）外全部清零。③ agent 会话的 attention 不随 resume
> 恢复，接受为 1.0 已知限界（规范已载明）。④ `BackgroundCommandResource.exit_code` 恒缺
> （watch 表无退出码）——B8 收尾时给 watch 表补 exit status。⑤ `/team status` 增印成员
> thinking/cwd 的可见变化核准（Amendment #5 的读者）。

### B5 · 动作注册表与目录（M）
`Action` 枚举 + 单 registry（`action/list` 元数据与分派同源 + 完备性测试，终结斜杠三表漂移）；
斜杠家族逐一迁出 Chat/team_cmd；`catalog/read`（session 前可用，决定 7）/`config/read`/
`resource/read` 分页；`asset/registerPath`/`readChunk`。
**验收**：四门绿 + registry 完备性测试 + 每 Action 至少一条行为测试。

> **B5 review 裁决（2026-08-19，Fable）**：① `Turn`/`Shell` disposition 与 `Deliver` 同墙
> （都要 engine），并归 B7——对任务书的偏离核准，诚实记录优于硬凑。② `session/start`/`resume`
> 归 B6 传输层拥有（一个 AppCore 即一个 session，换 session 是换 actor，不是问 actor）——正确。
> ③ `provider.login` 不携带 `--device-auth`/`--manual <token>` 核准：凭据不进 wire 请求；
> GUI 侧认证是未来的设计决定（operation 式 device flow），不是加个字段。④ 四个终端 handler
> 二次读参、core/engine 双镜像、`ArgumentSource` 留进程内——均为已标记 shim/已知限界，B7 收敛。
> ⑤ D81 session 级授权未出现在 `config/read`：**B7 跑 parity ledger 时核验** TUI /permissions
> 是否展示 session 规则，若展示则 wire 必须跟上（B8 补），不许静默分叉。

### B6 · app-server 传输（M）
stdio JSON-RPC 循环（framing、initialize 协商、有界队列、delta 合并、慢客户端关闭策略）；
黑盒场景套件（真进程 + fake provider，覆盖规范 §Black-box 列表；沿用 v1 黑盒的
隔离 HOME/stdout 纯净形制）。
**验收**：四门绿 + 黑盒全绿 + stderr/stdout 分离断言。

> **B6 落地记（2026-08-19）**：传输已上（D147，五提交）。三任务一循环，`select!` 偏向内核帧 →
> 不变式 #3 成为定序的事实而非纪律。两处停机修补：`settle()`（EOF/shutdown 前把已受理的请求
> 答完）与 `retire()`（被替换/关闭的 session 把链路读到尽头再放手）。`session/start`/`resume`
> 归传输（B5 裁决②），中间以 **lobby**（无 session 的 AppCore）让 catalog/session-list/delete/
> asset-chunk 继续作答；哪四个方法无需 session 由「声明错误里没有 NO_ACTIVE_SESSION」的测试锁死。
> 两处加法契约：`RequestId::Null`（JSON-RPC 要求）与 `EventMeta.coalescedFrom`（合并帧必须说明
> 它代表哪一段，否则 seq 不再无洞）——后者是本批唯一「宁可加字段也不留需求不做」的取舍，**请 review 重点看**。
> 退出码 0/1/2 已定义并测试。engine 未接：text/tool/permission/retry/steer 场景显式留 B7，
> 两个测试文件头部各自写明。

> **B6 review 裁决（2026-08-19，Fable）**：① `EventMeta.coalescedFrom` **核准**——actor 赋 seq
> 与传输层合并只有靠 span 字段兼得 gapless 检测，规范已载、fixtures 已锁；`RequestId::Null`
> 是 JSON-RPC 合规修复，核准。② session/start 即建 transcript 文件与 TUI 的分歧：B8 统一为
> 单一行为（parity ledger 分类，不留前端分叉）。③ pipelined-close 拒绝语义核准（refusal
> 而非沉默）。④ **行数债务**：`controller.rs` 4214 行是战役自产——B7/B8 必须拆到 4000 线下；
> `chat.rs` 预期随 B7 拆 shim 自然缩减；B8 终审 discipline gate 须绿或由用户显式豁免。

### B7 · TUI 重接（L）
Chat/App 管线换 AppLink 帧，删全部 shim；TuiEvent 本地化；按键动作走同一 Action registry；
渲染层不动（写 once 不变量、statics、页引擎照旧）。
**验收**：四门绿 + TUI 域测试全绿 + **真机 smoke 清单**（main turn、agent 页直播、房间收发、
权限弹窗、queue/steer、/compact、resume 后房间恢复）——smoke 结果如实记录，未跑项列明。

> **B7 拆分（2026-08-19）**：实施中判定本批需拆两段，按任务书守则停下报告而非硬吞。
>
> **B7a · engine 上 wire（D148，已合 dev，五提交）**：`app/engine.rs` 一个单方法 trait +
> `engine/runner.rs` 一个实现；`conversation/submit` 的 `Turn`/`Shell`/`Deliver` 三路全部服务
> （核内做 item/turn/deposit/post，engine 做 model/shell/wake，B4 裁决②的线落成代码）；
> app-server 装配真 `Session` 并 attach；turn 结束时核内 drain 队列（仅在有 engine 时）；
> `EngineEvent::CommandTail` 给 `item/commandTailUpdated` 补上生产者；`warn_sink` 改走 `emit`。
> 黑盒补六场景（脚本 provider on loopback）。B5 裁决⑤核验完成：**TUI /permissions 确实展示
> D81 session 授权**（真机确认），故 `config/read` 已跟上（`sessionScoped: true`，不持久化）；
> 遗留 B8：控制台那行 "(.bingo/settings.json)" 表头对 session 授权是假话。
> `controller.rs` 4214 → 2812（拆 `resources.rs`/`tests.rs`/`run.rs`），线债已清。
>
> **B7b · TUI 换帧（待做）**：shim 清零（`Answer::now` 19 处生产调用点在同步按键/渲染路径、
> ~60 处 `watch::Receiver` 每帧拉取需先建本地投影、`UiEvent`→`AppEvent` 15 个语义臂、
> 56 处测试用 `conv.busy` 伪造运行中回合）、`chat.rs` 线债、三项需要 instance 的真机 smoke
> （agent 页直播、房间收发与 @、tool barrier steer）。理由与尺寸见 D148 末节。

> **B7b 落地记（2026-08-19）**：**store 已上线，换帧未做**（D149，五提交）。
> `src/tui/store.rs` 是客户端 reducer：attach → 快照切割 → 折叠 `AppEvent` 流；
> `pump()` 同步（每 tick，在 idle 门之前——核不等前端，不读就掉链路），
> `reconcile()` 异步（seq 空洞 → 重读快照替换本地态，`coalescedFrom` 按 span 判连续）。
> 真机确认已 attach（`BINGO_DEBUG` 报首次切割：4 会话/2 agent/1 房间）。
> 清账：`SubmitRequest.main_busy` **已删**（54 处 conv.busy 测试改为在核内起真 turn，断言未削弱）；
> 四个二次读参 handler **三个已收敛**（`provider.login` 按 B5 裁决③保留并改写注释），
> 顺手修出真 bug：`/team list` 因两个读者而印 usage 菜单而非组织图；
> 行数：`chat.rs` 4103→3783（拆 `chat_setup.rs`）、`agents.rs` 4048→2488（拆 `agents_tests.rs`），
> 全仓无文件超 4000。`rg "B7 removes this"` 6→2。测试 1686→1695，黑盒 27 不变。
> 三项真机 smoke **全部通过**（agent 页直播与历史、房间收发/@/resume 恢复、tool barrier steer），
> 另回归四项（主 turn/权限弹窗/队列/compact）。
>
> **拆批理由（新发现，非 D148 的尺寸论）**：读取面换帧被**测试运行时**卡住，不是被代码形状卡住。
> 控制台 ~640 个测试里 570 个是 `#[test]` 无 tokio 运行时，而 `AppCore::attach` 要
> `Handle::current()` 起转发任务 → 无运行时即无法 attach → store 为空视图。
> 只要有一个读取点搬上 store，这 570 个测试就读到空投影。故 **B7b-2 第一件事是裁决
> 「控制台测试如何拿到核」**：①全体改 `#[tokio::test]`；②让 `attach` 不需运行时
> （转发任务只为打 attachment 标签 + drop 时 Detach，可用带 `Drop` 的 `Requests` 包装替代，
> 代价是进程内失去有界请求队列，wire 侧自留即可）；③测试直接给 store 灌种子快照+事件。
> ②是唯一消除分叉而非绕开它的解，且改动很小——但它动核的公开接缝，所以是裁决不是实现细节。
> 裁决后再做：读取面 → 变更面（`AppRequest` + 等回执）→ 给控制台的核 attach engine
> （`tui_hooks`/`subagent_hooks` 随之删）→ `Answer::now()` 清零（余 15 处生产调用，全在变更面）。

> **B7a review 裁决（2026-08-19，Fable）**：① 拆批核准——止步报告优于硬吞，B7a 自含且 TUI
> 行为位相同。② 对开放问题的裁决：**B7b 照常进行，不需要重新规划**。frame-pull/push 的
> "错配"有标准解：TUI 本地 **store**（客户端 reducer——快照 + AppEvent 物化成本地投影，
> 渲染/按键同步读它，检测到 seq 空洞即重读快照 resync）。这不是逻辑复制——store 不含业务
> 规则，只是物化视图，GUI 客户端将来在 JS 里建的是同一个东西；这正是本协议的客户端形态。
> ③ 三个实现发现（输入 item 双落修复、`quiet` 是成帧契约、warn_sink 绕 actor 修复）核准；
> parity 核验结论核准（session 授权两侧都见，`(.bingo/settings.json)` 标头谎言转 B8）。
> ④ D 编号：B7b 取 **D149**；追加预留 **D150–D152**（B8 与溢出）。⑤ agents.rs 仍超 4000 线，
> B7b/B8 收敛或报告距离。

> **B7b review 裁决（2026-08-19，Fable）**：① 再次拆批核准（store 落地 + 三项团队 smoke 全过
> + main_busy 清零 + 全仓行数达标；读取面切换止步于真发现）。② 对三选项的裁决：**选 ②，
> `attach` 去 runtime 化**——`AppLink.requests` 改为 `Requests` 包装（send 时打标、Drop 时发
> Detach），不再派生转发任务。理由：唯一消除分歧而非绕开分歧的选项；与 B2b 的架构形状同构
> （actor 独立线程 + 无界收件箱正是为同步调用方而设，runtime 绑定的 attach 是实现意外不是
> 设计要求）；失去的进程内逐 attachment 有界请求队列可接受——wire 传输保有自己的入站上界
> （B6），进程内生产者与 B2b 裁决①同一信任域。核之公共接缝由此变更，本裁决即授权。
> ③ `/team list` 印用法菜单的 bug 修复核准；`TeamStart{members}`/`McpReconnect{server:None}`
> 两处"wire 能说、console 从没说过"的既有限界 → B8 parity ledger 分类。
> ④ store 的 `#![allow(dead_code)]` 随 B7b-2 读者落地摘除。

> **B7b-2 落地记（2026-08-19）**：**attach 去 runtime 化已落，换帧仍未做**（D150，六提交）。
> 裁决② 实施：`Requests` 包装（send 打 attachment 标、Drop 发 Detach）取代转发任务，`attach` 变纯写入
> （通道与编号在调用侧铸造），`AppLink::request` 同步；失去的进程内有界请求队列由 wire 侧
> `INBOUND_CAPACITY`（64，B6 未变）承担。「已结束的 session 拒绝 attach」改由 actor **先关收件箱再回 close**
> 保证。`Session` 携带 `AppCore`，四个 `Chat` 构造处（含六个 `test_chat()`）全部 attach ——
> **570 个无 runtime 测试自此读真投影**。
> 读取面搬走六处（prompt 三处 + agent/room 存在性三处）；顺手发现并修三件：
> ① 契约缺 `agent/removed`（`agent/changed` 只增不删，客户端永远删不掉一个实例；按 `task/removed` 形制补，
> schema 重生成 + fixture；**不加 `room/removed`**——本会话不删房间，无生产者的变体是空承诺）；
> ② `AgentResource.last_active_at` 恒为 `now`（`Idle for Ns` 永远读 0）；
> ③ 链路断开无人察觉（落后即失去 attachment，控制台会永远画旧投影）→ `reconcile_store` 重新 attach。
> 测试 1695→1700，黑盒 27 不变；四门 + discipline gate 全绿；真机 smoke 13 项全过（含团队三项与 /compact）。
>
> **未做与理由（新发现，非 D149 的运行时论）**：`.now()` 15 处、`tui_hooks`/`subagent_hooks`、配置双镜像
> 是**同一堵墙**——控制台的核没有 engine，故不发 `item/*`，故控制台只能从 `UiEvent` 渲染、只能自己跑
> run loop、只能自己写 `runtime.model_tx`。多数 `.now()` 根本没有对应的 `AppCommand`/`AppQuery`
> （`mail.due`、`drain_main_arrivals`、`drain_front`、开 turn），因为在 wire 上那些是**核**做的事。
> 接 engine 是一行；这一批是另一端：store 有意不投影 transcript，控制台每一行都由 `UiEvent` delta 建成。
> 详见 D150 末两节。另：roster 读取面搬不动，因 `AgentResource` 缺 `recentActivity`/`prompt`
> ——真实 parity 缺口，**建议 B8 ledger 裁决**，未擅自加字段。

> **B7b-2 review 裁决（2026-08-19，Fable）**：① `agent/removed` 契约增补**追认**——规范本就
> 要求 keyed upsert *与 removal*，`task/removed` 是先例，缺它 wire 客户端的 roster 永不收缩；
> 不加 `room/removed` 同样正确（无生产者的变体是服务端不守的承诺）。`last_active_at` 恒为
> now 的修复与断链重连是真缺陷修复，核准。② `AgentResource` 补 `recentActivity`/`prompt`
> **批准**，B7c 按加法流程落（roster 读取面与 GUI 画名册都需要）。③ 范围裁决：**B7c =
> 最后的身份互换**——控制台的核挂上 engine（提交由核开 turn、核驱动 engine），store 获得
> transcript/item 投影，控制台行渲染改从 store 取数。15 处 `.now()`、`tui_hooks`/
> `subagent_hooks`、配置双镜像随之消亡——它们是同一事实的影子，不是三件事。渲染不变量
> 红线不变：现有行断言测试是护栏。④ 真机发现的既有显示瑕疵（`!` 行经权限弹窗后工具行
> 留在 `⎿ Running…`）：若随渲染取数源切换自然消除则顺手收，否则入 B8 清单。D151 归 B7c。

> **B7c 落地记（2026-08-19）**：**读取面收官，写入面留 B7d**（D151，五提交）。
> `tui_hooks`/`subagent_hooks` 删除，`AgentRegistry` 的事件 sink 一并删；`src/tui/chat_feed.rs`
> 把 store 折进来的 `AppEvent` 译成控制台的渲染增量——**换的是取数源，不是渲染器**，
> 700 行 `Chat::route` 一字未动，全部行断言原样通过。关键发现纠正 D150 的判断：
> 控制台的 turn 从 B3 起就 `EngineHost::bound` 到核的 turn，agent turn 从 B4 起也是——
> **item 流一直在，只是没有读者**；"核没 engine 所以不发 item" 只对*核自己开的* turn 成立。
> store 获得 `Transcript`（log + live，`conversation/read` 打底、`item/completed` 追加、
> `turn/retrying` 按名撤回、generation 变化触发重读）与 `take_folded()`（状态 + 变更，
> 一个订阅者需要的那一对）。契约加法：`AgentResource.prompt`/`recentActivity`（裁决②，
> 两者进变更摘要），roster 三处读取面换 store；`buffer::refresh` 留在注册表并写明理由
> （DM 徽章走 `pair_lane` 走史，store 不投影那个）。三处形状被迫改正：吸收的 prompt 由
> 无主 `notice` 改为带 `from` 的 `peerMessage`（控制台第二个 walker 随之删除，B4 的"单走查器"落实）、
> `ItemBody::Interruption` 有了生产者、turn 级错误码变 `String`。
> 真机 smoke 18 项全过，并**修出一个只有真机能发现的缺陷**：`↪` steer 行原由控制台自己的
> steering 闭包发出，与核流竞态，真终端画出 `the tool ans`·`↪ …`·`wered; done.`；
> 改由 `queue/itemAbsorbed` 驱动后落回工具结果与回复之间。`!` 经权限弹窗后留 `⎿ Running…`
> 的瑕疵**在本批父提交 3924f9c 上逐键复现**，确认非回归，留 B8。
> 四门 + discipline 全绿；测试 1700→1703，黑盒 27 不变；`rg "B7 removes this"` **0**。
>
> **未做与理由（新墙，非 D150 的渲染论）**：`.now()` 15 处、配置双镜像、engine 挂接是**写入面**，
> 卡在三件事上：① `serve_submit` 会把斜杠命令交给核执行，而 `/status`/`/model`/`/help` 是控制台的
> *渲染*——控制台需要一个"核执行 Turn/Shell/Deliver、把 Command 交回"的 submit，
> 即 `Controller::submit` 从"只裁决"变成"裁决并执行"；② 15 处里有 10 处不是 run loop 而是
> **按键写入要回执**（invite/kick/stop/set_permission_mode/respond/reclaim_tail），消除它们要给
> 控制台一条 intent 队列（按键记意图、async 循环执行并在下一帧前折入），~130 处 `chat.submit()`
> 假定即时生效，是设计不是清账；③ digest 唤醒没有门（空 prose 被 `compose` 读成 `Empty`，
> 乙案的 debounce 尚未进 controller），images/memory/`LiveBash` 同理。
> 故建议 **B7d = 写入面**：`Controller::submit` 执行化 → 控制台 intent 队列 → 唤醒的门 →
> 末尾挂 engine；`.now()` 在那里归零，配置双镜像随之。

> **B7c review 裁决（2026-08-19，Fable）**：① 读取面切换全部核准——渲染器 700 行字节未动、
> 行断言原样通过是行为等价的正确证法；steer 行竞态修复（改从 `queue/itemAbsorbed` 画）核准，
> "两个生产者都对、只有钟错了"是投影架构才能修的 bug。② 两处可见微差核准并记录（流式中
> 工具参数 JSON 不计入 token 估算——权威值在终态到达；watch 行插入点 ±数字符——本就双生产者
> 竞态）。③ `absorb_inbound` 落 `peerMessage` 且 user 转发行标已读**追认**——与 B4"用户自己
> 的话定义为已读"同一原则。④ `standalone` 走 call-id 前缀（`bash-`）记为债务，
> `ItemBody::Command` 是正home → B8。⑤ 范围裁决：**B7d = 写入面**，按其建议顺序——
> `Controller::submit` 从"裁决"变"执行"（Command 交回前端渲染）→ 控制台意图队列
> （~130 提交点，回执在下帧前折叠）→ 乙案的门+图片+memory pass+LiveBash 另一半 →
> **engine 最后挂**。完成时 `.now()` 生产路径零、config 双镜像亡、answer.rs 阻塞半边亡。
> D152 归 B7d，B8 顺延 D153（预留扩至 D154）。

> **B7d 止步记（2026-08-19）**：**engine 侧对齐 + 房间加入归核已落，写入面未动**（D152，两提交）。
> 落地：`SessionEngine` 从 session 的 attachment 注册表解析图片标记（图片达 engine）、memory pass 与
> 空回复警告进入 run（turn 关闭之后）、`Run::Promote` 让 ctrl+b 有了地址（`command.promote` 从
> `Requires::ENGINE` 改为 `NOTHING`——它依赖的是"此刻有东西在跑"而非能力，空闲即 `noChange`）；
> D103 的"发言即入座"从 `tui::bufferview` 移入 `app/controller/run.rs`（wire 客户端向只旁观的房间
> 发帖曾被按名拒绝）。黑盒 provider 学会：非流式请求是场外调用，不计入脚本轮次。
> 测试 1703→1704，黑盒 27→28；四门 + discipline 全绿；`src/tui/` 零字节改动，故未重跑终端 smoke。
>
> **墙（结构性，非实现细节）**：裁决⑤的顺序无法成立，链条每一环都是被迫的——
> ① `Controller::submit` 执行 Turn/Shell/Deliver 必须 `self.engine()?`，故控制台的核**必须先挂 engine**，
> 不能最后挂；② 挂上 engine 即打开 `Controller::drain_main`（其门就是 `engine.is_none()`），
> 与控制台 `submit_queued` 成两个 drain 抢同一队列，故控制台的 drain 必须同步消亡；
> ③ 核的 drain 用 `serve_command_line` → `apply_action`，而 **`apply_action` 只实现 28 个 action 中的 14 个**：
> `conversation.compact`/`rewind`、`session.reset`/`rename`/`share`、`skill.invoke`、`provider.login`、
> `mcp.reconnect`、`team.*` 五个——全部 `Requires::ENGINE`，全部落到 `_ => unavailable()`，
> 实现都还在 `tui::chat::run_command` 里。故排队的 `/compact`（B7c smoke 第 11 项）会在挂上 engine 的
> 那一刻静默变成 `ActionUnavailable`。
> **写入面不是被 engine、意图队列或乙案的门卡住的，是被 B5 留在控制台的那半张动作表卡住的。**
> `Availability::engine_attached` 因此仍是常量 `false`（改真会让 `action/list` 承诺 `action/execute` 随即毁约），
> 只是现在写明它代表哪十三个。
>
> **三条出路，请裁决**：**(a)** 动作表的 engine 半边先入核（十三个 handler，按 B4 裁决②的分法：
> 账本归核、工作经 `Engine` 出去），独立成 B7d-2，写入面排其后——唯一不留分叉的解，且不小；
> **(b)** 在 SessionSetup 里说明"谁 drain"（一个 bool：transport 置真、控制台不置；控制台的 drain 搬进
> async 循环，`drain_front` 变 `.await`，`.now()` 归零仍成立）——作为陈述是诚实的，但在以消除分叉为
> 目的的战役末尾引入一处前端行为分叉；**(c)** 把 drain 出来的行发事件让各前端渲染——最小契约加法，
> 但不解决问题：核仍只能应用 14 个、拒绝 13 个，控制台得按 `spec_of(&action).requires.engine` 分支
> 决定要不要自己再应用一遍，一行两个权威。
> 建议 **(a)**。已探到的写入面成果（`Submitted` 处置、`serve_submit` 降为映射、`❯` 行改由
> `turn/started` 的 `input_item_ids` 画）已回滚，详见 D152 末节。

> **B7d review 裁决（2026-08-19，Fable）**：① 第四次止步核准（engine parity/说话即入场/
> 时序 bug 三件已落且回滚了不能变绿的勘察工作——工作区干净是止步纪律的正确形状）。
> ② 三条出路裁 **(a)**：动作表的 engine 半边入核（B7d-2，D153）——(b) 是在终点重新引入
> 前端分叉，(c) 留两个权威；(a) 是 B5 单表原则的补完。之后 B7d-3（D154）按已核准顺序
> 完成写入面。③ `Availability::engine_attached` 维持 false 直到 B7d-2 使之真实——
> "action/list 不许诺 action/execute 会拒绝的事"这条判断核准。④ wire 会话每 turn 新增
> memory pass 成本：核准（对齐即代价，控制台一直在付）；未来可做成 capability，不现在做。
> ⑤ `!session.quiet` 吞掉第一条空回应警告的 parity 泄漏 → B7d-2 修（走 emit）。
> ⑥ B8 顺延 D155（预留扩至 D156）。

> **B7d-2 落地记（2026-08-19）**：**动作表的 engine 半边已入核**（D153，六提交）。
> 十三个 handler 从 `tui::chat::run_command` 移入 `src/engine/actions.rs`——**一份实现两个渲染器**：
> 工作层返回 `Said{tier,text}`，控制台按 slash 档位渲染，核记为 `ItemBody::Notice` 并配一个 operation。
> 之所以不是"控制台改问核"，是控制台的核仍无 engine（挂上即开 `drain_main`，与 `submit_queued` 撞车，
> 那是 B7d-3 的整个问题），所以搬的是**工作层**而非调用层，格式串只剩一份。
> `apply_action` 的 `_ => unavailable()` 消失——编译器成为动作表完备性的守卫；
> `Availability::engine_attached` 变回 `self.engine.is_some()`；排队 `/compact` 经核 drain 正确执行（新测试）。
> `provider.login`：动作只带 provider 名，核跑浏览器流并把授权 URL 经 operation progress 冒出；
> `--manual <token>` 留在控制台，经**进程内函数实参**入库（不序列化即无帧可漏），有测试断言 action 不含 token；
> 远端客户端因此**不能**经本协议粘贴 token（device flow 或自写凭据文件）——这是边界，写明而非发现。
> 顺手修 parity 泄漏：`query.rs` 第一条空回应警告脱 `!session.quiet` 门。
> 测试 1704→1708，黑盒 29 不变；四门 + discipline 全绿；真机 smoke 16 项过、1 项未跑（详见 D153）。
> **已知缺口（B8 ledger）**：`RewindTarget::Item` 拒绝（checkpoint 身份是 transcript 行，item id 无对应记录，
> 猜即是 D135 要防的错目标损失）；控制台 rewind 仍是五答手势 vs wire 两模；rename 不动核的 `session.locator`（旧限）。

> **B7d-2 review 裁决（2026-08-19，Fable）**：① 13 动作入核全部核准；`engine/actions.rs`
> 单一 home 返回 `Said{tier,text}`、格式串只存在一次、编译器守表完整——正确形状。
> ② `provider.login` 凭据通道核准：`Flow::Manual` engine 本地枚举、读它的线程函数传参、
> 无序列化路径 + 断言测试；"远程客户端不能经本协议粘贴 token"作为边界如实声明。
> ③ `RewindTarget::Item` 拒绝核准（item id 无 transcript 行身份，猜测即 D135 要防的错靶）；
> rewind 的终端五答手势 vs wire 两模式收敛 → B8 parity 决定。④ smoke 发现的 shell 模式
> drain bug（`submit_queued` 把 /compact 按散文喂模型）随 B7d-3 的 drain 统一消亡——
> 又一个"核对了、控制台错了"的例证。⑤ rename 不动 locator：既有未引入，记录。

### B8 · 收尾（S）
`--print` 薄客户端；main.rs 启动统一；parity ledger 落成 CI 检查表（每个斜杠命令/提交分支/
AppEvent 变体分类 shared|frontend-local，新增未分类即红）；schema 标 experimental；
文档批：`guide.md`、README、`feedback-states.md`、规范文档终稿。
**验收**：四门绿 + ledger 测试绿 + Fable 全量终审。

---

## 5. 实施者守则（写进每个任务提示）

- 读本方案 + 规范文档（含 Amendments）+ `AGENTS.md`；行为疑问以决策记录（research.md D 号）为准，
  不确定就停下来在 D 记录里写明假设，不擅自定契约。
- 顺项目纹理：错误处理、测试命名、注释密度学周围代码；wire 文案英文、sanitized。
- 事件/快照永不含密钥；provider 状态只暴露 presence/source/status。
- 文本上限按字符（unicode scalar）计数（v1 既有惯例）；NDJSON 每行独立可解析。
- 不引新依赖除非方案列明（schemars 已在；JSON-RPC 手写勿引框架）。
- 提交：Conventional Commits，一批可多提交但逐文件 add；D 记录写"改了什么、验了什么、
  什么未验"。
