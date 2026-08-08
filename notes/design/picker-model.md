# 通用 Picker 组件模型（picker-model.md）

> 状态：草案（待 #slash-ux 对齐后 Post 定稿）· 2026-08-07 · Team A (feat/slash-ux)
> 任务来源：main 第 18 条——用户反馈「/think 选择器优化很棒，可以把选择器模型应用到所有类似的 slash 交互场景」。
> 本文件 = 设计（第一步，不写码）。交互模型主导：ui/ux；评审：devex；实现面评估：dev（本文件）。
> 回归底线：**抽象层不改变 ThinkMenu 行为，644 测试原样通过**。
> 对照：`slash-command-ux.md`（契约 v0.4，/think picker 规格）、`slash-ux.md`（dev 实现计划）、`research.md` D4/D16/D26/D30。

## 1. 泛化抽象：数据驱动的通用 Picker

### 1.1 从 ThinkMenu 提炼的交互模型（契约 v0.4 §3 的通用化）

| 要素 | ThinkMenu（现有实例） | 通用 Picker 契约 |
|---|---|---|
| 数据 | `items`（静态 6 项：label+desc） | `Vec<PickerItem { label, description }>`（静态或异步填充） |
| 选中态 | `selected: usize`（纯索引，浏览零副作用） | 同左——**效果只在确认键写入**，Esc 天然无回滚 |
| 生效态 | `current: usize`（● 固定不动） | `current: Option<usize>`（None = 无生效概念） |
| 浏览键 | ↑↓ wrap | 同左 |
| 直达键 | 1-6（`1..=items.len()`） | 可选 `number_jump`；**仅 `1..=min(items.len(), 9)`**（>9 项只用 ↑↓，数字位不够） |
| 确认键 | Enter = 应用+持久化 | Enter = 应用（持久化与否由场景语义决定） |
| 会话级键 | `s` = 仅本次会话（不写 settings） | 可选 `session_only`（对齐 CC `/model` 的 `s`） |
| 预览 | footer `think {level} ▸`（浏览态实时） | 可选 `preview`：浏览时 footer 对应段显示 `{label} ▸` |
| 行渲染 | `  {❯|●}{label:<w}  {desc}` + dim 提示行 | 同左（marker(2) + name 列 + desc 列 + 可选提示行） |
| 提示行 | `↑↓ 选择 · Enter 确认并保存 · s 仅本次会话 · 1-6 直达 · Esc 取消` | 可选 `hint_row`（文案按场景键位拼装） |

### 1.2 实现面评估（dev）——两个落点方案

**方案 A（组合薄壳，推荐首步）**：新增 `src/tui/picker.rs` 纯核心：

```rust
// src/tui/picker.rs（纯逻辑，零渲染依赖，可单测）
pub struct PickerItem {
    pub label: String,        // 显示名（渲染列）
    pub value: String,        // 应用值（Enter/s 落地时写入；与 label 分离，如 /resume 的会话名）
    pub description: String,  // 说明列（可选空）
}
pub struct PickerModel {
    pub items: Vec<PickerItem>,
    pub selected: usize,
    pub current: Option<usize>,
}
impl PickerModel {
    pub fn move_selection(&mut self, step: isize) -> bool;  // wrap
    pub fn jump(&mut self, n: usize) -> bool;               // 1..=min(len,9)
    pub fn row(&self, index: usize, width: usize) -> Row;   // ●/❯/name/desc 渲染（复用契约 §3.3 布局）
    pub fn hint_row(&self, width: usize, keys: &PickerKeys) -> Row;
}
pub struct PickerKeys { pub session_only: bool, pub number_jump: bool }
```

- `ThinkMenu`/`ModelMenu` 变薄壳：持有 `PickerModel` + 场景差异（两级结构/异步态/预览文本/确认动作），**Chat 字段类型与公开 API 不变** → 既有测试零改动，644 原样过。
- 键事件仍由 `Chat.on_key` 分派（优先级唯一事实源不动），壳层调 core 纯函数 + 场景动作。
- 渲染：`suggestion_rows` 的 think/model 分支改为读壳层 `rows()`（宽度预算逻辑随 core 迁移，契约束 3.3 布局不变量保留）。
- footer 预览：壳层暴露 `preview_text()`，`footer_row` 读（ThinkMenu 实例= `think {label} ▸`；无预览场景返回 None）。

**方案 B（统一替换）**：`Chat.think_menu/model_menu` 合并为 `Option<Picker>`，改 Chat API——破坏面大（测试、chrome、on_key 全动），**不做首步**，仅在方案 A 稳定后按需评估。

**两级/异步差异**（/model）：PickerModel 管「当前层级的列表+选择」；层级切换（Enter 进二级、Esc 回一级）与异步加载（loading/empty hint 行）留在 ModelMenu 壳层——core 不感知。理由：两级+异步是模型选择的专属形态，塞进 core 会污染单级场景（过度设计）。

### 1.3 不迁移的交互（明确排除，理由入档）

- **slash 下拉补全**（`/` + 过滤 + Tab 补全）：是「过滤补全」不是「轮换选择」，交互模式不同——不迁移。
- **实体选择器**（D30，ctrl+g 聚焦 + 窗口滑动 + Enter 打开模态）：窗口滑动与实体语义特殊——不迁移。
- **权限对话框**（1-9 数字选择，D22/D2）：独立模态通道——不迁移。

## 2. 候选场景清单（逐个评估）

| # | 场景 | 套用价值 | 适配差异 | 优先级 |
|---|---|---|---|---|
| 0 | **/think** | 现有实例，抽象层不改变其行为（回归底线） | — | 提交 A（纯重构） |
| 1 | **/theme** | 高：3 项静态（dark/light/auto），当前值可标 ●，1-3 直达 | ⚠ **行为变化点**：现 `/theme` 无参 = 直接切 auto；picker 化后无参 = 开选择器（对齐 CC「无参 → opens a picker」）。需 ui/ux 定：保留 auto 快捷（如 `/theme auto`）还是直接改语义。footer 无主题段 → 无预览；无 s（主题持久化是设计）；hint 行 = `↑↓ 选择 · Enter 确认 · Esc 取消` | ★★★★★ 提交 B |
| 2 | **/provider** | 高：单级静态（`default` + settings.providers），当前值标 ●，footer 已有 provider 段 → 预览有意义 | `s` = 仅本次会话（现 `/provider` 总是持久化，与 /think 的 s 同语义）；选项数可 >9（数字直达退化）；异步无 | ★★★★☆ 提交 C |
| 3 | **/resume** | 高：会话列表（`transcript::list` 同步磁盘读 = 静态快照），Enter 切换会话——CC 的 /resume 正是 picker | current = 当前 transcript（有则标 ●）；无预览（footer 无会话段）；无 s；选项 description = 会话名/消息数 | ★★★★☆ 提交 D |
| 4 | **/model** | 中高：两级（provider → models）+ 异步 + loading/empty hint | 差异最大：两级层级切换 + 异步态留在壳层（§1.2）；footer 预览意义有限（列表异步）——可选不做；`s` 键维持 defer（main 裁决 #6）；数字直达每级 1..N | ★★★☆☆ 提交 E（最后，风险最高） |
| 5 | **/mcp** | 低：子命令文本界面（list/enable/disable/reconnect/状态徽标），不是选项选择 | enable/disable 目标选择可用 picker，但整体形态不符——**不迁移**（记录理由） | — |
| 6 | **/permissions** | 无：规则列表 + 添加，非选择场景 | — | — |
| 7 | **/skills** | 中：`/技能名` 直接执行已很快，Picker 化是「浏览选执行」，价值中等——本轮不迁（记案） | 动态（skills 目录） | — |
| 8 | **/team** | 无：子命令家族分派（G1 子命令补全属 defer 项），非值选择 | — | — |

**评估口径**：套用价值 = 「选项列表 + 确认」交互是否天然成立；不成立的场景不强迁（main 边界 3）。

## 3. 迁移策略（一次一场景，每步独立提交可回退）

1. **提交 A（`refactor:`，零行为变化）**：新建 `src/tui/picker.rs`（PickerModel 纯核心 + 渲染 + 键转移纯函数）；ThinkMenu 改薄壳；**回归底线 = 644 测试原样过**（Chat 公开 API 不变 → ThinkMenu 既有测试原样保留）。新增验收测试（§5 devex 增量）：core 纯函数测试（wrap/jump 边界/row 布局/hint 拼装）、空 items 防御、value≠label 落地、薄壳确认动作。
2. **提交 B：/theme picker**——3 档静态、成本最低，顺带修「无参=直接切 auto」的行为缺口；行为变化需 ui/ux 定稿无参语义 + guide.md 同步。
3. **提交 C：/resume picker**——DX 价值最高（免去「看列表→手动敲名」），动态单级（磁盘扫描注入 items）；guide.md 同步。
4. **提交 D：/provider picker**——静态单级，列表信息列（URL/key）并入 description；`s` 语义可选；guide.md 同步。
5. **提交 E：/model 两级迁移**——Picker 管第一级 provider，第二级 model 列表独立（异步壳层）；最后做。
6. 每步独立提交、可单独回退；场景不适配即停（不强行迁移）。

## 4. 边界

- **零新依赖**；不引入 ratatui 之外的任何 crate。
- **feedback-states 规范照旧**：picker 确认 = 瞬态 ✓ 行（成功 2s）、错误/用法 = 错误桶（≥8s + 下次输入清除）、无新动效、状态机全重置（Esc 关闭清全部瞬态）；新增场景沿用错误码格式（如 `BAD_ARGUMENT`）。
- **文档同步**：每个迁移场景的行为变化同步 `guide.md`（slash 快速参考）+ `feedback-states.md` changelog。
- **回归底线**：644 测试全绿（提交 A 原样）；`cargo build` / `clippy -- -D warnings` 干净。
- **不动 dev/main**，只进 feat/slash-ux。

## 5. 对齐记录（devex 评审 #21 通过，4 问定稿）

1. **/theme 无参语义：开 picker（CC 对齐）**——顺带修「无参静默切 auto 无提示」的隐性缺口；`/theme auto` 显式快捷保留；guide.md 同步行为变化。
2. **/provider 的 `s` 键：做**——PickerKeys.session_only 配置位成本≈0；与 /think 一致；与 /model 的 s defer 不矛盾（model 切换有确认成本流程、provider 轻量）。
3. **提交 A 的 core 含 hint_row：含**——keys 配置拼装文案；行数预算沿用窄屏丢弃规则（think 6+1 行已在此规则内）。
4. **/model 的 footer 预览：defer**——异步列表增益有限，且要动 footer_row 的 model 段。

### devex 测试性增量（并入 §3 提交 A 验收）

1. PickerModel 纯核心测试：move_selection 两端 wrap / jump 边界（`1..=min(len,9)`，越界返回 false 不 panic）/ row 渲染（●❯ 重叠行、name 列宽、desc 截断沿契约 §3.3）/ hint_row 按 PickerKeys 拼装。
2. **空 items 防御（泛化后每场景都要）**：items 为空则菜单不开、回退用法行——测试覆盖。
3. **value ≠ label 落地测试**：/resume 的 label=展示名、value=会话 key——断言落地值用 value 不用 label。
4. 每场景薄壳至少一条「确认动作正确落地」测试（theme 切主题 / resume 切会话 / provider 切 provider + 持久化语义）。

### devex DX 增量

1. **resume 选项数上限**：items 截断（最近 20 个 + 提示行注明），desc 含会话名 + 消息数/时间。
2. **provider desc 保留信息列**（URL/key 脱敏沿用现有 4 字符 key 显示逻辑）。
3. **提交 A 标注 `refactor:`**（提取 PickerModel 纯核心，零行为变化），与后续 feat 提交区分。

## 6. 验收锚点（ui/ux 补充，qa 断言依据）

**提交 A（抽象提取，回归底线）**
- [ ] 644 tests 原样通过（现有 /think 测试断言**零改动**——它们是抽象层的验收套件，不是等价重写）
- [ ] `PickerModel` 纯逻辑可单测：`move_selection` wrap 上下界、`jump` 边界（1..=min(len,9)，越界返回 false 不 panic）、越界 clamp
- [ ] **空 items 防御（devex 增量）**：泛化后每场景都要——items 为空则菜单不开、回退用法行；测试覆盖（契约 §3.6 原先只有 think 有）
- [ ] **value ≠ label 落地测试（devex 增量）**：/resume 的 label=展示名、value=会话 key——断言落地值用 value 不用 label
- [ ] `/think` 行为逐字不变：双标记（●/❯ 重叠行）、1-6 直达、`s` 会话级、footer ▸ 预览、Esc 还原、窄屏截断、提示行
- [ ] chrome 行预算不变量保留（marker(2) + name 列 + desc 列 + 提示行；窄屏整体截断）
- [ ] commit 标注「refactor: 提取 PickerModel 纯核心（零行为变化）」，与后续 feat 提交区分

**提交 B-D（各场景）**
- [ ] /theme：无参开 3 项 picker（按拍板语义）、`/theme dark` 快速路径不变、Enter 持久化 + ✓ 行、Esc 无副作用
- [ ] /resume：无参开会话 picker、Enter 切换会话、Esc 不改当前会话、空列表提示行；**选项截断上限（如最近 20 个 + 提示行注明，devex DX 增量）**
- [ ] /provider：无参开 picker（● 标当前、URL+脱敏 key 进 desc 列——脱敏逻辑跟着走，现有 4 字符 key 显示）、`/provider <name>` 不变、`s` 会话级（不写 settings）、footer 预览 `{provider} ▸`
- [ ] 每场景薄壳至少一条「确认动作正确落地」测试（theme 切主题 / resume 切会话 / provider 切 provider + 持久化语义）
- [ ] 每场景：错误/用法行走错误桶（`[error] code=…` 格式沿用）、成功 ✓ 行 2s TTL

**提交 E（/model，可选）**
- [ ] 两级：Enter 进二级、Esc 弹回一级再 Esc 关闭；loading/empty hint 行保留；异步重载后 selected/current clamp

**通用**
- [ ] `cargo build` / `clippy -- -D warnings` / `cargo test --bin bingo` 每提交全绿
- [ ] 每提交同步 guide.md 快速参考 + feedback-states changelog（AGENTS.md 同步规则）
- [ ] 仅 feat/slash-ux，不碰 dev/main

## 7. 三方产出合并记录

- **dev 主体**（本文件）：PickerModel 形状 + 方案 A/B + 场景矩阵（0-8）+ 迁移策略 + 边界
- **devex 输入**（第 19/21 条）：/mcp 判 ❌（状态展示非值选择）、/resume DX 价值最高（已提前至提交 C）、评审底线（核心=单级、s 默认关）、/theme 隐性行为缺口论证（已入 §5.1）、空 items 防御 / value≠label 测试 / 每场景确认动作测试 / resume 截断上限 / provider desc 脱敏（已入 §6）
- **ui/ux 定稿**（交互模型）：§6 验收锚点 + §8 定稿立场

## 8. 交互模型定稿（ui/ux · 对 §5 四问）

1. **/theme 无参 = 开 picker**（CC 对齐 + 修「无参静默切 auto」隐性缺口）；`/theme auto|dark|light` 快速路径保留显式快捷。guide.md 同步行为变化。
2. **/provider 的 `s` 键做**：PickerKeys.session_only 成本≈0，与 /think 一致；/model 的 s defer 不矛盾（model 切换有确认成本流程，provider 轻量）。
3. **hint_row 由 core 拼装**（keys 配置驱动文案），每场景不复制；行数预算沿用窄屏丢弃规则。
4. **/model footer 预览 defer**：异步列表增益有限，且要动 footer_row 的 model 段。
