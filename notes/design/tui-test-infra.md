# TUI 组件级回归测试基建需求（#14）

> 状态：v3.0（2026-08-07，qa 交付 3/3 完成——4 个断言测试写入 chat.rs：qa_ac53 长回合升级、
> qa_ac29 逐码矩阵、qa_page_error Buffer 层样式、qa_fx01_real_path 真实路径；565 tests 全过 + clippy 0。
> AC-15 落库幂等唯一遗留待定：无幂等键机制，服务端「落库计数=1」无法断言，待 main/dev 定夺）
> 关联：`notes/design/feedback-states.md`（设计 v1.16）、`notes/design/feedback-states-ac.md`（AC 表 v1.9）、
> `notes/design/feedback-states-presentation.md`（呈现层验收清单 v1.8，ui/ux）
> 角色：qa 出需求 → dev 基建实现 → qa 断言 + ui/ux 验收 → devex DX 复评（#15，已关闭）

## 0. 对齐结论（三方共识）

- **fixture 采纳**（devex #58 / ui/ux #60）：错误态 fixture 与测试钩子合并为同一机制
- **时序分级采纳**（dev #61 / ui/ux #62）：TUI 逻辑时序走参数化 `now: Instant` 注入（轻），client 超时时序走 tokio fake-timers（重）
- **toast 移出 #14**（ui/ux #62）：AC-16/18/19/20/21 无实现对象，待 toast 功能落地另行排期
- **qa 增量**：样式感知断言（`Recorder` 行级文本断言不含 cell style，高亮断言需补）

## 1. 基建需求（按 ui/ux 优先级排序，P0 = #14 必需）

### R1 测试钩子 + 错误态 fixture（优先级 1）

**需求**
- `ErrorFixture { code, msg, level, context, action, expect_style }` 数据层（**6 字段，ui/ux #80 ② 恢复 level**）
- **`level` 字段（呈现级别显式锚）**：非 TIMEOUT 码的级别是**码固有属性**——CONFIG_INVALID=字段级、AUTH_REQUIRED/PERMISSION_DENIED=全流程级、其余=页面级，`context` 表达不了（「短操作」会错误推导成页面级）。显式携带可直接断言、可 review，不复制 §4.4 映射逻辑
- **`context` 字段（qa #69 对齐增量 2，R1 设计硬性输入）**：呈现级别不能单由 code 推断——同一 `TIMEOUT` 码，短同步读超时 = 页面级（FX-01），长回合传输层超时 = 全流程级（FX-11，client.rs:67 确认落 TIMEOUT）。**level/context 必须存活到渲染路径（ui/ux #80 ③）**：`UiEvent::Error` 当前仅 `{ code, msg }`，渲染层拿不到级别——#18 生产改动需扩展带 level/context（chat.rs:2748/2792 发射时已知），否则同一 TIMEOUT 码无法区分 FX-01/FX-11
- 经 mpsc events 通道注入（沿用 `test_chat()`，不加新注入点）
- 渲染断言链路：注入 → `Frame::assemble(&chat, size)` → 断言
- 兼作 dev「错误态本地预览」+ ui/ux 验收载体
- 可注入延迟/失败响应（client mock 或 start_turn 注入点——**唯一改动面大的设计点，dev 需先出方案**）
- AC-15 写幂等需「延迟写响应注入 + 落库调用计数」

**错误码覆盖（与 ui/ux #68 FX-01…13 对齐定稿）**
`TIMEOUT`（页面级 + 全流程级双上下文）/ `SERVER_ERROR` / `OFFLINE` / `AUTH_REQUIRED` / `PERMISSION_DENIED` / `CONFIG_INVALID` / `RATE_LIMITED` / `TOOL_FAILED` / `HOOK_FAILED` / `STORAGE_ERROR` + 长回合传输层超时（FX-11）
- **`AUTH_EXPIRED` 契约漂移（qa #69 增量 1，main 已定夺方案①）**：设计 §3.1/AC-29 原用 `AUTH_EXPIRED`，实现不存在（client.rs 401 落 `AUTH_REQUIRED`）。main 采纳方案①（文档对齐实现、不新增码）——§3.1/AC-29 已改 `AUTH_REQUIRED`（文档 v1.15 / AC v1.9），fixture 用 `AUTH_REQUIRED` 即可
- **`GENERIC` 不进 fixture**：无实际返回点，正常用户路径不可见；显式 GENERIC 护栏断言由 error.rs 单测覆盖

**验收断言**
- 注入任一 fixture 失败 → 对应错误级可渲染（code 文本 + expect_style）
- 注入 9s 延迟 → loading 态可见 → 10s 触发 `TIMEOUT`
- 写操作重试落库计数 = 1（幂等）

### R2 Recorder 提升共用 + 样式感知（优先级 2）

**需求**
- `Recorder` 从 term.rs 测试模块提升 `pub(crate)`（screen/scrollback/计数器）
- 新增样式感知断言：`assert_row_styled(y, fg, bg, contains)`（cell fg/bg 颜色）
- 新增视口定位：`visible_rows()`（供「错误行滚动到可见区」断言）

**验收断言**
- 同一行文本断言 + 样式断言均通过
- 样式断言能区分 error 色 `(255,107,128)` 与正常色

### R3 时序基建（优先级 3）

**R3a 参数化 now 注入（轻）**
- 复用 `on_key_at` / `track_burst` / `ctrl_c` 的 `now: Instant` 参数模式补覆盖
- 覆盖：AC-07 loading 200ms、G 区超时态触发
- 不依赖 fake-timers，兼容现有无 runtime `#[test]`

**R3b tokio fake-timers（仅 client 超时时序）**
- 覆盖：**AC-12/13/14 到点行为断言（P0，当前仅常量断言）**
- P1 增强：AC-54 drop future 取消（结构性保证已通过，补「到点 drop」钩子计数）
- 长回合 20s 不触发短超时（AC-53）

### 明确移出 #14
- toast 时序（AC-16/18/19/20/21）——无实现对象

## 2. AC 对照

| AC | 内容 | 基建 |
|---|---|---|
| AC-15 | 超时重试幂等+复位 | R1(延迟+落库计数)+R3a+R2 |
| AC-26 | 整屏错误态+返回路径 | R2(样式+视口)+R1 |
| AC-53 | 长回合失败升级 | R3b+R2+R1 |
| AC-12/13/14 | 短超时分档行为 | R3b |
| AC-07 | loading 200ms | R3a |
| AC-29 | 错误码→动作 TUI | R1(fixture 逐码) |

## 3. 实施顺序

1. **dev**：R1 注入方案设计 → R2 提升 → R3a → R3b → fixture 落定
2. **ui/ux**：fixture 错误码覆盖清单与 qa 对齐；验收清单 v1.2 已就绪
3. **qa**：R1 注入方案确认后同步断言细节；基建就绪按上表逐 AC 断言
4. **devex**：DX 复评收尾（#15）

## 4. ⚠️ 呈现层缺口预警（待 main 拍板）

`UiEvent::Error`（chat.rs:1439）当前仅把最后一条 assistant 消息文本替换为
`[error] code=... msg=...`——**无错误行高亮、无整屏错误态、无重试/返回路径**。
ui/ux 验收清单 A/F/D/G 区无实现对象。基建就绪后断言将暴露此缺口。

建议：基建先行 → 断言暴露缺口 → 本轮补最小呈现层实现
（错误行高亮样式 + 整屏错误态骨架 + 重试/返回路径）→ ui/ux 验收。
若本轮不补，AC-15/26/53 停在「缺口已暴露」状态。

## 5. qa 断言规格草案（交付 3/3 前置，纯契约语义，待注入 API 落地后映射）

> 状态：草案（2026-08-07）。契约锚已定（AUTH_REQUIRED 单一码、TIMEOUT 级别由 context 决定）。
> 依赖：R2 渲染断言侧已就绪（`src/tui/test_util.rs`：Recorder 共用 + `assert_row_styled`
> + `visible_row_containing`，含 error 色自测 ✅）；R1 注入侧（fixture 数据层 / 可注入延迟 /
> 落库计数）实现中；呈现层最小实现（#18）pending——**高亮/整屏态断言依赖 #18**。
> **fixture 6 字段（ui/ux #80 ②③）**：`{ code, msg, level, context, action, expect_style }`；
> `level` 显式携带（断言「呈现级别正确」直接锚），`level/context` 需存活到渲染路径
> （#18 需扩展 `UiEvent::Error` 带 level/context，否则 TIMEOUT 码无法区分页面/全流程级）。

### AC-15 超时重试幂等（短同步写超时）
前置：注入 9s 延迟写响应 → 虚拟时间到 10s（R3b）。
- ① 错误态渲染：可见区含 `[error] code=TIMEOUT`（`visible_row_containing`）
- ② 错误行高亮：`assert_row_styled(y, fg=error 色(255,107,128), None, "TIMEOUT")`（依赖 #18）
- ③ 重试可达：重试项可选中（选中态渲染断言，依赖 #18）
- ④ 重试成功 → 状态机复位：busy=false、无错误残留、loading 清除（AC-02/03）
- ⑤ 写幂等：落库计数 = 1（R1 落库计数钩子）

### AC-26 全流程级整屏态
前置：注入全流程级 fixture（AUTH_REQUIRED / PERMISSION_DENIED / 长回合 TIMEOUT）。
- ① 整屏态呈现：错误标题 + 说明（发生了什么+能做什么）+ 首要动作 + 退出动作（`screen()`）
- ② 返回路径非死路：退出/返回动作可选中（D3）
- ③ 选中态落首要动作（D2）

### AC-53 长回合失败升级
前置：注入长回合传输层超时（context=长回合）。
- ① 虚拟时间 20s 不触发短超时（10s/15s，R3b）
- ② 失败后全流程级整屏态（非局部提示，F1/F2）
- ③ 含重试或返回路径（F3）

### AC-12/13/14 短超时分档
- ① 读 10s 在 11s 前必报（R3b）
- ② 写 15s 在 14s 前不报（R3b）
- ③ TIMEOUT 码 + 页面级呈现（区别于全流程级，context 决定）

### AC-29 错误码→动作（逐码）
- FX-01~11 逐码注入 → code 文本 + 动作可达 + 呈现级别（context 决定）

### AC-07 loading 200ms
- R3a now 注入：200ms 内 loading 态可见，过后消退。

### 执行依赖矩阵
| 断言 | R1 注入 | R2 渲染 | R3a | R3b | #18 呈现层 |
|---|---|---|---|---|---|
| AC-15 | 需(延迟+计数) | 需 | — | 需 | 需(高亮+重试) |
| AC-26 | 需(全流程 fixture) | 需(样式+视口) | — | — | 需(整屏态) |
| AC-53 | 需(长回合 fixture) | 需 | — | 需 | 需(整屏态) |
| AC-12/13/14 | 需(延迟) | 需 | — | 需 | 部分(页面级高亮) |
| AC-29 | 需(fixture 逐码) | 需 | — | — | 需 |
| AC-07 | — | 需 | 需 | — | — |
