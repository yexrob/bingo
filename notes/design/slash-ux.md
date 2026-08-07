# Slash Command UX — dev 实现计划（对齐 slash-command-ux.md）

> 状态：对齐草案（ui-ux 的 `notes/design/slash-command-ux.md` v0.1 为**设计契约**，本文件为 **dev 实现计划**：代码落点、差异补充、测试与提交拆分）。
> 2026-08-07 · Team A (feat/slash-ux) · 对照 `notes/research.md` D4/D16/D20/D26/D30/D22。

## 0. 角色划分

- **交互设计契约**：`notes/design/slash-command-ux.md`（ui-ux 维护）——`/think` picker（●/❯ 双标记、1-6 直达、footer ▸ 预览、忙时白名单、无匹配提示行）、`/model` 延迟项、状态机 §3.1、键位 §3.2、布局 §3.3、验收锚点 §7。
- **本文件（dev）**：补充设计契约未覆盖的工程面——G1 参数提示结构化（契约 §4 下拉部分缺这一项，dev 提出并已获认可）、代码落点、submit 忙时分发安全、guide.md 同步、提交拆分。

## 1. 现状要点（调研结论，与契约 §1.3 一致）

- `SLASH_COMMANDS: &[(&str, &str)]` 扁平表（chat.rs:198），18 内建；desc 内嵌参数提示（非结构化）。消费方：`slash_help`、`update_slash_suggestions`（合并 skills）、`submit` 补全判定。
- 执行链：`submit()` → busy 分支（**全部入队**，含 slash 命令）→ `run_slash()` 按 (cmd,arg) 分派 → `push_slash_output` 瞬态行。
- `submit_queued()` → `start_turn(text, true)`：**队列里的 slash 命令作为普通文本发给模型**（不重新走 run_slash）——这是对齐 CC「忙时命令排队、回合后执行」语义时必须一并修的坑（见 §3.2）。
- thinking：settings `thinkingLevel` 三层合并 + `/think` 经 `upsert_project_settings` 持久化；`THINKING_LEVELS = [low..max]`（api/types.rs:139）、UI 表 `THINK_LEVELS`（chat.rs:267）= off + 五档，同序有测试。
- 菜单 = 输入框上方 suggestion rows（app.rs `suggestion_rows`），fullscreen 在 chrome 尾、inline 在 prompt 后；on_key 优先级：error > ask > model > think > search > entity > ctrl+c/esc > slash > 编辑（现状正确，不动）。
- footer_row（app.rs:144）读 `runtime.thinking` → `model_footer_label`；theme 已有 `claude` / `permission` / `inactive` token（契约 §3.4 样式可行）。

## 2. Claude Code / Codex 调研结论（与契约 §1.1/1.2 一致）

- CC：`/` 全列表 + 过滤；菜单行 = 命令名 + 参数提示（`<arg>` / `[arg]`）+ 说明；Tab 接受；`/model` 无参 → picker（↑↓ 轮换、`s` session-only、Enter 保存、有输出先确认）；`/effort` 档位 low..max 与 bingo 同序；命令忙时排队但 /status /tasks 立即执行。
- Codex：`/` popup 继续过滤；忙时 slash + Tab 排队下一轮。
- 已核实：bingo 的 picker 骨架（↑↓ wrap + 预选当前档 + Enter/Esc）与 CC 同构，**差距在反馈与层级，不在骨架**（契约 §2）。

## 3. dev 补充（契约未覆盖 / 需 dev 拍板的工程面）

### 3.1 G1：SLASH_COMMANDS 参数提示结构化（契约 §4 的补丁）

- 常量形状：`pub const SLASH_COMMANDS: &[(&str, &str, &str)]`（name, hint, desc）——三元组 tuple，零新类型。
  - hint 取自 desc 内嵌的用法片段并标准化：`help`→`""`、`model`→`[名称]`、`think`→`[off|low|medium|high|xhigh|max]`、`resume`→`[名称或关键词]`、`permissions`→`[allow|deny|ask] [规则]`、`mcp`→`[enable|disable|reconnect]`、`team`→`start|status|assign|stop|list`、`provider`→`[名称]`、`share`→`[--open]`、`rename`→`[名称]`、`theme`→`[dark|light|auto]`，其余 `""`。
  - desc 去掉 `（/xxx [arg]）` 前缀，保持纯净说明。
- 消费方：`SlashSuggestion` 加 `hint: String`（skills 项 hint=空）；`update_slash_suggestions` 保留 name/desc 匹配逻辑不变；`suggestion_rows` slash 臂渲染 `/{name} {hint}` 列 + desc 列（name_col 计算含 hint）；`slash_help` 同格式。
- 影响：纯显示 + 帮助排版，行为不变；涉及测试：`slash_menu_lists_commands_and_hides_with_args`（chat.rs:6474）等断言需随行格式更新。

### 3.2 忙时白名单（契约 §4.2/§5 锚点 5）——dev 核实结论

- **submit() 分发安全**：白名单处理函数全部同步 + fire-and-forget：
  - `slash_think`（sync：channel send + upsert）、`slash_theme`、`slash_provider`、`slash_status`、`slash_context`、`slash_tasks`（sync，refresh_tasks 是快照刷新）、`slash_help`、`slash_skills`（sync 只读，load_skills 磁盘读）——安全；
  - `slash_model` 无参路径 `open_model_models` 是异步拉取（后台 fetch，不阻塞事件循环）——安全；带参路径 sync。
  - 实现：busy 分支在 `queued.push` 之前先判断 `text.strip_prefix('/')` 是否命中白名单 → 命中走 `run_slash`（复用现有分发，无新路径）；**执行后不碰 busy 状态**（白名单命令不 reset 回合）。测试：忙时执行白名单命令后 `busy` 仍为 true。
  - 白名单 = 契约 §4.2 七条 + `help` + `skills`（devex 补，纯只读零副作用）；`resume` 无参 list 拉盘，留排队。
- **连带修复（必须）**：`submit_queued` 现在把队列文本直接 `start_turn` 发模型——非白名单 slash 命令（如忙时敲 `/clear`）会被当成 prompt 发给模型（错误语义）。改为：队列出队时若以 `/` 开头 → 走 `run_slash`，否则 `start_turn`。对齐 CC「忙时命令排队、回合后按命令执行」。

### 3.3 footer ▸ 预览（契约 §3.5/锚点 3）——dev 核实

- `footer_row` 单分支：`if let Some(menu) = &chat.think_menu` → think 段渲染 `THINK_LEVELS[menu.selected].0` + `▸`（theme.claude）；否则现状（runtime.thinking，inactive）。可测。
- 注意：`model_footer_label` 现签名接收 `(model, thinking: Option<&str>)`——预览分支不经过它、直接构造段文本，避免把预览值混进通用路径。

### 3.4 s 键（session-only）：**v1 延迟**，与契约 §5 的 defer 保持一致

- 契约把 `s`（/model session-only）列延迟项；/think 的 `s` 同理由：/think 持久化是其既有设计（settings 三层），session-only 是低频需求，v1 不加键、不加 persist 参数。
- 记录为 v1.1 候选（若用户反馈"调档不想写项目配置"再加）。

### 3.5 按键提示行（待 ui-ux 确认，默认并入）

- think 菜单尾追加一行 dim：`↑↓/1-6 选择 · Enter 确认 · Esc 取消`（7 行总计，frame 预算与窄终端丢弃规则不受影响——chrome 行构建不预测，D26/D27 不变量）。
- ui-ux 无异议则并入契约 §3.3；有异议则砍（不是硬需求）。

### 3.6 默认档口径修正（devex G2，P0）

现状三处「默认」互相矛盾，AGENTS.md 文档漂移违例：

| 位置 | 现文案 | 事实 |
|---|---|---|
| `settings.thinkingLevel` | 缺省不发参数（= off） | settings 缺省 `None`，off 不序列化 |
| `THINK_LEVELS` high 行 | `（默认档位）` | **误导**——缺省是 off，high 不是默认 |
| `toggle_thinking`（Alt+T） | 恢复默认 `medium` | 无 last_thinking 时的 fallback，非全局默认 |

**定一口径（并入 guide.md + THINK_LEVELS）**：
1. settings 缺省 = `off`（不发 thinking 参数，DeepSeek 等端点兼容）；
2. `THINK_LEVELS` high 行删「默认档位」字样（高亮建议可保留为「（推荐）」，待 ui-ux 定文案）；
3. Alt+T = off ↔ 上次非 off 档的快速开关，从未开启过时恢复 `medium`——这一条写入 guide.md 快捷键说明。
- 附带事实修正：**bingo 已有 Alt+T toggle**（chat.rs `toggle_thinking`，经 `slash_think` 持久化）——它已经是 CC Alt+T 的对应物，本次**不做、不改**，仅随 G2 统一文案。

### 3.7 slash 输出 TTL 分级（devex G3，main 裁决：**本轮顺手做**）

- 现状：`push_slash_output` 全部 2s TTL（`slash_at`）。未知命令/用法错误 2s 即消失，inline 落盘后不可展开。
- 方案：成功类反馈保持 2s；**错误/用法行 ≥8s 或常驻到下次输入**。实现：`push_slash_output` 增 `error: bool` 参数（显式标记，不按内容猜），`slash_lines` 记类型，TTL 渲染过滤按类型取不同窗口；与「无匹配 dim 提示行」互补（提示行是 chrome 非错误）。
- 边界：错误行不占 slash_lines 之外的新通道；/clear 仍清全部。

### 3.8 结构化错误码（devex G4，main 裁决：**本轮顺手做**）

- 现状：未知命令/非法参数纯文案（`未知命令: /xxx` / `用法: /think [...]`），feedback-states §4.1 要求错误出口带 `code=`。
- 方案（最小面）：未知命令 → `[error] code=UNKNOWN_COMMAND` + 原文案；非法参数 → `[error] code=BAD_ARGUMENT` + 用法行。码登记进 `src/error.rs` 码表（只增不改），TUI 瞬态行渲染码前缀。
- 范围控制：只覆盖未知命令与非法参数两类最常断言的口径；其余 slash 错误文案不动（避免放大）。qa 验收锚点可挂这两码。

### 3.9 dispatch 完整性测试（devex G5）

- `SLASH_COMMANDS` 每个 name 都有 `run_slash` 分支（含别名），且 `run_slash` 每个分支都在表内（别名映射到主名）——防止新增命令漏分发或死表项。
- `arg_hint` 与 `/help` 输出一致性测试：help 渲染 = 表内 hint 拼装，单一来源。
- picker 状态机测试：循环 wrap / 数字直达 / 确认 / 取消 / 当前档预选五条（既有模式扩展）。

### 3.10 待对齐（不在本轮，记录）

- **Q1 持久化层**：`/model` `/think` 写项目层 `.bingo/settings.json`（git 噪音）；CC 写 user 层。模型/思考级别更像个人偏好——**后续改 user 层，本轮不动**。
- **Q2 无匹配提示**：`/zzz` dim 提示（契约 §4.1）之外，「参数缺省才弹菜单」的命令非法值（`/think foo`）保持现状：用法行 + 不弹菜单 + 状态不变（= CC keep current 语义，已成立）。

## 4. 状态机（/think picker，与契约 §3.1 同源）

```text
Idle ──"/think" 无参 Enter──▶ MenuOpen{ selected = current }   (current = runtime.thinking 或 "off")
MenuOpen ──↑/↓──▶ MenuOpen{ selected ±1 mod 6 }                (wrap)
MenuOpen ──1..6──▶ MenuOpen{ selected = n-1 }                  (直达)
MenuOpen ──Enter──▶ apply(persist) ──▶ Idle                    (runtime 写入 + upsert + "✓ …" 瞬态行)
MenuOpen ──Esc──▶ Idle                                          (不写 runtime，footer 还原)
忙时：白名单命令绕开 busy 队列立即执行；其余入队，TurnEnd 后出队按命令/文本分派（§3.2）。
```

- 纯 `selected` 索引、效果只在 Enter 写 runtime——浏览零副作用、Esc 天然无需回滚（契约 §3.1 原则，dev 认可）。

## 5. 实现落点（文件 × 改动）

| 文件 | 改动 |
|---|---|
| `src/tui/chat.rs` | `SLASH_COMMANDS` 三元组；`SlashSuggestion.hint`；`update_slash_suggestions`/`slash_help` 格式；`ThinkMenu.current`（● 数据源）；`think_menu_key` 加 `1..6`；`submit` 忙时白名单分派（含 help/skills）；`submit_queued` 命令/文本分派；`push_slash_output` 错误/成功 TTL 分级（§3.7）；未知命令/非法参数带 `[error] code=`（§3.8）；THINK_LEVELS high 行文案（§3.6，一行） |
| `src/tui/app.rs` | `suggestion_rows`：think 臂 ●/❯/name/desc/提示行；slash 臂 hint 列；`footer_row` ▸ 预览分支 |
| `src/error.rs` | 登记 `UNKNOWN_COMMAND` / `BAD_ARGUMENT`（§3.8，只增不改） |
| `src/skills/bundled/guide.md` | `/think` 6 档 + picker 一句话 + 默认档口径（§3.6）+ Alt+T 说明 |
| `notes/design/feedback-states.md` | changelog 回填（v1.21）：瞬态 ✓ 行语义不变；忙时白名单即时反馈；错误行 TTL 分级；无匹配提示行为 hint 非 error |

## 6. 测试计划（对齐契约 §7 锚点 + dev 补充 + main 裁决）

1. 浏览后 Esc → `runtime.thinking` 不变（扩展既有模式）；
2. `1..6` 直达选中正确行；wrap 边界回归；**菜单打开时 1-6 被菜单消费、不进输入框**；
3. footer 菜单打开显示 `▸` 预览、Esc/Enter 后还原；**预览中 ↑↓ 仅切换 footer 数据源、不 dirty 文档重渲染**；
4. 忙时 `/think xhigh` 立即生效（不入队）且 **busy 仍为 true**；忙时 `/clear` 入队 → TurnEnd 后按**命令**执行（不发给模型）；
5. `/zzz` 显示无匹配提示行，继续输入清除；
6. `●` 标 runtime 档、`❯` 标 selected（分离时两行不同标记；重叠时 ❯ 占前缀位）；
7. slash 下拉行渲染含 hint 列；`slash_help` 新格式；**dispatch 完整性 + arg_hint/help 一致性（main 必做验收项，§3.9）**；
8. 未知命令/非法参数行含 `[error] code=UNKNOWN_COMMAND|BAD_ARGUMENT`（qa 断言码）；
9. 错误行 TTL ≥8s、成功行 2s（时钟推进/注入 now 测试）；
10. 回归：全部既有 slash 测试通过；`cargo build` / `clippy -- -D warnings` / `cargo test --bin bingo` 全绿。

## 7. 提交拆分（按依赖顺序，全部在 feat/slash-ux；main 裁决后范围 = P0 六项 + G3/G4）

1. `refactor(tui): SLASH_COMMANDS 参数提示结构化（G1，纯显示）`——含 hint 渲染 + dispatch 完整性测试 + arg_hint/help 一致性测试；
2. `feat(tui): /think picker ●/❯ 双标记 + 1-6 直达 + footer ▸ 预览`——契约 §3 核心；
3. `feat(tui): 忙时 slash 白名单即时执行 + 队列命令分派修复`——契约 §4.2 + §3.2；
4. `feat(tui): /zzz 无匹配提示 + 错误行 TTL 分级 + 结构化错误码`——契约 §4.1 + §3.7/3.8；
5. `docs: 默认档口径统一（THINK_LEVELS 一行 + guide.md）+ feedback-states changelog`——§3.6 + AGENTS.md 同步规则。

## 8. 明确不做（记录，防过度设计）

- 模型切换二次确认（bingo 无 CC 的缓存契约）；`s` session-only（v1.1 候选）；←/→ 在 /model 里调 effort（/think 已有独立 picker，保持分离）；子命令二级补全（`/mcp e → enable`，devex G1 中量小可 defer——本轮只做 arg_hint 展示）；j/k 菜单导航（吞输入，现状注释已论证）；通用 Menu 组件抽象（三菜单形态各异）。
- **Alt+T 不做不改**：bingo 已有（`toggle_thinking`，off ↔ 上次档，与 CC Alt+T 对应），仅随 §3.6 统一文案。
