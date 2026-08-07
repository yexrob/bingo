# bingo 官网视觉方向（Site Visual Direction）

> 版本：v1.1 · 状态：草案（对齐 prd-site.md v1）
> 用途：bingo 官网（静态站点，dokploy 部署）的视觉规格。官网开发按本文 token 表 + 组件清单直接实现；本文与 share 页设计（`share-page-design.md`）共享**同一组语义色值**（品牌橙 #D77757/#B05227、语义绿/红/金/青）。
> **关于 share 页**：share 页 v2.0 起为**浅色文档风**（对齐 opencode share 参考，用户指定）——官网是暗色终端风（营销门面），share 页是浅色文档风（阅读产物），两者刻意不同：终端血统归官网与 CLI，文档可读性归分享页；共享的是品牌色值与组件心智（工具行、状态字形、折叠交互），不是表面配色。
> **v1 关键约束（源自 PRD）**：纯静态、最好零 JS、无构建步骤；英文主站 + `/zh/` 单页中文摘要；站内 8 栏目；`/share/` 路径预留给将来内嵌 bingo share 产物。

## 0. 一句话定位

**官网是 bingo 的"产品说明书"，不是 SaaS 营销页。** 暗色为主、等宽为骨、克制高亮——把终端里那个 agent 工作台的气质原样搬到网页上，让访问者第一眼就确信"这是给认真做技术的人用的工具"。

Hero 锚点句（PRD §1，设计上它是 H1 的唯一候选）：

> **bingo is a local agent CLI written in Rust — the model produces intent, the harness gates every side effect.**

## 1. 品牌锚点（不可动摇）

| 锚点 | 值 | 说明 |
|---|---|---|
| 字标 | `▸ bingo` | 小写 bingo + accent 橙的 `▸` 前缀（与 CLI prompt 前缀、share 页 brand 同款） |
| 品牌橙 | `#D77757` | 唯一的"品牌时刻"颜色：CTA、字标前缀、hover、选区 |
| 底色 | 深 `#0B0B0D` | 近黑但不纯黑，带一点暖 |
| 字体 | 标题与终端 = 等宽；正文 = 系统无衬线 | 等宽承担气质，无衬线承担长文可读性 |
| 图形语言 | 终端窗口、ASCII/Unicode 符号（`▸ ∴ ⚙ ✓ ◇`）、1px hairline、文本/SVG 架构图 | 不引入插画、照片、3D、图标库 |
| 语言 | 英文主站；`/zh/` 单页中文摘要（节译 README.zh-CN） | 视觉语言两版完全一致，无双语切换 UI |

禁止：玻璃拟态、大面积渐变、发光霓虹、卡片堆叠阴影、圆角轰炸、卡通吉祥物、插画体系。

## 2. Token 表

### 2.1 色板（CSS 变量，与 share 页同源）

```css
:root{
  /* 表面 */
  --bg:        #0B0B0D;   /* 页面底 */
  --bg-elev:   #121215;   /* 卡片/终端窗口底 */
  --bg-sunken: #0E0E11;   /* hero 底（略深于页面） */
  --bg-code:   #141417;   /* 代码块/终端内容底 */
  --bg-hover:  #1A1A1E;
  --hairline:  #242428;   /* 分隔线 */
  --hairline-strong:#33333A;

  /* 文本（对比度均相对 --bg） */
  --text:  #E8E8E6;   /* 17.6:1 正文 */
  --dim:   #A3A3A8;   /* ~7.8:1 次级文本 */
  --faint: #6F6F76;   /* ~4.2:1 仅用于纯装饰（窗口 chrome、网点、分隔），禁止承载任何文字 */
  --faint-strong: #7A7A80;  /* ~4.6:1 信息性小字（特性示例/元信息/脚注）v1.2 新增，值经 dev 实现校验后定稿 */
  --ink:   #0B0B0D;   /* accent 底上的文字色 */

  /* 语义（全部 ≥ 4.5:1） */
  --accent: #D77757;   /* 品牌橙 6.2:1 */
  --accent-strong: #E8896B;  /* 大字号/描边态用（8.0:1） */
  --teal:   #4FB3C7;   /* 工具/信息 */
  --green:  #4EBA65;   /* 成功 */
  --red:    #FF6B80;   /* 错误 */
  --gold:   #FFC107;   /* 警告（隐私提示等） */
  --periwinkle: #B1B9F9; /* 次级强调（mode: free 等标签） */
  --mauve:  #AF87FF;   /* 次级强调 */
  --pink:   #FD5DB1;   /* 次级强调 */
}
```

使用纪律：
- 一屏内 accent 出现 ≤ 3 处（含 CTA）。
- 次级强调只用于**内容里的符号与标签**，不许用于按钮大面积填充。
- 所有颜色走 `var()`；无亮色节——全站暗色（若未来加长文博客，再单独评估浅色令牌，不在 v1 范围内）。

### 2.2 字体

```css
--font-mono: ui-monospace, "SF Mono", "JetBrains Mono", "Cascadia Code",
             Menlo, Consolas, "Liberation Mono", "DejaVu Sans Mono", monospace;
--font-sans: -apple-system, "SF Pro Text", "Segoe UI", "Noto Sans SC",
             "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", sans-serif;
```

| 用途 | 字族 | 规格 |
|---|---|---|
| 展示标题（H1/H2） | mono | 700，`clamp(28px, 5vw, 52px)`，行高 1.15 |
| 小节标题 H3 | mono | 700，18px |
| 终端 mockup / 代码 / 命令 | mono | 13–14px，行高 1.6 |
| 正文 / 特性描述 | sans | 400，16px，行高 1.7 |
| 元信息 / 标签 / footer | mono | 12–13px |
| 特性编号（`01 /`） | mono | accent，14px |

`/zh/` 页：等宽对中文回退系统中文 mono（`PingFang SC` 等），展示标题保持等宽感；正文同 sans 栈。

### 2.3 间距 / 圆角 / 动效

```css
--s1:8px; --s2:16px; --s3:24px; --s4:40px; --s5:64px; --s6:96px;   /* 8pt 网格 */
--radius:8px;          /* 终端窗口/按钮/输入 */
--radius-sm:4px;       /* chip/标签 */
--maxw:1120px;         /* 内容容器 */
--ease:cubic-bezier(.2,.6,.2,1);
--dur-1:120ms;  /* hover 反馈 */
--dur-2:300ms;  /* 可见性过渡 */
/* 章节垂直节奏：hero 96px → 各节 96px（移动端减半） */
```

## 3. 排版层级（页面骨架，对应 PRD 8 栏目）

```
nav（sticky，hairline 底，maxw 容器）
├── ▸ bingo … Features · How it works · Quick start · Docs · Contributing · GitHub [Install]
hero（--bg-sunken，96px 上下留白）
├── H1（PRD 锚点句）+ 副标题（sans 18px dim）+ 安装命令 + 双 CTA
└── 终端演示 mockup（下接，与 hero 共用容器边）
Features（8–10 特性网格，hairline 棋盘平铺）
How it works（harness 心智模型文案 + 文本/SVG 架构图，无 JS）
Quick start（3 步 + --print headless 示例 + README 链接）
Share 样例（预留：#/share/ 或独立页，v1 占位链接）
Docs / Contributing（索引列，指向 GitHub 资产，不复制全文）
CTA（居中大标题 + accent 按钮）
footer（三列 + 版权 + ▸ 标记）
```

每节以 mono 小节标签开头（如 `01 / features`），形成"文档感"——与竞品营销页最大的差异化。

## 4. 组件清单与规格

### 4.1 nav

- sticky，背景 `--bg`（95% 不透明即可，**不做毛玻璃**），底 1px hairline；高度 56px。
- 左：字标 `▸ bingo`；右：栏目链接（Features / How it works / Quick start / Docs / Contributing，sans 14px，hover accent）+ 主 CTA「Install」（accent 实底）。
- 移动端：链接收进 `<details>` 折叠菜单（**原生可折叠，零 JS**）或 hamburger + aria-expanded。
- `scroll-padding-top` 对齐锚点；`scroll-behavior: smooth`（reduced-motion → auto）。

### 4.2 hero

- 背景：纯 `--bg-sunken` + **一处**低透明度 radial 光晕（`radial-gradient(60% 50% at 70% 0%, rgba(215,119,87,.08), transparent)`）——只此一处渐变，其余禁止。
- 可叠加 12px 等宽网点网格（双 `linear-gradient`，`rgba(255,255,255,.03)`）或纯色，**二选一**。
- H1：PRD 锚点句，mono 大字号；关键词着色只给 `harness` 或 `intent` 之一（accent），**一个都不许多**。
- 副标题：sans 18px `--dim`，单段 ≤ 2 行（说明 harness 心智模型，不重复 H1）。
- 安装命令：`$ cargo install --git https://github.com/yexrob/bingo --locked`（mono 代码块 + 复制按钮）+ 双 CTA（GitHub / Quick start）。
- 底部小字（可选）：`Rust · runs locally · your key never leaves the machine`（mono 12px faint-strong）。

### 4.3 终端演示 mockup（核心组件，全站视觉担当）

**规格**（纯 HTML/CSS，零图片零依赖；零 JS 优先，页签可用 `<details>` 或纯静态双窗并排）：

```
┌─────────────────────────────────────────────┐
│ ● ● ●    bingo · share        mode: plan   │  ← 窗口头：三点 + 标题 + 状态
├─────────────────────────────────────────────┤
│ ▸ user   Design the export contract…        │  ← 消息流：gutter 菱形 + 行头
│ ∴ thinking · 88 tokens                      │  ← 折叠态摘要（灰斜体）
│ ⚙ Bash · git status   ✓ 0.3s                │  ← 工具行（teal + green）
│ ▸ assistant  …列表…                          │
│ ▾ code rust（语言标签）                       │
│ ▸ _                                         │  ← 闪烁光标（CSS steps，静态降级）
└─────────────────────────────────────────────┘
```

- 窗口：`--bg-elev` + 1px `--hairline` + `--radius`，无阴影（或仅 1 层 24px/4% 深色柔影，二选一）。
- 消息行与 share 页对话视图**同构**（gutter 线 + 菱形 + 角色色 + 折叠工具行）——「官网上展示的就是产品里长这样」。
- 光标：`▸` + 方块 `_`，`steps(1)` 1s 循环；`prefers-reduced-motion` 下静态。
- 多视图切换（如 Team/Channels 演示）：优先 `<details>` 页签（零 JS）；如需滑动切换，JS 增强仅负责 `open` 互斥，30 行内。
- 移动端：mockup 允许横向滚动，或降级为只显示首屏 6 行。

### 4.4 Features（8–10 张特性卡）

- 网格：≥1024px 三列、640–1023px 两列、以下单列；`gap: 1px` + 容器 hairline 边框（**棋盘式平铺**，不靠阴影卡片）。
- 每格：mono 编号（`01 /`，accent）+ 标题（sans 600 17px）+ 描述（sans 15px dim，1–2 行）+ 一个具体行为示例（mono 12px faint-strong，如 `bingo --print '…'`）。
- 候选卡面（PRD §2 所列，取 8–10）：streaming loop · permission gate · tool trait · sub-agents · serial channels · slash commands · hooks · experience reuse · MCP · skills。
- hover：`--bg-hover` 过渡 120ms；编号色不变。

### 4.5 How it works（harness 心智模型）

- 一段话讲透：**"the model produces intent; the local harness gates every side effect"**——权限、并行、副作用、压缩、记忆与 UI 都由本地 harness 负责。
- 配图：**文本/SVG 简图**（无 JS）：`model ⇄ intent ⇄ harness → [permission gate / tools / hooks / memory]`，等宽排版 + hairline 连线；SVG 内联且 `aria-hidden`，文字内容在正文重复一遍。
- 禁止：流程图插画、动画连线、3D 模型。

### 4.6 Quick start

- 3 步（等宽编号 `1 /` `2 /` `3 /`）：configure API key → `bingo` → first prompt。
- 附 `bingo --print '…'` headless 示例代码块（`$` accent 前缀 + 复制按钮）。
- 底部链接「Read the README」（指向 GitHub）。

### 4.7 Share 样例（/share/，P1）

- **页面形态**：独立页 `/share/index.html`，**直接使用 `share-page-design.md` 的模板**（`share-page-template.html`，浅色文档风、四视图、零依赖、离线可用）——令牌天然同源，满足 pm 验收 A5「复用 share-page-design.md 令牌」。
- **内容**：用模板自带真实感样例数据（对话 + Team 名册 + 私聊 + 频道活动，真实 bingo 工作流，不编造营销话术）；等 CLI 团队产出真实 `bingo share` 文件后**整体替换**（同名文件覆盖，不改站点布局）。
- **接入**：站点导航加「Examples」链接指向 `/share/`；站内不 iframe 不嵌套（模板是完整 HTML 文档，嵌套会破坏样式）——独立页 + 链接即可。
- **风格关系**：暗色站点中打开的是一份浅色文档页，这是有意的对比（产品输出物 vs 营销门面），不做任何混搭适配。

### 4.8 CTA 区

- 居中：mono 大标题（≤ 12 词）+ 一句副文案 + 单个 accent 实底大按钮（padding 12px 28px，如 `cargo install bingo`）。
- 背景与 hero 相同（纯色 + 可复用一处光晕），首尾呼应。
- hover：提亮不位移（不做弹跳）。

### 4.9 Docs / Contributing（索引列）

- 两栏（或并入 footer 上方一栏）：Docs → GitHub README / README.zh-CN / design notes；Contributing → 理念（Rust 2024、no unsafe、做减法）+ worktree 工作流 + `cargo build/clippy/test` 验证命令。
- 样式：链接列表（sans 14px dim，hover accent）+ 每项一行 mono 说明。
- **不在站内复制全文**，只做索引。

### 4.10 footer

- 顶 1px hairline；`--bg`。
- 左：字标 + 一句话（mono 12px faint）；中：链接列（GitHub / crates.io / Changelog / License）；右：版权 + 版本号。
- 最下行：`Rust · MIT · zero telemetry`（mono 12px faint-strong，`zero telemetry` 可 accent）。

### 4.11 通用状态

| 组件 | hover | focus-visible | disabled |
|---|---|---|---|
| 按钮 | 亮度/边框变化 120ms | `outline:2px solid var(--accent); offset:2px` | `--faint` + `cursor:not-allowed` |
| 链接 | accent + 下划线加粗 | 同上 | — |
| 复制按钮 | 显现（opacity 120ms） | 同上 | — |
| 代码块 | 无 | 同上 | — |

## 5. 动效（全部可选增强，核心零 JS）

| 场景 | 动效 | 参数 | JS? |
|---|---|---|---|
| 锚点滚动 | smooth scroll | CSS，reduced-motion → auto | 否 |
| 滚动进入视口（可选） | 淡入 + 上移 | opacity + translateY(12px)，300ms，仅一次 | 是（IntersectionObserver，可选） |
| mockup 页签切换 | 淡入（若做） | 300ms | 是（可选） |
| 光标闪烁 | steps 闪烁 | 1s 循环，仅 mockup | 否（CSS） |
| hover | 颜色/边框 | 120ms | 否 |

- 无 JS 时：无 reveal、无页签切换（用 `<details>` 或静态并排）、无滚动特效——**页面依然完整**。
- 拒绝：视差、横向滚动大图、marquee、按钮弹跳、打字机逐字、无限循环。

## 6. 响应式

| 断点 | 行为 |
|---|---|
| ≥1024px | 三列特性、mockup 完整、nav 全链接 |
| 640–1023px | 两列特性、nav 链接收敛（details 折叠）、hero 字号降档 |
| <640px | 单列；hero 留白 48px；mockup 横向滚动或截断；CTA 按钮全宽 |

## 7. 可访问性

- 对比度：§2 表全部达标（正文 17.6:1；faint 仅大字号与装饰）。
- 语义：`header/nav/main/section/footer`；mockup 是 `<pre>`/`<figure>` + `aria-label`，不用截图替代文本；页签若做用 `role=tablist`，用 `<details>` 则原生语义。
- 键盘：nav 链接、复制按钮全可达；focus 环统一。
- 非颜色通道：状态符号伴随颜色；链接一律下划线。
- 文案：CTA 动词开头（Install / Read / View）；装饰字符（`▸ ∴ ⚙`）`aria-hidden` 或 CSS 伪元素承载。

## 8. 参考气质（气质参照，非照抄）

| 参考 | 借鉴什么 | 不学什么 |
|---|---|---|
| Claude Code 官网/文档 | 近黑底 + 陶土橙的克制暗色、终端 mockup 的"产品即页面" | 玻璃拟态按钮与复杂光效 |
| Warp 官网 | 终端 mockup 作为 hero 主角、命令区即内容 | 霓虹渐变 |
| Stripe 官网 | 排版层级与留白、一屏一重点 | 插画体系与繁复动效 |
| Ghostty 官网 | 纯色、等宽、克制得近乎冷淡的自信 | — |
| 本项目 share 页 | 消息行/折叠/色板同构 | — |

一句话：**如果官网上出现一个不该出现的渐变，就删掉。**

## 9. Do / Don't

**Do**
- 高亮只用于：CTA、字标前缀、角色/状态符号、hover。
- 一屏一重点；区块间 96px 留白而不是分隔条。
- 所有终端元素（mockup、代码、命令）用等宽，其余用无衬线。
- 特性/演示内容用真实 bingo 输出（真实消息流、真实频道 seq），不编造华丽假数据。

**Don't**
- 不用玻璃拟态、毛玻璃、大渐变、发光、霓虹、粒子。
- 不堆卡片（卡片之间必有 1px 网格线连接，不是悬空圆角块）。
- 不做无意义动效；不让核心内容依赖 JS。
- 不用插画/emoji 当图标（符号用 `▸ ∴ ⚙ ✓ ● ◐ ◇` 等字体符号）。
- 不在等宽标题里塞超过 12 个词的句子（PRD 锚点句是唯一例外，且给足行高）。

## 10. 交付物建议（供官网开发/编排参考）

1. 本文 token 表转 `tokens.css`（或单文件 `:root`）。
2. 组件顺序：nav → hero + 终端 mockup → features → how it works → quick start → share 预留 → CTA → footer。
3. mockup 与 share 页共用"消息行"样式心智，未来在 repo 内维护同一套语义令牌。
4. 静态构建：仓库直接是静态文件（PRD §4），`public/` 映射站点根；`/share/` 目录预留。
5. 零 JS 优先：先做无 JS 完整版，再按需加 reveal/页签增强（均带降级）。

## 11. 变更记录

| 日期 | 版本 | 说明 |
|---|---|---|
| 2026-08-07 | v1.2 | 对比度修补：新增 `--faint-strong`（定稿 `#7A7A80`，~4.6:1 ≥ AA）用于信息性小字（特性示例/元信息/脚注，§4.2/§4.4/§4.10 同步更新）；`--faint` 收窄为纯装饰用途，禁止承载任何文字 |
| 2026-08-07 | v1.3 | /share/ 样例页提级 P1：规格明确 = 直接复用 share-page-template.html（同令牌、独立页、可整体替换） |
| 2026-08-07 | v1.2 | 澄清与 share 页 v2.0 的关系：官网暗色终端风 / share 页浅色文档风，共享语义色值而非表面配色 |
| 2026-08-07 | v1.1 | 对齐 prd-site.md v1：英文主站 + /zh/ 单页、8 栏目结构（How it works / Quick start / Share 预留 / Docs / Contributing）、8–10 特性卡、零 JS 优先、/share/ 路径预留 |
| 2026-08-07 | v1.0 | 初稿：token 表 + 组件清单 + 参考气质 |
