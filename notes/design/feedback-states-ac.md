# 反馈状态规范 · 验收断言表（AC）

> 版本：v1.9（qa 交付 + devex 契约对齐）· 对应设计文档：`notes/design/feedback-states.md` **v1.15**（§3.1/§4.4 契约对齐落定）
> 用途：dev 按 AC 表实现，qa 按 AC 表回归。**断言一律锚错误码（`[error] code=...`），永不断言 msg 文案**。
> 优先级：P0 = 发布门禁必须过；P1 = 应过，可排期后补。
> 断言方法：单元（fake timers / tokio `pause`+`advance`，ms 级确定性）｜集成（spawn 真实 CLI，非 TTY）｜组件/E2E（TUI，仅冒烟不断言时序）｜SR 审计（手动读屏，仅 Web 前端适用）。

## dev 实现核对（2026-08-07，对应文档 v1.12）

以下断言已由 dev 实现落地，qa 回归时可直接验证（断言锚点不变）：

- **AC-30/31/32**：CLI 顶层出口已接线——`main` 捕获顶层 `Box<dyn Error>`，非 TTY（`stderr().is_terminal()` 判否）输出 `[error] code=<SCREAMING_SNAKE> msg=<单行 ≤200>`；msg 经 `src/error.rs::sanitize_msg` 归一化换行/制表符 + 截断 200 字符。实测 `[error] code=AUTH_REQUIRED msg=missing API key...`。
- **AC-36 双出口一致性**：GUI（TUI `UiEvent::Error { code, msg }`）与 CLI（`report_error` → `error_code_boxed`）共用 `src/error.rs` 的码表；TUI 生产端经 `map_error`，CLI 经 `error_code_boxed`（cause 链 downcast 到具体类型再取码），两侧同源。
- **AC-38/40/41/43/44**：10 个模块错误 enum 全部实现 `ErrorCode`（match 穷尽无 `_` 臂，新增 variant 编译报错）；防漂移单测枚举每模块每 variant 断言非 `GENERIC`；`GENERIC_ALLOWLIST` 当前为空（全 variant 已登记稳定码）；契约集中于 `src/error.rs`。
- **AC-45**：`TIMEOUT`/`AUTH_REQUIRED`/`PERMISSION_DENIED`/`SERVER_ERROR`/`OFFLINE`/`CONFIG_INVALID` 全部按 §4.4 实现；新增登记 `RATE_LIMITED`（429）、`STORAGE_ERROR`、`TOOL_FAILED`、`HOOK_FAILED`（见文档 v1.12 §4.4）。
- **AC-12/13/14/53/54**：反馈层超时分档落地——`SHORT_READ_TIMEOUT=10s`（list_models/count_tokens）、`SHORT_WRITE_TIMEOUT=15s`（complete_text，包裹整个含重试操作）、长回合 stream 保持传输层 120s/60s；反馈层 `tokio::time::timeout` 到点 drop future 即取消底层请求。client.rs 有 `feedback_timeout_tiers_are_read_10s_write_15s` 常量断言。
- **AC-52**：`UiEvent::Error { code, msg }` 结构化完成（原 `Error(String)`），chat.rs 消费端渲染 `[error] code=... msg=...`，生产端经 `map_error` 取码——非 `to_string()` 拼接。
- **AC-50**：时序测试用 `tokio::time::pause/advance` 的基建沿用 chat.rs 现有无 runtime 纯逻辑测试模式；具体 fake-timers 用例由 qa 按本表补充。
- **AC-33 暂不适用**：`detail=` 输出通道（JSON 转义多行堆栈）**仅 `--verbose` 触发**，当前无 `--verbose` 即无触发点，故暂不实现；`sanitize_msg` 已保证主 msg 单行（AC-31/32 已测），码表侧无缺口。**实现 `--verbose` 时补本断言**（msg 保持单行 + `detail=<JSON 转义>`，详见 §F）。

待办（dev 侧不覆盖）：AC-15 重试幂等、AC-53 长回合失败升级 TUI 呈现、AC-26 全流程级错误 TUI 整屏态等**组件/TUI 呈现层**断言，属 qa 回归范围。

## TUI 映射基准（dev 第 21 条评审，已由 ui/ux 并入文档第 5 节）

bingo 技术栈为 **ratatui TUI + headless CLI**，无 DOM / aria-* / CSS 动效 / prefers-reduced-motion / rAF。本表 Web 侧术语按下表映射，**断言以 TUI 可观测行为为准**：

| 规范项（Web） | bingo TUI 映射 |
|---|---|
| aria-busy / loading | `chat.busy` + 状态行（已有） |
| aria-invalid 红框 | 错误行高亮样式 |
| aria-live 写空串（非删节点） | 状态区内容更新（非删行） |
| 焦点转移（渲染后异步） | 错误行渲染后**滚动到可见区 + 高亮**；TUI 帧循环天然在渲染后 |
| prefers-reduced-motion | TUI 无此概念：spinner 动画频率可简单降级，**指示不删** |
| role="alert" / aria-describedby | 错误行高亮 + 与关联输入行同时可见 |

（Web 前端若未来存在，按文档第 5 节原样；AC-22/46 的 aria 项标注为「Web 侧约定，bingo TUI 按映射断言」。）

---

## A. 状态机复位与竞态（设计文档：状态机节 + §7）

| ID | 触发 | 预期（可量化） | 断言方法 | 优先级 |
|---|---|---|---|---|
| AC-01 | 任意异步操作完成（成功） | `idle→loading→success→idle` 完整闭环；`chat.busy` 清除、状态行更新回正常 | 单元（fake timers） | P0 |
| AC-02 | 任意异步操作失败 | `idle→loading→error→idle` 完整闭环；错误展示后回到 idle，可再次触发 | 单元（fake timers） | P0 |
| AC-03 | 复位动作四项（TUI 映射） | (1) `chat.busy`/loading 态清除、状态行更新；(2) 错误行**高亮移除**（无残留错误样式）；(3) 状态区**内容更新**（非删行）；(4) 成功→下一操作可见；失败→错误行/重试项**滚动到可见区 + 高亮** | 单元 + 组件 | P0 |
| AC-04 | 焦点转移时序 | TUI 帧循环天然在渲染后（无 rAF 问题）；断言错误行**渲染后**滚动到可见区 + 高亮；渲染失败则跳过、不阻塞 | 单元 + 组件 | P0 |
| AC-05 | 陈旧响应竞态 | 重试/新请求发起后，旧请求的迟到成功/失败响应被忽略；**不得出现新成功被旧 error 闪掉**。**实现机制（qa 回归确认，ui/ux v1.14 对齐）**：竞态防护为 **drop future + cancel 通道**（query.rs `cancel_requested`/`aborted`）结构性保证，**无独立序号计数器**——结构性取消优先，序号校验未单独实现 | 单元（注入延迟响应序列） | P0 |
| AC-06 | 超时计时器取消 | 超时计时器在成功/失败/取消时**同步取消**；成功后延迟区间内无迟到 error 进入错误态 | 单元（fake timers，推进到超时点后断言无错误） | P0 |

---

## B. Loading（设计文档 §1）

| ID | 触发 | 预期（可量化） | 断言方法 | 优先级 |
|---|---|---|---|---|
| AC-07 | 异步操作耗时 >200ms | 200ms（±50ms）后 loading 态出现；<200ms 完成则**不闪烁**（无 loading 出现又消失） | 单元（fake timers） | P0 |
| AC-08 | 局部操作 | 操作位（按钮/动作行）spinner **原位替换**图标位；文案不换（保持「提交」） | 组件 | P0 |
| AC-09 | 任意加载中 | **禁止全屏阻塞**；页面级为内容区骨架/占位 + 状态行（沿用 `chat.busy`） | 组件 | P0 |
| AC-10 | loading 期间触发提交动作 | 防重以**提交动作**为粒度：命令输入/Enter/快捷键提交路径统一拦截（`isSubmitting` 门控 onSubmit 等价物），均不产生第二次请求 | 单元 + 组件 | P0 |
| AC-11 | 同一动作连点 | 幂等保证：同一提交动作 loading 期间仅 1 次请求（可注入计数钩子验证调用次数=1） | 单元（测试钩子） | P0 |

---

## C. 超时（设计文档 §1 超时行 + §7；dev 第 31 条定夺口径）

> **超时分层（dev 定夺，2026-08-07）**：
> - **`TIMEOUT` 呈现级别由触发上下文决定**：短同步=页面级（AC-12/13/14），长回合=全流程级（AC-53）；fixture/断言不得单由 code 推断级别。
> - **短同步操作**（`list_models`/`count_tokens`/`complete_text` 等）：反馈层读 10s / 写 15s，超时即失败（`TIMEOUT` 码），**首要动作 = 重试**（页面级）。
> - **agent 长回合（流式 + 多轮工具）**：**不套用 10s/15s**——回合中已有持续进度反馈（状态行 + 活动行 + `chat.busy`），超时由传输层（120s/60s）+ 用户中断兜底。
> - **长回合真失败（传输层超时/中断）→ 升级全流程级错误**（AC-26），给「可重试或返回」路径，不静默局部提示。
> - **取消机制**：现有 client 请求均以 tokio `timeout()` 包裹/可 drop 的 stream——反馈层超时**再包一层 timeout、到点 drop future**，reqwest 底层连接随之取消；**序号校验仅作非 timeout 路径的迟到响应兜底**（不真取消）。
> - **写路径防御**：drop 对「服务端已应用写」是 best-effort，超时→重试仍建议动作级幂等兜底（防极端竞态下重复落库）。

| ID | 触发 | 预期（可量化） | 断言方法 | 优先级 |
|---|---|---|---|---|
| AC-12 | **短同步**读操作 10s 未完成 | 10s 后进对应错误级；错误码 `TIMEOUT`；提示可重试 | 单元（fake timers） | P0 |
| AC-13 | **短同步**写操作 15s 未完成 | 15s 后进对应错误级；错误码 `TIMEOUT`；提示可重试 | 单元（fake timers） | P0 |
| AC-14 | 读/写分档正确 | 读 10s、写 15s，两档互不混淆（读在 11s 前必报，写在 14s 前不报） | 单元（fake timers） | P0 |

> **`TIMEOUT` 呈现级别（qa #72 对齐，文档 §4.4 v1.15）**：`TIMEOUT` 码的呈现级别由**触发上下文**决定——短同步（AC-12/13/14）=页面级（错误行高亮 + 重试可达）；长回合传输层超时（AC-53）=全流程级（整屏态）。断言不得只按 code 推断级别。
| AC-15 | 超时后重试 | 超时错误态可重试；重试成功走状态机复位（AC-02/AC-03）；**写操作超时→重试不重复落库**（动作级幂等防御，drop 是 best-effort） | 单元 + 集成 | P1 |
| AC-53 | 长回合失败升级 | agent 长回合（流式+多轮工具）**不套 10s/15s**；传输层超时/中断导致失败时，错误为**全流程级**（非局部提示），含「可重试或返回」路径 | 单元 + 组件 | P0 |
| AC-54 | 超时取消机制 | 反馈层超时到点 **drop future**（底层请求取消，可钩子计数/无后续网络活动）；序号校验仅作非 timeout 迟到响应兜底（AC-05） | 单元（测试钩子） | P0 |

---

## D. Toast（设计文档 §2）

| ID | 触发 | 预期（可量化） | 断言方法 | 优先级 |
|---|---|---|---|---|
| AC-16 | toast 出现 | 3s（±500ms）自动消失 | 单元（fake timers 精确断言） | P0 |
| AC-17 | 用户手动 | 可手动关闭（关闭动作可按键触发） | 组件 | P0 |
| AC-18 | hover 到 toast | **暂停**计时；移开后续走**剩余时间**（非重置回 3s） | 单元（fake timers：先推进 1s→hover→推进 5s→已消失则断言失败） | P0 |
| AC-19 | 键盘聚焦到 toast | 与 hover 同：暂停并续走剩余时间；失焦后续走 | 单元 + 组件 | P0 |
| AC-20 | 第 3 条 toast 触发 | 同时最多 2 条；**仅槽满时顶掉最旧**；未满时新条排队等待 | 单元（fake timers）+ 组件 | P0 |
| AC-21 | 同类重复触发（连点「已复制」） | **替换为同一条并重置计时**，不堆叠（同一 toast 标识恒 ≤1） | 单元 + 组件 | P0 |
| AC-22 | toast 可访问性 | **Web 侧约定**：容器 `aria-live="polite"`；含动作入口时 `role="status"`（勿用 `role="alert"`）。**bingo TUI 侧**：toast 显示于状态区、内容更新可感知（映射：状态区内容更新非删行） | 组件 + SR 审计（仅 Web） | P0 |
| AC-23 | 文案 | 一句话结果 + 必要时动作入口（「已复制 ✓ / 撤销」） | SR 审计/人工 | P1 |

---

## E. 错误态三级 + 混合态（设计文档 §3）

| ID | 触发 | 预期（可量化） | 断言方法 | 优先级 |
|---|---|---|---|---|
| AC-24 | 字段级校验失败 | 错误行内联于输入区下方：图标 + 具体原因；**高亮仅标错误输入行**；错误行与关联输入行同时可见（映射 `aria-invalid`+`aria-describedby`）；光标/高亮定位到对应输入行 | 组件 | P0 |
| AC-25 | 页面级失败 | 错误卡片/占位区 + 重试项；错误行高亮 + 滚动可见；重试项可达且被选中（映射聚焦重试按钮） | 组件 | P0 |
| AC-26 | 全流程级失败 | 整屏错误状态 + 返回路径（非死路）；焦点落到首要动作项 | 组件 | P0 |
| AC-27 | 批量部分失败 | 混合态：「成功 n/m，失败 k 项」+ 失败项列表；失败项可**单独重试**；失败项列表滚动可见/可导航 | 单元 + 组件 | P0 |
| AC-28 | 任意错误文案 | 必须含「发生了什么 + 用户能做什么」；禁止「操作失败」死路文案 | 人工/测试钩子采样 | P0 |
| AC-29 | 错误码→用户动作映射 | `AUTH_REQUIRED→重新登录/配置 key`、`PERMISSION_DENIED→返回/申请`、`SERVER_ERROR→稍后重试`、`OFFLINE→检查网络`；同一错误码 UI 动作一致 | 组件（注入各错误码） | P0 |

---

## F. CLI 结构化错误契约（设计文档 §4.1/4.3 + §7）

| ID | 触发 | 预期（可量化） | 断言方法 | 优先级 |
|---|---|---|---|---|
| AC-30 | 非 TTY 下任意错误 | stderr 输出单行 `[error] code=<SCREAMING_SNAKE> msg=<单行>`，可 grep | 集成（spawn CLI，非 TTY） | P0 |
| AC-31 | msg 含换行/制表符 | 归一化为空格，单行不被破坏 | 集成 | P0 |
| AC-32 | msg 超长 | 截断 200 字符（长度 ≤200） | 集成 | P0 |
| AC-33 | 多行堆栈 | `detail=<JSON 转义>` 承载，**仅 `--verbose` 输出**；主 msg 保持单行 | 集成 | P0 |
| AC-34 | 断言稳定性 | 同一错误：改 msg 文案 → 测试不破；改 code → 测试必破 | 集成（等价类） | P0 |
| AC-35 | 命名规范 | 所有 code 匹配 `^[A-Z][A-Z0-9_]*$`（SCREAMING_SNAKE） | 集成 + 单测（码表扫描） | P0 |
| AC-36 | 双出口一致性 | GUI 与 CLI 对同一底层错误产出**同一 code**（共用 `map_error`）；两端对照断言 | 单元（构造各代表错误→两端各调 map_error 比对）+ 集成 | P0 |
| AC-37 | TTY/非 TTY 信息等价 | 同一操作两种环境反馈信息不丢（仅呈现形式不同：spinner vs 日志行） | 集成 + 人工 | P1 |
| AC-52 | TUI 错误行结构化 | TUI 错误行基于结构化 `UiEvent::Error { code, msg }` 渲染（dev：现 `UiEvent::Error(String)` 改造 3 处即可），**非 `to_string()` 拼接**；TUI 渲染层天然带稳定码可断言 | 单元（构造 UiEvent 断言渲染输入）+ 组件 | P0 |

---

## G. 错误码基础设施（设计文档 §4.3）

> 注：AC-38/40/43 以 v1.6/v1.7 **修正口径**为准（穷尽 match 无 `_` 臂 + 单测枚举每模块每 variant + **显式 GENERIC 路径、无自动兜底**），与 devex P1 及 dev 第 24/26 条发现的「§4.3/护栏 2 旧口径残留」修正方向一致——实现时**勿参照「未登记走兜底」「代表错误变体」等旧措辞**。

| ID | 触发 | 预期（可量化） | 断言方法 | 优先级 |
|---|---|---|---|---|
| AC-38 | `ErrorCode` trait 实现 | 各模块 match **穷尽所有 variant、无 `_` 臂**——新增 variant 未处理**编译直接报错** | 编译期（cargo build）+ 单测 | P0 |
| AC-39 | 显式 `GENERIC` | debug 构建下 `eprintln!` 醒目告警（含 missing_code 标记），**可断言**；release 下语义已知（暂未分配稳定码） | 单测（debug 断言）+ 集成 | P0 |
| AC-40 | 防漂移单测 | **枚举每个模块错误 enum 的每一个 variant**，断言映射到**非 `GENERIC`** 稳定码 | 单测（全 variant 枚举） | P0 |
| AC-41 | GENERIC allowlist | `const GENERIC_ALLOWLIST: &[&str]`：不在列表的 variant 一律非 `GENERIC`；列表条目为可定位路径（如 `"tool::bash::Error::NonZeroExit"`）；每条带 `TODO(generic-allow): <issue>/<日期> <理由>` 注释 | 单测 + review | P0 |
| AC-42 | 码值生命周期 | 码值**只增不改不重用**（semver）：发布后语义冻结；码表单文件追加，review 可见 | 单测（码表唯一性）+ review | P0 |
| AC-43 | 显式 GENERIC 路径 | 暂未分配稳定码的 variant **显式返回 `GENERIC`**（已发布稳定码，非临时值）；**不存在「未登记自动落 GENERIC」**（match 穷尽无 `_` 臂，v1.6+ 口径，dev 第 26 条确认） | 单测 | P0 |
| AC-44 | 契约文件 | 契约集中在 `src/error.rs`：`ErrorCode` trait / `GENERIC`+debug 告警宏 / 共用 `map_error` 出口函数 / 防漂移单测；**单出口 + 单码表** | 结构审查 + 单测 | P0 |
| AC-45 | 场景→错误码一致性 | 实现与「场景→错误码示例表」一致：超时→`TIMEOUT`、登录过期→`AUTH_REQUIRED`、无权限→`PERMISSION_DENIED`、服务端→`SERVER_ERROR`、无网络→`OFFLINE`、配置非法→`CONFIG_INVALID` | 单测 + 集成 | P0 |

---

## H. 可访问性（设计文档 §5 + §7；bingo TUI 按「TUI 映射基准」执行）

| ID | 触发 | 预期（可量化） | 断言方法 | 优先级 |
|---|---|---|---|---|
| AC-46 | 读屏读取 | **Web 侧约定**：toast（`aria-live`）与错误（`role="alert"`）可被 SR 读取。**bingo TUI 侧**：状态区错误/toast 内容可读（映射：状态区内容更新非删行），SR 支持有限则降级为人工确认 | SR 审计（仅 Web）+ 人工 | P1 |
| AC-47 | reduced-motion | **Web 侧约定**：`prefers-reduced-motion` 下动效关闭、loading 指示保留。**bingo TUI 侧**：TUI 无该概念——spinner 动画频率可简单降级，**loading 指示本身保留**（慢加载可感知是状态不是装饰） | 人工（TUI）/ 组件（Web） | P0 |
| AC-48 | 错误码高级详情 | 折叠区默认隐藏；**TUI 侧**：按键切换展开/折叠，展开态可切换、可感知（映射 `aria-expanded`） | 组件 | P1 |
| AC-49 | loading 指示 | **Web 侧**：`aria-busy="true"` + disabled；spinner `<span role="status">`。**bingo TUI 侧**：`chat.busy` + 状态行 spinner 标识 | 组件 | P0 |

---

## I. 可测试性基建（设计文档 §6）

| ID | 触发 | 预期（可量化） | 断言方法 | 优先级 |
|---|---|---|---|---|
| AC-50 | 测试钩子 | 组件/命令提供可注入钩子：**可注入延迟**（触发 loading/超时）、**可注入失败响应**（触发各错误级），各态稳定复现 | 结构审查 + 冒烟 | P0 |
| AC-51 | 时序测试策略 | 200ms/3s/10s/15s 时序**全部走 fake timers**（ms 级）；E2E 不做时序断言（防 flaky） | 审查（测试清单） | P0 |

---

## 共用解析 helper（测试侧契约）

```rust
/// 解析单行错误契约。仅解析 `[error]` 行；`[progress]` 等不适用。
/// 断言只依赖 code；msg/detail 仅供展示与排查，永不断言。
pub struct ParsedError {
    pub code: String,      // SCREAMING_SNAKE，normative
    pub msg: Option<String>,     // 单行 ≤200，换行已归一化为空格
    pub detail: Option<String>,  // JSON 转义，仅 --verbose 出现
}

/// 语法：`[error] code=<CODE> msg=<single line>[ detail=<json>]`
/// msg 在 ` detail=` 处截断（归一化后 msg 内不应出现该序列）；无 detail 时取到行尾。
pub fn parse_error_line(line: &str) -> Option<ParsedError>;

/// 断言助手：assert_code!(line, "TIMEOUT") 等。永不断言 msg 文本。
```

- helper 与 dev 的防漂移单测互为镜像：单测保证「每 variant → 稳定码」，helper 保证「CLI 输出行 → 可断言 code」。
- 时序测试在 Rust 侧用 `tokio::time::pause/advance`（tokio 已含 time 特性，零新依赖，dev 第 21 条确认）；chat.rs 现有无 runtime 纯逻辑测试模式可复用。
- 实现时点：`src/error.rs` 落成后，helper 随集成测试落地（测试侧代码，由 qa 维护）。

---

## 回归清单（按发布门禁排序）

P0 门禁：AC-01/02/03/04/05/06（状态机）→ AC-07/10/11（loading）→ AC-12/13/14/53/54（超时+长回合+取消机制）→ AC-16/18/19/20/21（toast）→ AC-24/27/29（错误态+混合态+映射）→ AC-30/32/34/35/36/52（CLI 契约 + UiEvent 结构化）→ AC-38/39/40/41/42/43/44/45（错误码基建）→ AC-47/49（可访问性）→ AC-50/51（测试基建）。

P1 后补：AC-15、AC-23、AC-28（人工）、AC-37、AC-46、AC-48。

## qa 回归记录（v1.3.1，2026-08-07）

验证基线：`cargo build` / `cargo clippy --all-targets` 零警告；`cargo test` **553 通过 0 失败**；实测 CLI 两个错误码出口。

**实测通过（P0 主干）：**
- **AC-30/45**：非 TTY `[error] code=AUTH_REQUIRED msg=...` exit=1（干净 HOME 无 key）；`[error] code=CONFIG_INVALID msg=...` exit=1（坏 settings.json）——断言锚 code ✅
- **AC-12/13/14**：`SHORT_READ_TIMEOUT=10s` / `SHORT_WRITE_TIMEOUT=15s` + 常量断言测试 ✅
- **AC-31/32**：`sanitize_msg` 归一化 + 200 字符截断（含中文逐字符）单测通过 ✅
- **AC-35/38/44**：SCREAMING_SNAKE 断言、10 模块 ErrorCode 穷尽 match 无 `_` 臂（编译强制）、契约集中 `src/error.rs` ✅
- **AC-36**：TUI 经 `map_error`、CLI 经 `error_code_boxed` 同源码表 ✅
- **AC-41/43**：`GENERIC_ALLOWLIST` 空、全 variant 稳定码（显式 GENERIC 路径为零）✅
- **AC-52**：`UiEvent::Error { code, msg }` 结构化；TUI 渲染 `[error] code=... msg=...` + `busy=false` 复位 ✅
- **AC-53/54**：长回合保持传输层 120s/60s；反馈层 timeout 到点 drop future 取消 ✅（AC-53 的 TUI 呈现断言待组件级补）

**整改项（需 dev 确认/处理）：**
1. **[P1] AC-40 漂移覆盖缺口**：`TeamError` 3 个 variant 中**仅 `Invalid` 被单测构造**，`Io`/`Parse` 未枚举——若改显式 GENERIC，防漂移测试抓不到（devex 曾批的「代表错误变体」模式残留）。修复：`error.rs` 单测补 `TeamError::Io(io::Error::other)` + `TeamError::Parse(serde_json err)` 断言。其余模块全 variant 覆盖确认 ✅。
2. **[P1] `error_code_boxed` 隐式 GENERIC + downcast 登记表漂移**：CLI 出口（main.rs:279）走 boxed 路径，末端直接返回 `GENERIC`——**无 debug 告警**（`missing_code` 为 dead code 从未调用），且 `downcast_error_code!` 是手工登记表，新增 ErrorCode 类型漏登记 → **静默 GENERIC**。修复建议：(a) `error_code_boxed` 落 GENERIC 时（debug 构建）调用 `missing_code`；(b) 补「各 ErrorCode 类型 boxed 经 `error_code_boxed` 可达且非 GENERIC」测试（当前仅测 QueryError + 未知 io::Error）。
3. **[P2] AC-39 无可执行断言**：`missing_code` 当前无任何调用/测试（无显式 GENERIC 路径）。建议补 cfg(test) 场景断言 debug 告警可触发。
4. **[P2] AC-05 机制标注**：竞态防护实际为 **drop future + cancel 通道**结构性保证（query.rs `cancel_requested`/`aborted`），无独立序号计数器——比序号校验更强，建议 AC-05 备注明确「序号校验未单独实现，结构性取消优先」。
5. **[Info] AC-33**：无 `--verbose` 无触发点，dev v1.4 已标注「暂不适用」，认可。

**回归结论**：P0 主干**通过，有条件放行**——整改 1/2（P1）建议本迭代处理（低成本、补 2 个测试）；整改 3/4（P2）可随文档标注跟进；AC-15 重试幂等、AC-53 长回合失败 TUI 呈现、AC-26 全流程级整屏态属组件级回归，待 TUI 组件测试基建补齐后覆盖。

### 复验记录（v1.6.1，2026-08-07，响应 devex 落修 #48）

devex 落修 P1-P3 + missing_code 代码落地后复验（clippy 零告警、cargo test 553 通过 0 失败）：

- ✅ **整改 1（AC-40 TeamError 漂移覆盖）**：`config_and_storage_errors` 现枚举 TeamError 全 3 variant（Invalid/Io/Parse → `CONFIG_INVALID`，走 `assert_stable_codes`）。
- ✅ **整改 2a（missing_code 代码落地）**：`error_code_boxed` 落 `GENERIC` 分支 debug 构建下调 `missing_code` 告警（ui/ux v1.14 要求 + #47 范围）；宏登记表漏登记/未实现类型不再静默。
- ⏳ **整改 2b（宏登记表覆盖测试）**：仍待 dev 表态 boxed 技术必要性后落——若确认必要，补「10 类型经 boxed 路径断言非 GENERIC」。
- ✅ **整改 3（AC-39）**：`missing_code` 已由 boxed GENERIC 分支调用，debug 下 `boxed_error_walks_cause_chain`（未知 io::Error → GENERIC）路径已行使该告警（eprintln 输出未断言，正式 eprintln 断言可后补）。
- ✅ **整改 4（AC-05 机制标注）**：AC-05 行已标注（v1.6）。
- ⏳ **P2（boxed 出口技术必要性）**：待 dev 表态。

### 复验记录 2（v1.7.1，2026-08-07，响应 dev #49 落修）

dev 表态 P2 + 落宏登记表覆盖测试后复验（`cargo test error::` 7 通过；全量 553 通过 0 失败；clippy 零告警）：

- ✅ **整改 2b（宏登记表覆盖测试）**：`boxed_export_covers_all_registered_modules` 已存在并通过——10 类型逐一 boxed 断言非 GENERIC + `samples.len()==10` 双处登记对照（与 `downcast_error_code` 宏清单一一对应）。
- ✅ **P2（boxed 技术必要性）**：dev 正式表态「boxed 场景 `map_error` 静态泛型无法覆盖 CLI 顶层 `&dyn Error`，`error_code_boxed` + 宏登记表必要」；「统一单一入口」评估为纯形式重构、按默认做减法不采纳，护栏 4 双出口口径无需回退。

**复验清单全部闭环**：整改 1/2a/2b/3/4 + P2 必要性全绿；回归结论维持「P0 主干通过」。

### 组件级回归记录（v1.9.2，2026-08-07，#14 TUI 组件级回归）

**qa 断言落地（565 tests 全过 + clippy 零告警，chat.rs 4 个 `qa_*` 测试）**：
- **AC-15（超时重试幂等）** ✅：TUI 层（整屏态 Enter=重试可达、重试后状态复位）+ client 层（超时落 TIMEOUT）已断言。
  **服务端「不重复落库」边界（dev #99 / qa #98，main 方案① 定案）**：短同步写路径 = `complete_text`（compact/memory 纯生成，**无持久化副作用**），重试覆盖式无害；**服务端幂等依赖 API 幂等能力（当前 LLM API 无幂等头），客户端结构性保证（drop future + 取消通道）为唯一防御**；幂等键不必要（无副作用写面），未来接入幂等键 API 时属「能力升级」非「补缺口」。
- **AC-26（全流程级整屏态）** ✅：整屏态（标题+码+说明+动作+光标隐藏）+ Esc 返回 + Enter 重试 + 焦点落首要动作。
- **AC-53（长回合失败升级）** ✅：FX-11（TIMEOUT+LongTurn）→ 全流程级整屏态，与 FX-01（同码短同步=页面级）**同码不同级**对照（TIMEOUT 双级别由 context 区分实测）。
- **AC-29（错误码→动作）** ✅：`qa_ac29` 全 11 fixture 逐码矩阵（级别由生产者显式携带 + 渲染形态与 level 匹配）。
- **真实路径** ✅：`qa_fx01_real_path`（/model 拉取超时 → 页面级错误行，**生产发射源** list_models，非 fixture 单腿）。
- **呈现层验收（ui/ux #20）**：FX-01…11 注入→渲染链路全部通过（A1/A3/C2/D2/D3/F1/F2/F3/G1/G3）；H 区折叠（AC-48 P1）+ 人工项待后续。
- **DX 复评（devex #15）**：级别在事件链中存活 + 发射源/复位/降级保留核验通过，已关闭。
- **短操作发射源（main #91 方案①）**：list_models/count_tokens 失败发 Page+ShortSync 错误行、降级行为保留、Field 级不补——#14 全链路闭环。

---

## 变更记录

- v1.0（2026-08-07）：依据 `feedback-states.md` v1.7 产出 51 条断言表 + 解析 helper 契约；标记 api/client.rs 超时不一致发现（AC-15 备注）。
- v1.1（2026-08-07）：按 dev 第 21 条评审与 main「以 TUI 行为为准」指示修订——引入「TUI 映射基准」表，ARIA/DOM/rAF/prefers-reduced-motion 类断言（AC-03/04/08/09/10/17/22/24/25/26/27/46/47/48/49）改为 TUI 可观测行为；时序测试注明用 `tokio::time::pause/advance`（零新依赖）；新增 AC-52（`UiEvent::Error` 结构化）；G 节注明以 v1.6/v1.7 修正口径为准（对齐 devex P1）。
- v1.2（2026-08-07）：按 dev 第 26 条复评对齐——AC-43 措辞改为「显式 `GENERIC` 路径」（删除「未登记自动落 GENERIC」过期语义），G 节注同步扩展到护栏 2 口径。
- v1.3（2026-08-07）：按 dev 第 31 条定夺同步超时分层——AC-12/13/14 限定「短同步操作」；新增 AC-53（长回合不套 10s/15s、失败升级全流程级）与 AC-54（反馈超时 drop future 取消机制）；AC-15 关闭原发现备注、补写路径幂等防御；共 54 条。头部钉对应文档 **v1.11**（ui/ux 第 34 条已将超时细化提前落盘，非实现期回填）。
- v1.4（2026-08-07）：dev 实现期回填——「dev 实现核对」节补 **AC-33 暂不适用**标注（无 `--verbose` 即无触发点；实现 `--verbose` 时补 `detail=` 断言）；§4.4 标题核对经 ui/ux 改为「场景 → 错误码表」，与码映射/防漂移断言一致，无行为口径变更。
- v1.5（2026-08-07）：qa 回归记录节——实测 553 测试通过 + CLI 双码出口验证；P0 主干有条件放行；整改项 5 条（P1：AC-40 TeamError 漂移覆盖缺口、error_code_boxed 隐式 GENERIC/downcast 登记漂移；P2：AC-39 无可执行断言、AC-05 机制标注；Info：AC-33）。
- v1.6（2026-08-07）：AC-05 行落地机制备注（drop future + cancel 通道、序号校验未单独实现，ui/ux v1.14 确认）；G 节 AC-39/43 待 devex P1-P3 落修后同步 missing_code 告警口径（含 boxed 出口宏登记表漏登记分支）。
- v1.7（2026-08-07）：复验记录（v1.6.1）——devex 落修后复核：TeamError 3 variant 断言 ✅、missing_code boxed 分支代码落地 ✅、空测试删除 ✅、clippy/test 全绿 ✅；待办收敛为 2 项（宏登记表覆盖测试、boxed 出口必要性表态，均待 dev）。
- v1.8（2026-08-07）：复验记录 2（v1.7.1）——dev #49 落宏登记表覆盖测试并表态 boxed 必要性后，**复验清单全部闭环**（整改 1/2a/2b/3/4 + P2 全绿）；回归结论维持「P0 主干通过」。
- v1.9（2026-08-07）：qa #69 + main 拍板契约对齐——AC-29 `AUTH_EXPIRED` 改为 **`AUTH_REQUIRED`**（对齐实现与文档 §4.4 单一来源，不新增码）；同步修正头部版本（此前 v1.3 滞后于实际 v1.8）与对应文档版本（v1.15）；AC-12/13/14 后补 **`TIMEOUT` 呈现级别注**（由触发上下文决定：短同步=页面级 / 长回合=全流程级，qa #72）。
- v1.9.1（2026-08-07）：ui/ux 按 qa #72 要求补 C 节**显式 TIMEOUT 级别口径**——「`TIMEOUT` 呈现级别由触发上下文决定：短同步=页面级（AC-12/13/14），长回合=全流程级（AC-53）」置顶超时分层块首，防 qa/呈现层对 `TIMEOUT` 级别断言各执一词（qa #76 实测核验两处一致 ✅）。
- v1.9.2（2026-08-07）：qa 回填 **#14 组件级回归记录**——AC-15/26/53/29 断言落地（4 个 `qa_*` 测试，565 全过）；AC-15 服务端幂等边界定案（main 方案①：短同步写=纯生成无副作用，结构性保证为唯一防御，幂等键不必要，dev #99 边界说明）。
