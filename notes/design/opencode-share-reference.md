# opencode share 页面样式参考（源码提取）

> 来源：sst/opencode 仓库 `packages/web/src/components/share/`（part.tsx / part.module.css / content-*.tsx）。
> 用途：bingo share 页面样式对齐参考（用户指定）。已按重要度提取，未照搬代码。

## 1. 整体气质

- **浅色为主**（Starlight 文档站配色体系 `--sl-color-*`），文档感、极简、克制：无气泡堆砌、无阴影、无渐变、无装饰动画。
- 信息密度中等：字号 0.875rem（正文）/ 0.75rem（元信息），行高宽松，区块间距 1rem。
- 语义色只用于状态与强调：`green-high`（成功/复制完成）、`blue-high`（助手消息框、思考框）、`red`（错误）、`text-secondary/dimmed`（次级文字）、`hairline`（分隔线）。

## 2. 布局骨架：左装饰列 + 内容限宽

每条消息部件是横向 flex：`装饰列（decoration）| 内容（content）`。

- **装饰列**（flex 0 0 auto，宽约 18px）：
  - 顶部一个**锚点图标**（18px，悬停变链环图标，点击复制「当前页 URL + #消息id」；复制成功短暂显示对勾 + tooltip）。
  - 图标下一条**3px 竖线**（hairline 色、圆角 1px）**贯穿整条消息**——视觉上把消息串成时间线。
- **内容列**（flex 1）：
  - 按内容类型**限宽**：`--sm-tool-width`（bash/read/list/glob/grep/write 工具结果）、`--md-tool-width`（正文、思考、错误）、`--lg-tool-width`（edit diff）——内容不铺满全宽，窄列更聚焦。
  - 内容底部留 1rem 间距（消息间分隔靠竖线 + 间距，不用卡片隔断）。

## 3. 各消息部件样式

| 部件 | 呈现 |
|---|---|
| **用户文本** | 纯文本，**无气泡无边框**，跟随左装饰列 |
| **助手文本** | `border: 1px solid blue-high`（浅蓝细框）+ `padding: 0.5rem` + `border-radius: 0.25rem`（4px 小圆角），字号 0.875rem |
| **思考（reasoning）** | 与助手文本同款：浅蓝细框小卡，标题行 "Thinking"（secondary 色），正文 0.75rem；「Show details」按钮展开 |
| **step-start（模型切换标记）** | provider 名**大写 + 字母间距 -0.5px**（secondary 色）+ 模型名 |
| **工具调用（tool）** | 两段式：① `tool-title` 行 = 工具名（Bash/Grep/Read/Write/Edit/List/Glob/Fetch/Task…）+ 目标参数（`"pattern"`、文件路径、命令），secondary 色、0.875rem、行高 18px；② `tool-result` = 结果块（纯文本预览 / 代码块），带「Show details」展开 |
| **工具错误** | `<pre>` + 红色 `Error:` 标记 + 原文；诊断错误带 `[line:col]` 前缀（dimmed） |
| **附件（file）** | 小标题行（ATTACHMENT，大写 secondary 色）+ 文件名（500 字重） |
| **bash 输出** | 等宽、`--sm-tool-width` 限宽、深色块（代码块组件） |
| **todo（todowrite）** | 标题行 + 列表，每项带状态色点（in_progress/pending/completed 分组排序） |

## 4. 可迁移到 bingo share 的要点（建议清单）

1. **布局**：消息行 = 左装饰列（锚点图标 + 贯穿竖线）+ 内容列；内容按类型限宽（sm/md/lg 三档）。
2. **消息锚点**：每条消息 id + 点击复制链接（渐进增强 JS；无 JS 时锚点跳转仍可用）。
3. **助手消息**：浅色细边框卡（1px + 4px 圆角 + 0.5rem padding），不用气泡。
4. **用户消息**：无框纯文本。
5. **工具调用**：标题行（工具名 + 目标参数摘要）+ 结果块（可展开），不用大 JSON 卡片糊满。
6. **模型/角色元信息**：小号大写字母行（secondary 色）。
7. **配色**：浅色底 + hairline 分隔 + 单一强调蓝/橙 + 语义绿/红；全站 `--sl-color-*` 式令牌。
8. **克制**：无阴影、无渐变、无卡片堆砌；分隔靠竖线、hairline 与留白。

## 5. 与 bingo 现有设计（v1.1 草案）的差异

| 维度 | 现有草案（终端暗色） | opencode 参考 |
|---|---|---|
| 底色 | 暗色 #0C0C0E | 浅色文档底 |
| 消息呈现 | 行头菱形标记 + gutter 竖线 | 左装饰列（图标锚点+竖线）+ 限宽内容 |
| 助手消息 | 无底 | 浅蓝细边框卡 |
| 工具 | details 折叠块（⚙ 名称·参数·状态） | 标题行 + 结果块两段式 |
| 字体 | 全页等宽 | 无衬线正文 + 等宽代码 |
| 模型元信息 | 无 | step-start 大写行 |

> 对齐建议：保留 bingo 品牌色（accent 橙可替代 blue-high 作强调色），布局/信息结构/克制感全面对齐 opencode 参考；暗/浅主题以参考页面的浅色为默认（品牌一致性优先于终端血统，终端血统由等宽代码块与工具行保留）。
