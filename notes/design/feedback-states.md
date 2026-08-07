# 反馈状态规范（Feedback States）

> 版本：v1.18 · 状态：正式生效（2026-08-07）
> 定位：bingo 所有用户可见反馈态的统一设计约定。GUI（TUI）与 CLI（headless）两侧同源，qa 验收锚点见文末。

## 总原则

1. **反馈态必须不依赖环境**：同一操作在 TUI、管道、CI、弱网窄屏下都要有可感知的反馈，且反馈信息不能只存在于交互式界面（见 CLI 侧约定）。
2. **状态机设计，不是一次性事件**：每个异步操作都走 `idle → loading → success / error → idle`，必须完整复位，禁止卡死在中间态。
3. **错误文案 = 发生了什么 + 用户能做什么**：禁止只报「操作失败」这种死路文案。
4. **为动而动不做**：反馈动效只用于引导注意或衔接状态变化，时间与幅度见各节约定。
5. **反馈以「动作」为粒度，不是以控件为粒度**：防重、加载、超时、复位都挂在提交动作上，而不只挂在按钮点击上（详见 Loading 节「防重复」、错误态节 3.1）。

## 状态机

```text
idle ──操作触发──▶ loading ──成功──▶ success ──┐
                     │                         │
                     ├──超时──────▶ error ──────┴──▶ idle（必须复位）
                     └────失败────▶ error
```

复位动作四项（qa 可逐条断言，见「验收锚点」）：

1. 清除 `aria-busy` / loading 态
2. 移除错误样式与 `aria-invalid`（不能残留红框）
3. 错误消息从 `aria-live` 区域移除（用**写空串/更新文案**，勿删节点——删节点多数浏览器不播报）
4. 焦点转移：成功 → 下一操作；失败 → 聚焦错误元素（字段级）或重试按钮（页面级）；**须在错误元素渲染完成后异步聚焦**（渲染后异步聚焦，如 requestAnimationFrame 或组件 effect，实现自定），否则聚焦失败

**陈旧响应竞态**：重试/新请求发起后，旧请求的迟到失败/成功响应必须被忽略（abort 或序号校验），禁止新成功被旧 error 闪一下。与复位一起测。

## 1. Loading（进行中态）

| 项 | 约定 |
|---|---|
| 触发阈值 | 异步操作 >200ms 未完成时出现 |
| 形态 | 局部操作：按钮内联 spinner，原位替换按钮图标位；页面级：内容区骨架屏 |
| 禁止 | 全屏遮罩阻塞；无反馈的静默等待 |
| 防重复 | 以**提交动作**为粒度防重：按钮 disabled 挡鼠标不挡 Enter 键 / 表单 onSubmit，提交动作粒度统一拦截；loading 期间同一动作不重复提交（幂等保证） |
| 文案 | 局部按钮不换文案（保持「提交」→ spinner 原位）；页面级给「正在加载…」 |
| 超时 | 分档超时：**读操作 10s、写操作 15s，适用于短同步操作**（list_models / count_tokens / complete_text 等），超时进对应错误级并提示可重试，**首要动作 = 重试**；**agent 长回合（流式 + 多轮工具）不套用 10s/15s**——回合中已有持续进度反馈（状态行 + 活动行 + `chat.busy`），超时由传输层（120s/60s）+ 用户中断兜底；**长回合若失败（传输层超时/中断）升级为全流程级错误**（可重试或返回，非静默局部提示）；**超时计时器必须在成功/失败/取消时同步取消**——否则成功后迟到的 error 会盖掉成功态。取消机制：反馈层到点 **drop future**（tokio `timeout()` 包裹，reqwest 连接随之取消），序号校验仅作兜底防御 |

## 2. Toast（轻提示）

| 项 | 约定 |
|---|---|
| 触发 | 操作结果不需要用户停留决策时（已保存、已复制） |
| 时长 | 默认 3s 自动消失；**hover 或键盘聚焦到 toast 均暂停计时，离开/失焦后续走剩余时间**（非重置）；可手动关闭 |
| 层级 | 同时不超过 2 条，**仅槽满时顶掉最旧**（A 剩 0.5s 且 B 进时，若未满则 B 排队等 A 走） |
| 去重 | 同类重复触发（连点 5 次「已复制」）→ **替换为同一条并重置计时**，不堆叠 |
| 文案 | 一句话说结果 + 必要时给动作入口（如「已复制 ✓ / 撤销」） |
| 可访问性 | 容器 `aria-live="polite"`，屏幕阅读器可读 |

## 3. 错误态（3 级递进 + 混合态）

| 级别 | 场景 | 形态 | 焦点 |
|---|---|---|---|
| 字段级 | 输入校验失败 | 内联于输入框下方：图标 + 具体原因（「邮箱格式不正确」）；红色描边仅标错误字段 | 聚焦对应输入框 |
| 页面级 | 单请求失败、依赖不可用 | 错误卡片/占位区 + 重试按钮 | 聚焦重试按钮 |
| 全流程级 | 接口挂、权限不足、会话失败 | 整页状态 + 返回路径（不能是死路） | 聚焦首要动作 |
| **混合态** | 批量操作部分成功部分失败 | 「成功 2/3，失败 1 项」+ 失败项列表，失败项可单独重试 | 聚焦失败项列表 |

### 3.1 错误码 → 用户动作映射（强制）

错误码契约除「怎么报」外，必须给出**用户动作**：同一错误码对应的 UI 动作必须一致。

| 错误码（示例） | 语义 | 用户动作 | 错误级别 |
|---|---|---|---|
| `AUTH_REQUIRED` / 401 | 登录过期 / 缺 key / key 非法 | 重新登录 / 配置 key | 全流程级 |
| `PERMISSION_DENIED` / 403 | 无权限 | 返回 / 申请权限 | 页面/全流程级 |
| `SERVER_ERROR` / 500 | 服务端错误 | 稍后重试 | 页面级 |
| `OFFLINE` | 无网络 | 检查网络后重试 | 页面级 |

映射表以 thiserror 错误类型为唯一来源，新增错误码时必须同时登记映射。

**错误级别 = 典型档位，上下文可覆盖**：表中「错误级别」列给出该码的**典型级别**；实际呈现级别由**生产者按触发上下文显式携带**（非渲染层推导、非测试侧复制映射）——如 `TIMEOUT` 短同步=页面级 / 长回合=全流程级、`PERMISSION_DENIED` 多档码取全流程档时整屏呈现。呈现层与断言以事件携带的 `level` 为准。

通用：
- 错误文案必须说明「发生了什么 + 用户能做什么」，文案可改，但承载信息不变。
- 错误码在界面上作为「高级详情」折叠呈现：普通用户默认只见人话文案，展开见 `code=...`。
- 字段级错误聚焦后，重试/修改成功必须回到 idle（见状态机复位）。

## 4. CLI 侧约定（headless / 管道 / CI）

### 4.1 结构化错误协议

- 错误以稳定错误码/分类定义（如 `CONFIG_INVALID`、`PERMISSION_DENIED`），代码中的 thiserror 错误类型即唯一来源；渲染层只消费不复刻。
- UI 渲染、CLI 输出、日志、qa 断言共用同一份错误定义：
  - 断言靠错误码，不靠字符串匹配文案；文案改版不影响测试。
- **稳定输出契约**（非 TTY 一律此格式，可 grep）：

```text
[error] code=CONFIG_INVALID msg=配置文件无法解析: line 3
```

  `key=value` 单行；`code` 为稳定错误码，`msg` 为人话文案（可改）。qa 断言只依赖 `code`。
- **msg 转义约定**（防破坏 key=value 解析，qa/日志 grep 稳定）：
  - 换行/制表符归一化为空格；
  - 主 `msg` 截断 200 字符（超出截断，可预期）；
  - 需多行堆栈时另加 `detail=` 字段承载，主 `msg` 保持单行。
- **错误码命名规范**：一律 `SCREAMING_SNAKE`（如 `CONFIG_INVALID`），新码照此登记。

### 4.2 TTY / 非 TTY 降级

- 交互式 TTY：可展示 spinner / 进度条。
- 管道 / CI / 脚本化调用：无 spinner；进度与错误必须落到**可 grep 的单行日志**（`[error] code=...`、`[progress] ...`），不能只在交互界面可见。
- 同一操作两种环境反馈等价（信息不丢失，只是呈现形式不同）。

### 4.3 错误码实现路径（已定：C 出口映射，dev 拍板）

现状：bingo 的 thiserror 错误为模块级分散定义（src/api/client.rs、settings.rs、tasks.rs、mcp.rs 等 10+ 处），Display 文案即唯一错误信息，尚无统一错误码层。落地路径三选一（选定 C）：

- **A. 顶层 AppError 聚合**：统一 enum 收编各模块错误，码表集中——契约最清晰，改动面大。
- **B. 模块级加 `code()`**：现有 enum 暴露 ErrorCode + 集中码表——保留模块隔离，改动小，需维护跨模块码表。
- **C. 出口映射（已选定）**：底层错误保持现状，仅 CLI/UI 出口做一次「错误 → 稳定错误码」映射——侵入最小、符合 AGENTS.md「默认做减法」；需求成熟后可演进到 A。

**C 的落地护栏**（devex 把关 + qa 验收侧认领，缺一则契约漂移）：

1. **防 downcast 脆弱性**：禁止靠**按类型名/字符串匹配**的 downcast（重构改名会静默断掉）。C 的落法 = **每个模块错误实现轻量 `ErrorCode` trait（返回 `&'static str` 码），出口只调用不判断类型**——编译期穷尽、可发现、不脆（B 的接口 + C 的侵入面）。boxed 出口用 **`downcast_ref::<$t>()` 编译期类型引用**（非按名匹配，重构直接报错），属于允许形态。
2. **登记即契约**：硬规则——新增错误路径必须为每个 variant **显式返回稳定码**；暂未分配稳定码的 **显式返回 `GENERIC`**（debug 构建告警，**禁止 `_` 隐式兜底**）。`GENERIC` 为**已发布稳定码**（不是临时值），qa 断言显式覆盖「显式 `GENERIC` 路径 → 落 `GENERIC`」。码表集中一个文件，即 dev/qa 的单一查找点。
3. **码值只增不改不重用（semver 式）**：错误码一经发布，语义不可变更、编号不可重用，否则历史日志与既有断言全作废。码表单文件集中 + 新增即追加，review 一眼可见；文件头注释写明 semver 规则。
4. **单出口函数 + 单码表文件**（结构强制一致）：映射逻辑单一来源 = 各模块 `ErrorCode` impl；**TUI 出口走 `map_error`，CLI boxed 出口走 `error_code_boxed` + 宏登记表**——两个函数都只消费 ErrorCode impl、不各自实现映射，一致性由此保证，双出口对照断言作为兜底保留。**宏登记表是「登记即契约」第二处**：新增实现 ErrorCode 的类型须同步登记，否则 CLI 出口静默落 `GENERIC`（TUI 出口正常）——需加测试断言宏登记表覆盖所有 ErrorCode 实现类型。码表集中在同一文件。
5. **防漂移单测**：**枚举每个模块错误 enum 的每一个 variant**，断言映射到**非 `GENERIC`** 的稳定码——从「防漏登记」升级为「CI 挡临时 GENERIC 漏登记」，改文案不炸、删码必炸。单测本身需随新增 variant 维护，接受这份成本（它是编译期之外 CI 期的兜底）。
   - **GENERIC allowlist**（qa 建议 + dev 定稿）：`src/error.rs` 里 `const GENERIC_ALLOWLIST: &[&str]`，条目用可定位路径（如 `"tool::bash::Error::NonZeroExit"`）；单测断言「不在 allowlist 的 variant 一律非 GENERIC」。
   - **临时标注**：每个 allowlist 条目必须带 `TODO(generic-allow): <issue>/<日期> <理由>` 注释，防永久豁免。
   - **review 约定**：新增 allowlist 条目必须有理由（为何暂不登记稳定码），无理由不允许。

**落地拍板（dev）**：契约集中在 **`src/error.rs`** 单文件，内含三样：
1. `ErrorCode` trait：`fn error_code(&self) -> &'static str`（SCREAMING_SNAKE）
2. `GENERIC` 显式返回 + debug 告警 + 共用 `map_error` 出口函数
3. 防漂移单测（枚举每个模块错误 enum 的**每一个 variant**，非 allowlist 断言非 `GENERIC`；含「显式 `GENERIC` 路径断言」与双出口对照，与 qa AC 表互为镜像）

**实现接入点（dev 对照代码确认，非新造）**：
- `src/ui.rs` 是 renderer-agnostic 契约层（TUI/GUI/测试 harness 共用 `UiEvent`）——「单出口 map_error」的天然挂载点已存在。
- `UiEvent::Error` 目前为纯字符串，全项目仅 3 处（chat.rs 1439 消费、2748/2789 生产 `e.to_string()`）——升级为 `UiEvent::Error { code, msg }` 改动面极小，是接入错误码契约的理想切入口；TUI 渲染错误行时天然带稳定码，qa 可断言。
- TUI 状态机复位已具雏形：`chat.busy` 是事实上的回合状态机，chat.rs 已有大量 busy 态纯逻辑测试（无 runtime），qa 断言复用该模式。

**ErrorCode trait 实现模型（编译强制 + GENERIC 显式化，dev 修正版）**：
- 各模块在自家 enum 上实现 trait，match **穷尽所有 variant、无 `_` 兜底臂** 返回稳定码——新增 variant 未处理 → **编译直接报错**，「编译期穷尽」保证成立
- 「暂未登记」的 variant 由开发者**显式返回 `GENERIC`**（显式行为，而非 `_` 隐式静默落入）；显式 `GENERIC` 时 debug 构建下 `eprintln!` 告警，提醒补登记
- **release 下显式 `GENERIC` 语义已知**：代表「此路径暂未分配稳定码」（错误语义降级为通用），不是意外丢失——已知权衡，如实记录，不承诺编译期兜住
- **当前显式 `GENERIC` 路径 = 0**：`missing_code` 告警机制休眠；未来新增显式 `GENERIC` 返回时（含 **boxed 出口宏登记表漏登记落入 `GENERIC`** 的分支）**必须调用 `missing_code` 告警**，防静默——debug 下 `eprintln!` 醒目告警、release 下语义已知；不能只在文档标注休眠而代码无告警尾巴
- 出口只调 `error_code()`，不判断具体类型

本规范只约束**出口契约**（CLI/UI 输出稳定错误码），内部路径已定为 C；出口输出一致。TTY 检测用 `std::io::IsTerminal`（零新依赖）。

### 4.4 场景 → 错误码表

dev 实现与 qa 断言统一对照此表（**登记即契约**；实现后全 variant 已显式分配
稳定码，`GENERIC_ALLOWLIST` 为空）：

| 场景 | 错误码 | 用户动作 |
|---|---|---|
| 读/写超时（短同步操作）；长回合传输层超时 | `TIMEOUT` | 稍后重试（短同步=页面级；长回合=全流程级，AC-53） |
| 登录过期 / 缺 key / key 非法 / 401 | `AUTH_REQUIRED` | 重新登录 / 配置 key |
| 无权限 / 403 | `PERMISSION_DENIED` | 返回 / 申请权限 |
| 限流 / 429 | `RATE_LIMITED` | 稍后重试 |
| 服务端错误 / 流协议 / MCP 连接失败 | `SERVER_ERROR` | 稍后重试 |
| 无网络（传输层错误） | `OFFLINE` | 检查网络后重试 |
| 配置非法（settings / team.json 读写或校验） | `CONFIG_INVALID` | 修正配置后重试 |
| 本地存储读写失败（tasks / transcript / experience） | `STORAGE_ERROR` | 检查磁盘 / 权限后重试 |
| 工具执行失败 | `TOOL_FAILED` | 查看工具输出后重试 |
| hook 执行失败 | `HOOK_FAILED` | 检查 hook 配置 |
| 显式 `GENERIC` 路径 | `GENERIC`（暂未分配稳定码） | 按文案指引 |

码值 semver：一经发布只增不改不重用；新增码 = 新增 variant 映射 + 在
`src/error.rs` 防漂移单测补断言（缺一环 CI 红）。

**短同步操作失败 = 降级可见，不静默吞错（#18 落地后口径）**：
- 短同步操作（`list_models` / `count_tokens`）失败时，TUI 发射页面级 `UiEvent::Error`（`level: Page, context: ShortSync`）→ 内容区末尾错误行 error 色高亮（用户可感知「发生了什么 + 能做什么」）。
- **行为降级保留**：/model 菜单仍显示已知模型（列表为空可用）、/status 预算仍显示 0——错误行是提示，不阻断交互、不升级整屏态。
- 对照：静默吞错（`unwrap_or_default()` / `unwrap_or(0)` 无感知）违反总原则 1「反馈态必须不依赖环境」，已修正为「降级 + 可见」。

## 5. DOM / 样式 / ARIA 约定

> 本节为 Web 前端（DOM/ARIA）口径；**bingo 当前技术栈为 ratatui TUI + headless CLI，无 DOM/aria/CSS 动效/prefers-reduced-motion/rAF**。规范值（状态、时序、复位）不变，TUI 侧按下方映射实现。

| 规范项（Web） | bingo TUI 映射 |
|---|---|
| `aria-busy` / loading | `chat.busy` + 状态行（已有） |
| `aria-invalid` 红框 | 错误行高亮样式 |
| `aria-live` 写空串更新 | 状态区内容更新（非删行） |
| 焦点转移（渲染后异步聚焦） | 错误行渲染后滚动到可见区 + 高亮 |
| `prefers-reduced-motion` | TUI 无此概念：spinner 动画频率可降级，指示不删 |
| requestAnimationFrame | TUI 帧循环天然在渲染后，无此问题 |
| 动效时长（150ms/120ms/100ms） | TUI 不适用；以帧数表达（如 1-2 帧淡入），不做夸张位移 |

**TUI 侧补充（#18 落地）**：错误**级别/上下文由生产者显式携带**（`UiEvent::Error { code, msg, level, context }`，chat.rs 发射时已知触发路径）、渲染层只消费不推导——级别非码的固有属性（`TIMEOUT` 双档、`PERMISSION_DENIED` 双档），渲染层与测试共用同一事件契约，禁止在渲染层/测试侧复制「码→级别」映射。

Web 侧约定（供未来 Web 前端复用）：

- **loading 按钮**：`aria-busy="true"` + disabled；spinner 用 `<span role="status">`。
- **toast**：容器 `aria-live="polite"`；含动作入口时 `role="status"`，勿用 `role="alert"`（非阻塞提醒）。
- **字段级错误**：错误文案挂 `role="alert"` 或 `aria-describedby` 关联输入框；输入框 `aria-invalid="true"`。
- **页面/全流程级错误**：`role="alert"`，重试按钮可达且聚焦。
- **错误码高级详情**：折叠区默认隐藏，展开按钮语义化（`aria-expanded`）。
- **焦点时序**：复位第 4 条聚焦必须在错误元素渲染完成后进行（渲染后异步聚焦），失败则跳过而非阻塞。
- **动效**：spinner 循环动画；toast 进入淡入 + 微上移（150ms，cubic-bezier 缓出），退出淡出（120ms）；错误块出现 100ms 内完成，不做夸张位移。时长均短、可被 `prefers-reduced-motion` 关闭。
- **reduced-motion 边界**：`prefers-reduced-motion` 只关动效（骨架屏 shimmer 可关），**loading 指示本身不能移除**——慢加载感知是状态不是装饰。

## 6. 可测试性约定

- **时序断言**：200ms / 3s 这类时序用 **fake timers / 虚拟时间**（ms 级确定性），E2E 只留冒烟，不做时序断言（避免 flaky）。Rust 侧用 `tokio::time::pause/advance`（tokio 已含 time 特性，零新依赖）；Web 侧用组件级 fake timers。
- **测试钩子**：组件/命令需提供可注入钩子——可注入延迟（触发 loading 态）、可注入失败响应（触发各错误级），便于各态稳定复现。
- 断言一律以错误码为准（`[error] code=...`），不匹配文案。

## 7. 给 qa 的验收锚点

- **Loading**：200ms 阈值触发/隐藏正确；局部按钮原位 spinner；禁止全屏遮罩；**动作粒度防重（含 Enter/表单 onSubmit）**。
- **Toast**：3s 自动消失、可关闭；hover/键盘聚焦暂停并续走剩余时间；最多 2 条、仅满时顶最旧；同类去重替换+重置计时。
- **错误态**：三级 + 混合态分别触发正确；401/403/500/offline 映射动作正确；字段级聚焦对应输入框；文案含「发生了什么 + 能做什么」；重试后状态复位。
- **超时**：读 10s / 写 15s（短同步操作）超时进对应错误级；长回合走传输层（120s/60s）+ 用户中断，失败升级全流程级；**成功后迟到 error 被取消**；超时计时器在成功/失败时取消。
- **状态复位**：四项复位逐一断言（aria-busy、aria-invalid、aria-live 内容、焦点转移）；聚焦发生在渲染完成后；陈旧响应竞态被忽略。
- **结构化错误**：非 TTY 输出为 `[error] code=... msg=...` 单行契约；断言用错误码不用文案。
- **可访问性**：toast `aria-live` / 错误 `role="alert"` 可被屏幕阅读器读取；`prefers-reduced-motion` 下动效关闭但 loading 指示保留。

## 变更记录

- v0.1（2026-08-07）：草案，含 Loading/Toast/错误态三级与验收锚点。
- v1.0（2026-08-07）：并入 devex 三条——结构化错误协议（4.1）、TTY/非 TTY 降级（4.2）、状态机复位四条（状态机节）；补 GUI 侧反哺：错误码折叠呈现、反馈态不依赖环境总原则。
- v1.1（2026-08-07）：并入 main 实现侧两条——焦点渲染后异步聚焦（状态机复位第 4 条 / 第 5 节「焦点时序」）、CLI 错误码契约格式 `[error] code=... msg=...`（4.1）；并入 qa 边界六类——分档超时与计时器取消（Loading 节「超时」）、错误码→用户动作映射（3.1）、混合态（3）、动作粒度防重（Loading 节「防重复」）、陈旧响应竞态（状态机节）、Toast 量化（2）；可测试性约定（6）；reduced-motion 下 loading 指示保留、aria-live 写空串不删节点。
- v1.2（2026-08-07）：并入 devex 的 msg 转义约定（换行归一化空格、主 msg 截断 200 字符、多行堆栈走 `detail=`，见 4.1）；并入 dev 现状发现——错误码实现路径 A/B/C 待 main 拍板（4.3），本规范只约束出口契约不依赖内部路径。
- v1.3（2026-08-07）：并入 devex 的 C 路径三护栏——防 downcast（模块错误实现 `ErrorCode` trait 返回 `&'static str`，出口只调用不判断类型）、登记即契约（未登记走 `GENERIC` 兜底 + debug 构建告警）、防漂移单测（4.3）；错误码命名规范 `SCREAMING_SNAKE`（4.1）；新增 4.4「场景 → 错误码」示例表供 dev/qa 对照。
- v1.4（2026-08-07）：并入 qa/devex 三条——兜底码定为已发布稳定码 `GENERIC` 且未登记落兜底可断言（护栏 2）；码值只增不改不重用 semver 规则（护栏 3）；双出口一致性结构强制：GUI/CLI 共用同一 `map_error` 函数、单码表文件，断言作兜底（护栏 4）。
- v1.5（2026-08-07）：dev 拍板——路径定为 C 出口映射（4.3）；契约集中 `src/error.rs` 单文件（ErrorCode trait / GENERIC 兜底 + debug 告警宏 + map_error / 防漂移单测）；ErrorCode trait 实现模型：各模块 enum 穷尽 match 返回稳定码（新增 variant 未处理编译报错）+ 尾部 `_ => missing_code + GENERIC` debug 告警。
- v1.6（2026-08-07）：qa 澄清「穷尽 match 与 `_` 兜底臂矛盾」，dev 修正实现模型——**去掉 `_` 兜底臂、真穷尽 match 编译强制**（新增 variant 未处理编译报错）；`GENERIC` 改为**显式返回**（显式行为 + debug 告警），release 下语义已知「暂未分配稳定码」如实记录；防漂移单测改为**枚举每模块每 variant 断言非 GENERIC**（CI 期挡漏登记）。
- v1.7（2026-08-07）：并入 qa 提出、dev 定稿的 **GENERIC allowlist 落地细节**（护栏 5）——`src/error.rs` 里 `const GENERIC_ALLOWLIST: &[&str]` 用可定位路径（如 `"tool::bash::Error::NonZeroExit"`）；单测断言「不在 allowlist 的 variant 一律非 GENERIC」；条目必带 `TODO(generic-allow): <issue>/<日期> <理由>` 注释；review 约定：新增 allowlist 条目必须有理由，无理由不允许。
- v1.8（2026-08-07）：dev 评审落地——第 5 节标注「Web DOM/ARIA 口径」，补 **bingo TUI 映射表**（chat.busy→aria-busy、错误行高亮→红框、状态区更新→aria-live 写空串、错误行滚动可见+高亮→焦点转移、spinner 降频→reduced-motion 等，规范值不变）；第 6 节补 Rust 侧时序测试用 `tokio::time::pause/advance`（零新依赖）；4.3 补实现接入点：`src/ui.rs` 即 renderer-agnostic 契约层（map_error 天然挂载点）、`UiEvent::Error(String)` 仅 3 处改造为 `{ code, msg }` 是理想切入口、`chat.busy` 已是回合状态机先例。
- v1.9（2026-08-07）：DX 评审修正（devex）——4.3「落地拍板」修正 v1.5 旧口径残留（「`GENERIC` 兜底」→「`GENERIC` 显式返回 + debug 告警」、「各模块代表错误变体」→「枚举每模块每 variant」），消除与护栏 5 / v1.6 修正版的自相矛盾；v1.7 变更记录归属修正（GENERIC allowlist 为 qa 提出、dev 定稿）；文档进入引用网络——`notes/research.md`「参考」节加链接、`AGENTS.md`「内置技能同步」节补「改动涉及用户可见反馈态须对照本文件」规则。
- v1.10（2026-08-07）：dev 复评收尾——修正护栏 2「登记即契约」与 4.4 示例表的**同源自动兜底残留**（「未登记走 `GENERIC` 兜底」/「未登记路径 → 落 `GENERIC`」→「每个 variant 显式返回稳定码，暂未分配的显式返回 `GENERIC`，禁止 `_` 隐式兜底」/「显式 `GENERIC` 路径 → 落 `GENERIC`」），全篇口径与 v1.6 模型完全自洽。
- v1.11（2026-08-07）：dev 定夺 AC-15 超时分层——§1「超时」行与 §7 锚点补**按操作类型细分**：短同步操作（list_models/count_tokens/complete_text 等）套反馈层读 10s/写 15s、超时首要动作重试；**agent 长回合不套用 10s/15s**（持续进度反馈已有，走传输层 120s/60s + 用户中断），**长回合失败升级全流程级错误**；取消机制 = 反馈层到点 drop future（tokio `timeout()`），序号校验仅兜底。
- v1.12（2026-08-07）：dev 实现期回填——§4.4 场景表从「示例」升级为**登记即契约的完整码表**：新增 `RATE_LIMITED`（429 限流）、`STORAGE_ERROR`（本地存储）、`TOOL_FAILED`、`HOOK_FAILED`，`AUTH_REQUIRED` 语义扩展为「登录过期/缺 key/key 非法/401」，`SERVER_ERROR` 覆盖流协议与 MCP 连接失败；实现落地点：10 个模块错误 enum 全部实现 `ErrorCode`（match 穷尽无 `_` 臂）、`UiEvent::Error` 结构化（`{ code, msg }`）、CLI 顶层出口 `[error] code=... msg=...`（非 TTY）、反馈层超时分档（读 10s/写 15s）落地。码值只增不改不重用。
- v1.13（2026-08-07）：devex 实现后复评回填——护栏 1 澄清 downcast 形态：禁止**按类型名/字符串匹配**，允许 `downcast_ref::<$t>()` **编译期类型引用**（重构报错）；护栏 4 补**双出口实现口径**：TUI 走 `map_error`、CLI boxed 走 `error_code_boxed` + 宏登记表（登记即契约第二处，加登记表全覆盖测试），映射逻辑仍单一来源（ErrorCode impl）保证一致；实现模型注明**当前显式 GENERIC 路径 = 0**（missing_code 休眠，未来新增须调用）。
- v1.14（2026-08-07）：qa 回归实证补充——实现模型强化 `missing_code` 告警责任：未来新增显式 `GENERIC` 返回时（含 **boxed 出口宏登记表漏登记落入 `GENERIC`** 的分支）必须调用 `missing_code` 告警（debug 醒目 / release 语义已知），不能只在文档标注休眠而无代码告警尾巴。
- v1.15（2026-08-07）：qa #69 契约对齐 + main 拍板——§3.1 示例表 `AUTH_EXPIRED` 改为 **`AUTH_REQUIRED`**（单一来源对齐 §4.4/实现，不新增码；登录过期/缺 key/key 非法/401 语义由 msg + 用户动作承载）；§4.4 `TIMEOUT` 行补**双呈现级别注记**（短同步=页面级；长回合=全流程级，AC-53——呈现级别由触发上下文决定，不单由 code 推断）。
- v1.16（2026-08-07）：#18 呈现层最小实现落地回填（dev #86 + #92）——`UiEvent::Error` 扩展为 `{ code, msg, level, context }`（级别/上下文由生产者发射时显式携带，chat.rs 回合级落 Full+LongTurn）；§5 补 TUI 侧注「级别由生产者携带、渲染层只消费不推导，禁止渲染层/测试侧复制码→级别映射」；§3.1 补「错误级别 = 典型档位，上下文可覆盖」注（TIMEOUT 双档、PERMISSION_DENIED 双档取全流程档）。呈现层按 `last_error`（chat.rs）驱动：Full=整屏态（标题+码+说明+动作提示，Enter 重试/Esc 返回/Ctrl+C 退出）、Page/Field=错误行 error 色高亮。**#92 短操作降级可见**：§4.4 补「短同步操作失败 = 降级可见，不静默吞错」口径——list_models/count_tokens 失败发 Page+ShortSync 错误行（行为降级保留：菜单空/预算 0 仍可用），TurnStart 复位错误态；生产 `ErrorLevel::Page`/`ErrorContext::ShortSync` 从 dead_code 转为真实发射源。
- v1.17（2026-08-07）：todo 任务区完成态收口（ui/ux 方案）——任务区区分**自动打开**（TaskCreate 信号，`tasks_auto`）与**手动打开**（Ctrl+T）；自动打开的面板全部任务 Completed 即自动隐藏（refresh_tasks 收口），并复用 §2 瞬态行机制推 `✓ N/N tasks 完成 · ctrl+t 查看`（2s TTL 不落盘）给闭合感与找回路径；手动打开的面板全部完成保留（用户显式要看的态，不推瞬态行）；`/tasks` 显式请求临时放行不受影响（不误报「没有后台任务」）。
- v1.18（2026-08-07）：AskUserQuestion 回答反馈块生命周期收口（main 现场报障）——回答结果块（`⏺ User answered the questions:`）从**常驻到 /clear** 改为**回合内瞬态**：TurnEnd 清除（含 `flushed_ask_rows` 游标归零，避免下次块跳过渲染）；回答过程与回合中保留（多问题中间态可见、答案回显），回合结束即消失——块渲染在文档尾部/输入框上方、不参与消息流，常驻会像残留物；与 §2 瞬态行「完成反馈不常驻」同一精神（区别：块在回合内全程可见，不走 TTL）。答案内容本就经工具回填给模型，无需 UI 常驻。
