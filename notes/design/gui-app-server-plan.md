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

### B4 · 协作域（L）
agent/room conversations；walk/LineSource 提升（决定：单走查器）；房间消息 = message item；
mail digest 入核（乙案）；delivery/ack 结构化状态（D137 语义：同事散文不结算 ack）；
attention（markRead/unread/mention obligations）；**房间+attention 持久化 sidecar + resume 重放**
（决定 3，含 golden 测试）；team 启动为 operation；agent 资源形状（决定 6）。
**验收**：四门绿 + 协作场景测试（DM 三声部 marker、@债务、serial bounce、resume 恢复房间）。

### B5 · 动作注册表与目录（M）
`Action` 枚举 + 单 registry（`action/list` 元数据与分派同源 + 完备性测试，终结斜杠三表漂移）；
斜杠家族逐一迁出 Chat/team_cmd；`catalog/read`（session 前可用，决定 7）/`config/read`/
`resource/read` 分页；`asset/registerPath`/`readChunk`。
**验收**：四门绿 + registry 完备性测试 + 每 Action 至少一条行为测试。

### B6 · app-server 传输（M）
stdio JSON-RPC 循环（framing、initialize 协商、有界队列、delta 合并、慢客户端关闭策略）；
黑盒场景套件（真进程 + fake provider，覆盖规范 §Black-box 列表；沿用 v1 黑盒的
隔离 HOME/stdout 纯净形制）。
**验收**：四门绿 + 黑盒全绿 + stderr/stdout 分离断言。

### B7 · TUI 重接（L）
Chat/App 管线换 AppLink 帧，删全部 shim；TuiEvent 本地化；按键动作走同一 Action registry；
渲染层不动（写 once 不变量、statics、页引擎照旧）。
**验收**：四门绿 + TUI 域测试全绿 + **真机 smoke 清单**（main turn、agent 页直播、房间收发、
权限弹窗、queue/steer、/compact、resume 后房间恢复）——smoke 结果如实记录，未跑项列明。

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
