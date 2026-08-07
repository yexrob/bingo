# bingo share 页面设计（v3.0 · opencode 完全复刻）

> 版本：v3.0 · 状态：定稿（**唯一事实源 = `share-page-template.html` v3.0**，本文件为摘要与契约说明）
> 定位：`bingo share` 子命令导出的自包含 HTML 分享页。**完全复刻 sst/opencode share 页面**（不再只是"参考风格"）：CSS 原样移植、结构用 opencode 同款 `data-component`/`data-slot` 属性、零外部依赖、离线可用、打印友好。
> 参考源：`opencode-source/`（share.module.css / part.module.css / content-*.module.css / copy-button.module.css / starlight-props.css / part.tsx / content-*.tsx / common.tsx / custom.css 覆盖）。

## 0. 原则

1. **复刻 = 移植**：CSS 规则原样保留（仅做模块命名空间化），DOM 结构与 opencode TSX 输出逐一对齐——dev 的 Rust 生成器按模板的结构契约输出，浏览器看到的就是 opencode 的 share 页面。
2. **零依赖落地**：opencode 用 shiki/marked 做高亮与 markdown——bingo 不引入外部库：代码块输出**纯 `<pre>`**（CSS 已保证观感），markdown 由 Rust 端输出安全 HTML 子集（结构见 §3.3），语法高亮作为 P2（未来可用服务端生成着色 span）。
3. **渐进增强**：锚点复制链接、复制按钮、结果展开、回到顶部、tab 切换均为 JS 增强；无 JS 时页面完整可读（折叠内容默认展开、锚点跳转原生可用、`.copy-root` hover 显现）。
4. **四视图保持（三视图为聊天记录形态）**：对话视图 = opencode 消息部件流；**Team = 线程列表**（会话列表心智）、**私聊 = 每代理聊天记录流**、**频道 = 消息流**——三个视图全部是聊天形态，无一表格/名册；bingo 数据语义（成员色/状态/seq/徽标）保留。

## 1. 复刻要点（模板实现对照）

| 层 | 复刻内容 | 模板实现 |
|---|---|---|
| 令牌 | Starlight `--sl-color-*`（浅/深两套）+ opencode custom.css 覆盖（`--color-background/weak/strong`、`--color-text/weak/weaker/strong`、`--color-border/weak`、`--color-icon`）+ share 组件专属（`--sl-color-bg-surface`、`--sl-color-divider`、`--sm/md/lg-tool-width`、`--term-icon`） | `:root` 浅色默认 + `@media (prefers-color-scheme: dark)` 深色（对齐 custom.css body 覆盖）；补 `--sl-color-text-secondary/dimmed`、`--sl-color-border` 别名 |
| 布局 | 头部（header-title 2.75rem 大标题 + header-stats 统计列表 + header-time）+ 消息流（parts 纵向 0.625rem 间距） | `[data-component="header"]` / `[data-component="parts"]` |
| 消息行 | 装饰列（18px 锚点：角色图标→hover 变 # 号→复制后变 ✓ + tooltip「Copied」；3px 贯穿竖线 `--sl-color-hairline`）+ 内容列（限宽） | `[data-component="decoration"]`（`[data-slot="anchor"]` 内 3 个 SVG + `[data-slot="tooltip"]`）+ `[data-slot="bar"]` + `[data-component="content"]` |
| 部件 | user 无框（`user-text` + content-text bg-surface 块）/ assistant 蓝框卡（`assistant-text > assistant-text-markdown` 1px `--sl-color-blue-high` + 4px 圆角 + 0.5rem）/ thinking（tool-title「Thinking」+ `assistant-reasoning` 同款蓝框小卡）/ step-start（provider 大写 + model）/ tool 两段式（`tool-title` 大写名 + 等宽目标 + `tool-args` 网格 + `tool-result` 可展开）/ 错误（`content-error` 红 label + dimmed `[line:col]`）/ bash（`content-bash` 终端窗：三点头「Shell」+ command + output 限 10 行）/ markdown（`content-markdown` 3 行折叠）/ 代码块（`content-code`）/ todos（`data-slot="item"` 状态点） | 全部部件 data-component/data-slot 与 opencode 一致；模块根类 `.part-root/.cm-root/.ct-root/.cc-root/.ce-root/.cb-root/.copy-root` |
| 复制按钮 | hover 显现、点击复制、2s 变绿 ✓（copy-button.tsx） | `.copy-root` + `*:hover > .copy-root` 显现规则 |
| 回到顶部 | 固定右下 2.5rem 按钮（scroll-button） | `.scroll-button`，滚动 >200px 显现 |
| 四视图适配（**全部聊天记录形态**） | **Team** = 线程列表（每成员 part 行：成员色圆点装饰列 + step-start 大写名/状态/def + 最近消息预览 + footer；整行 `data-jump` 直达私聊，锚点复制 `#dm-<agent>`）——选型理由：会话列表 + 详情两级导航是聊天应用标准心智，且完整保留名册语义（def/description/state/消息数）；聚合全员流会丢失名册信息并与对话视图混淆，故不选。**私聊** = 每代理一个 part 块（装饰列 + step-start 头 + `dm-thread` 消息流：`dm-msg` 内 tool-title 风格发送者大写成员色 + content-text，user 靠右，工具调用折叠同对话部件）。**频道** = 每频道 part 流（step-start 头：`◇ #name` + mode chip + 成员 chips；每条消息 = part：装饰列 + tool-title 发送者大写成员色 + `ch-row-seq` 目标位 + content-text，user 右对齐） | 全部复用 opencode 部件语言（part-root/decoration/anchor/bar/step-start/tool-title/content-text），令牌全走 `--sl-color-*`；成员色 `--hue-N` 保留 |
| 成员色 | bingo 语义保留：`--hue-0..5`（浅色深档 / 暗色浅档两套），main=text-strong、user=text-strong，其余 hash 取色 | `--from/--chip/--dot` 变量注入 |

## 2. 模板文件信息

- **路径**：`notes/design/share-page-template.html`（worktree feat/share）
- **MD5**：`c626cdfb6844c58aabcd5415f30fd4d6`（v3.1 定稿；v3.0 `09e59e72` / v2.2 `c4a781c5` 已被取代）
- **规模**：~84KB / ~1500 行（CSS ~1100 行原样移植 + 样例 ~300 行 + JS ~150 行渐进增强）
- **验证**：headless Chrome 实测（10 parts、12 锚点、7 复制按钮、4 视图、bash Shell 头、screenshot 161KB）；`share-review.js` v3.0 = **47/47 PASS**；零外部引用

## 3. dev 集成契约（Rust 端生成规则）

### 3.1 结构与转义

- 输出 `<html lang="en">` + 本模板 `<style>` 原样内联（CSS 是唯一需要整段搬移的部分）；JS 段可整体内联（渐进增强，不拼接任何数据）。
- **所有动态文本全量转义**（`& < > " '`）；代码/输出放 `<pre>` 内；图片仅 `data:` URI（media_type 校验 `image/*`）。
- 数据由 Rust 服务端渲染，JS 不接触数据——无脚本注入面（PRD C 组）。

### 3.2 消息部件映射（对话视图）

| bingo 内容块 | 输出结构（模板照抄） |
|---|---|
| user 文本 | `<div class="part-root" data-component="part" data-type="text" data-role="user" id="msg-N">` → `[data-component="decoration"]`(anchor 3 SVG + tooltip + bar) → `[data-component="content"]` → `[data-component="user-text"]` → `div.ct-root[data-component="content-text"]` > `pre[data-slot="text"]` + copy-button |
| assistant 文本 | 同上 data-role="assistant" → `[data-component="assistant-text"]` > `[data-component="assistant-text-markdown"]` > `div.cm-root[data-component="content-markdown"]` > `[data-slot="markdown"]`（Rust markdown→HTML 子集）+ copy-button；末尾可选 `[data-component="content-footer"]`（时间） |
| thinking | `data-type="reasoning"` → `[data-component="tool"]` > `[data-component="tool-title"]`（name=Thinking）+ `[data-component="assistant-reasoning"]`（button-text data-more + `[data-component="assistant-reasoning-markdown"]` > cm-root） |
| step-start（可选，会话头） | `data-type="step-start"` → `[data-component="step-start"]` > `[data-slot="provider"]`（大写 provider）+ `[data-slot="model"]` |
| tool bash | `data-type="tool" data-tool="bash"` → `div.cb-root[data-component="content-bash"]` > `[data-slot="body"]`（`[data-slot="header"]`=Shell + `[data-slot="content"]`：command `pre` + `[data-slot="output"]` div>pre）+ copy-button；**A4 契约**：bash input 中**非 command 字段**（background/timeout 等）渲染为 `[data-component="tool-args"]` 网格（键/值完整呈现，flat 值直出、嵌套 JSON 序列化，不截断）——无额外字段时省略该块（对齐 opencode 常见形态） |
| tool read/write/fetch | `data-tool="read|write|webfetch"` → `[data-component="tool-title"]`（name 大写 + `[data-slot="target"]` 路径/URL）+ `[data-component="tool-result"]`（button-text data-more + `div.cc-root[data-component="content-code"]` > pre + copy-button） |
| tool grep/glob/list | 同上，结果用 `ct-root`（data-compact）+ button |
| 未知工具 fallback | `data-tool="<name>"` → `[data-component="tool-title"]` + `[data-component="tool-args"]`（每参数 3 格：分隔条/键/值，值超 60 字符截断）+ `[data-component="tool-result"]` |
| tool 错误 | `data-tool="error"` → `div.ce-root[data-component="content-error"]` > `[data-section="content"]` > `pre`：`<span data-color="red" data-marker="label">Error</span>` + 可选 `<span data-color="dimmed">[line:col]</span>` + 消息 |
| 图片 | `data-type="image"` 可选：`<img src="data:{media_type};base64,{data}">`（alt 必填） |

### 3.3 markdown 输出规范（`[data-slot="markdown"]` 内）

Rust 端输出安全 HTML 子集，样式已内置：`p / ul,ol,li / h1-h6 / strong / code（行内加反引号装饰）/ pre（代码块）/ table,th,td / blockquote / hr / a`。链接仅 http/https/mailto 放行并 `target="_blank"`。不做高亮（纯 pre），不做外部字体。

### 3.4 四视图（非对话 · 聊天记录形态）

- 视图容器：`<section class="view" data-view="team|dm|channel" hidden>` + `h2[data-component="view-title"]`；空态 = `<div class="view-empty">— No … —</div>`。
- **Team**：`div.thread-list > div.part-root.thread-row[data-agent][data-jump="#dm-<agent>"][tabindex][role="link"]`：装饰列锚点（首 SVG = 成员色圆点 `fill="currentColor"` + `style="color:var(--hue-N)"`，href 指向私聊锚点）+ `[data-component="step-start"]`（`[data-slot="provider"]` 大写成员名 + `[data-slot="model"]` 状态 · def · 消息数）+ `ct-root[data-compact]` 最近消息预览 + `[data-component="content-footer"]`。整行点击/Enter 经 JS `data-jump` 直达对应私聊（锚点内点击交给复制逻辑）。
- **DM**：`div.part-root[data-type="thread"][data-agent][id="dm-<agent>"]`：装饰列 + `[data-component="step-start"]`（provider 大写成员名 / model 状态 · def）+ `div.dm-thread > div.dm-msg`：`[data-component="tool-title"]`（`[data-slot="name"]` 大写成员色发送者）+ `ct-root` 消息体（`dm-user` 类右对齐）；线程内工具调用复用 `[data-component="tool"]`（bash/read 等折叠部件）。
- **Channels**：`section.ch-block[data-component="channel"][id="channel-<name>"]`：`header.ch-head[data-component="step-start"]`（provider = `◇ #name` / model = mode chip + 成员 chips）+ `div.ch-stream > div.part-root[data-from][id="ch-N"]`：装饰列 + `[data-component="tool-title"]`（`[data-slot="name"]` 大写成员色发送者 + `[data-slot="target"].ch-row-seq` 四位 seq）+ `ct-root` 消息体（`ch-user` 类右对齐）。

## 4. 评审方法（qa / uiux 引用）

- **脚本**：`notes/design/share-review.js` v3.0——**47 项断言**（令牌/结构/部件/四视图/语言/转义/自包含/a11y/打印/交互契约），模板自检 **47/47 PASS**；全过退出码 0。
- **渲染复核**：headless Chrome dump-dom 断言（parts/anchors/copy-buttons/views 计数）+ screenshot 目检。
- 对照基准 = `share-page-template.html` v3.1（MD5 `c626cdfb…`）。

## 5. 变更记录

| 日期 | 版本 | 说明 |
|---|---|---|
| 2026-08-07 | v3.1.1 | A4 数据完整性契约补充：bash 非 command 字段 → `tool-args` 网格完整呈现（pm #27 回归发现，模板样例已落地；模板 MD5 `e79b37aa`） |
| 2026-08-07 | v3.1 | 补充要求并入：Team/私聊/频道三视图全部改为**聊天记录形态**——Team = 线程列表（会话列表心智，data-jump 直达私聊）、私聊 = 每代理 part 聊天流（dm-msg，user 靠右，工具折叠）、频道 = part 消息流（seq/成员徽标保留，user 右对齐）；旧 roster/dm-agent/ch-row 列表样式移除；评审脚本升级 51 项；模板 MD5 `c626cdfb` |
| 2026-08-07 | v3.0 | **opencode share 完全复刻**：CSS 原样移植（starlight + custom 覆盖 + share/part/content/copy 模块，命名空间化）+ data-component/data-slot 结构 + 部件全集 + 四视图适配 + 渐进增强 JS；评审脚本 47 项；模板定稿 MD5 `09e59e72` |
| 2026-08-07 | v2.3 | 模板定稿 MD5 c4a781c5；评审记录 D1/D3/D4（已被 v3.0 取代） |
| 2026-08-07 | v2.2 | a11y 修正：aria-hidden 移至 .line（已被 v3.0 取代） |
| 2026-08-07 | v2.0 | opencode 参考风格改版（浅色文档底，已被 v3.0 取代） |
