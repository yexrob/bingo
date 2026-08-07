# bingo share 页面设计（v4.0 · Claude Code app 风格）

> 版本：v4.0 · 状态：定稿（**唯一事实源 = `share-page-template.html` v4.0**，本文件为摘要与契约说明）
> 定位：`bingo share` 子命令导出的自包含 HTML 分享页。**展现形式参考 Claude Code app（claude.ai/code 桌面应用）**（用户指定方向，替代 v3.x 的 opencode 复刻）。
> 参考：GitHub issues #48158/#51069 界面描述 + Claude 设计语言 + bingo 既有品牌语义（`--accent:#D77757` 陶土橙同源）。

## 0. 设计原则

1. **聊天应用观感**：暗色近黑底、消息流居中限宽（800px）、用户右侧暖灰气泡、助手左侧 markdown 流——像一份 Claude Code app 的会话快照，而非终端截图或文档页。
2. **品牌克制**：陶土橙 `#D77757` 只用于品牌时刻（字标前缀、链接、工具图标、hover、选区）；状态语义用绿/红/橙徽标，不喧宾夺主。
3. **事实完整**：工具输入 JSON 与结果原样呈现、不截断（PRD A4）；bash 非 command 字段走 `tool-args` 网格。
4. **无 JS 核心**：数据由 Rust 服务端渲染（全量转义），JS 仅做渐进增强（tab/锚点复制/复制按钮/线程跳转/打印展开）；无 JS 时页面完整可读（折叠内容默认收起但 `<details>` 原生可开）。
5. **四视图统一**：对话 / Team 线程列表 / 私聊聊天流 / 频道消息流，全部此风格；Team 保持线程列表形态（成员最近消息预览 + 直达私聊）。

## 1. 视觉锚点

| 锚点 | 决策 |
|---|---|
| 底色 | 近黑 `#0D0D0F`；表面 `#151518`（工具卡）；代码块 `#1B1B20`；用户气泡暖灰 `#3A3731` |
| 消息流 | 居中限宽 `--maxw: 800px`；用户气泡 `--bubble-max: 72%` |
| 用户消息 | **右侧气泡**：暖灰底、圆角 14px（内角 4px）、`You · 时间` 元信息右对齐 |
| 助手消息 | **左侧 markdown 流**：无气泡，正文/代码块对比清晰（行内代码橙 tint `#E8B08F`） |
| 工具调用 | **折叠卡**：图标 + 工具名 + 参数摘要 + **状态徽标**（`✓ done` 绿 / `✗ error` 红 / `◐ running` 橙 + 时长），展开看完整 input/output |
| thinking | 折叠块：灰色斜体摘要（`∴ Thinking · 88 tokens`），正文灰斜体 |
| 顶栏 | sticky：品牌 `▸ bingo` + 会话标题 + 元信息（项目/模型/时间/模式）+ 四视图 tabs |
| 字体 | 正文系统无衬线；代码/工具名/seq/元信息等宽 |
| 品牌 | 陶土橙 `#D77757` 克制使用；状态绿 `#4EBA65` / 红 `#FF6B80` / 进行中橙 `#F0A05A` |

## 2. 令牌（摘要，完整见模板 `:root`）

```css
:root {
  --bg:#0D0D0F; --surface:#151518; --surface-2:#1B1B20;
  --bubble:#3A3731; --bubble-text:#F2F0EC;
  --hairline:#26262B; --hairline-strong:#36363D;
  --text:#EDEBE7; --dim:#A8A39B; --faint:#77726A; --ink:#0D0D0F;
  --accent:#D77757; --green:#4EBA65; --red:#FF6B80; --gold:#FFC107; --running:#F0A05A;
  --hue-0..5（暗色档成员色）;
  --maxw:800px; --bubble-max:72%;
}
```
对比度：正文 15.9:1、dim 7.2:1、accent 6.2:1、语义色全 ≥4.5:1（打印模式另行换算）。

## 3. 四视图（全部聊天形态）

| 视图 | 形态 |
|---|---|
| **Conversation** | 消息流：用户右气泡（`.msg-user > .bubble`）/ 助手左 markdown（`.msg-assistant > .content > .md`）+ thinking 折叠 + 工具折叠卡 |
| **Team** | 线程列表（`.thread-list > .thread`）：圆形头像（成员色）+ 名 + 状态（●◐✗）+ 消息数 + 最近消息预览 + footer（时间 · def）；整行 `data-jump` 直达私聊 |
| **DM** | 每代理一个聊天流（`.dm-block`）：头部（头像 + 名 + 状态 + def）+ `.dm-flow` 消息流（代理左 / 用户右气泡，发送者成员色） |
| **Channels** | 每频道消息流（`.ch-block`）：头部（`◇ #name` + mode chip + 成员 chips）+ `.ch-flow` 消息行（seq + 发送者成员色 + 文本，user 右对齐） |

空态：`— No … —`（`.view-empty`），四视图恒存在。

## 4. 交互（渐进增强）

tab 切换（hash + 1-4 键）、消息锚点复制 `URL#msg-N`（hover 显现 `#`，点击复制变 ✓）、复制按钮（JS 创建于 .code-block/.t-code）、线程行直达私聊、打印展开全部 + 全视图、reduced-motion 全关。

## 5. dev 集成契约（Rust 端生成规则）

- 输出 `<html lang="en">` + 模板 `<style>` 整段内联 + `<script>` 整段内联（JS 不拼接数据）。
- 全量转义（`& < > " '`）；代码放 `<pre>`；图片仅 `data:` URI。
- **消息部件映射**：
  - user：`<article class="msg msg-user" id="msg-N">` > `.msg-meta`（`who=You` + time + anchor `#`）+ `.bubble`（纯文本）
  - assistant：`.msg-assistant` > `.msg-meta`（who=Assistant + model + time + anchor）+ `.content`：`.md`（markdown HTML 子集）+ `details.think`（summary=Thinking · N tokens）+ `details.tool`（summary：`.t-icon` svg + `.t-name` + `.t-args` 摘要 + `.t-status.ok|.err|.running` 徽标；body：`.t-code`（input/result pre））
  - bash A4：非 command 字段 → `[data-component="tool-args"]` 网格（或 v4 用 `.t-args` 摘要 + 完整 input pre 保留——**以模板最终实现为准**，input pre 始终含完整 JSON）
  - Team/DM/Channels 结构见 §3 与模板样例。
- markdown 子集：p/ul,ol/h1-h6/strong/em/code/pre/table/blockquote/hr/a——样式内置。

## 6. 评审方法

`share-review.js` v4.0：**43 项断言**（令牌/结构/部件/四视图/语言/转义/自包含/a11y/打印/布局契约），模板自检 **43/43 PASS**；headless Chrome DOM 复核（复制按钮数 = .code-block + .t-code 数、气泡/工具卡/线程/消息计数、tab 行为）。

## 7. 变更记录

| 日期 | 版本 | 说明 |
|---|---|---|
| 2026-08-07 | v4.0 | **Claude Code app 风格**（用户指定，替代 opencode 复刻）：暗色近黑底 + 居中 800px 消息流 + 用户右暖灰气泡 + 助手左 markdown + 工具折叠卡状态徽标 + thinking 灰斜体 + sticky 顶栏 + 四视图聊天形态（Team 线程列表保持）；评审脚本 43 项；模板 MD5 `8c29a17b` |
| 2026-08-07 | v3.1.1 | opencode 复刻 + A4 契约（已被 v4.0 取代） |
| 2026-08-07 | v3.1/v3.0 | opencode 完全复刻 + 三视图聊天记录形态（已被 v4.0 取代） |
