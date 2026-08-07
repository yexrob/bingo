# 验收报告：`bingo share` 子命令（feat/share @ 3ad029d）

> 验收责任人：pm-cli（CLI 团队）· 日期：2025-08-07
> 依据：notes/design/prd-share.md v1.1（7 组 + V 组）；设计文档 v2.0/§10.1 与模板（唯一事实源）
> 样本：/tmp/bingo-share-acc/{inject,legacy,badlines,e2,empty}（构造注入/旧会话/坏行/缺字段/空会话）
> 提交：6323846（feat）+ 4c98dcc（style v2.0）+ 3ad029d（test+C1 全字符集）

## 结论

**通过（无遗留）**。A/B/C/D/E/F 六组 + 抽查 41 项断言全过（初跑 3 项 FAIL 均判定为断言缺陷，修正后全过，见 §备注）；G1 三连独立验证全绿（build/clippy/test 607 passed，基线 aec857f，日志 /tmp/bingo-share-acc/g1.log）；V 组 uiux 正式放行并全闭环——32→34 项评审断言全过（模板事实源 c4a781c5 完全一致）、DOM 复核（441 锚点可聚焦、竖线全承载 aria-hidden）、D1/D3/D4 + D5 全部复核通过。feat/share 五笔提交（6323846 / 4c98dcc / 3ad029d / aec857f / 03c3863）验收合格，可合并。

## A. 数据完整性 — 4/4 ✓

| # | 结果 | 证据 |
|---|---|---|
| A1 | ✓ | inject 样本 5 条消息（user 文本 / thinking+text / tool_use / tool_result err / image）按文件顺序呈现（id=msg-1..5），thinking/tool/image 块齐全 |
| A2 | ✓ | badlines 样本（3 好 2 坏 1 空）：3 好行全出；stderr `[bingo] warning: skipped 2 unreadable line(s)`；坏行内容 `not json at all` 未进页面 |
| A3 | ✓ | 空 JSONL 会话：exit 0，产出合法 HTML（23 KB），`— No messages —` 空态；单条消息会话正常（inject/legacy 均含单条与多条） |
| A4 | ✓ | tool_use input 全文呈现于 `<pre>`（`ls <unsafe> & echo "x"` 完整）；结果不截断；重序列化键序差异已在代码注释明确「内容语义等价」 |

## B. 四视图内容 — 6/6 ✓

| # | 结果 | 证据 |
|---|---|---|
| B1 | ✓ | thinking `<details class="think">` 折叠；工具 `<details class="tool-result w-sm">` + `Show result` 展开（input/result 全文） |
| B2 | ✓ | image 块渲染 `<div class="img-block"><img src="data:image/png;base64,iVBORw0KGgo...">`，alt 转义安全，离线可见 |
| B3 | ✓ | Team 名册：inject 实例行（name/def/state/messages 数）；无数据 → `— No agents —` |
| B4 | ✓ | 私聊：每实例历史线程完整（user/assistant 交替）；无历史 → `(no history yet)` |
| B5 | ✓ | 频道：`#chan<&>` 名/模式/成员 chip/消息流按 seq 呈现；无数据 → `— No channels —` |
| B6 | ✓ | 四面板恒存在（view-conv/team/dm/channel），英文空态不破坏结构 |

## C. 转义安全 — 3/3 ✓

| # | 结果 | 证据 |
|---|---|---|
| C1 | ✓ | 注入样本覆盖 user 文本/thinking/tool input/tool_result/agent 名/频道名/成员/频道消息中的 `<script>`、`<img onerror>`、`&"<>'`：grep 无真实 `<script>alert` 连写、无真实 `<img[^>]* onerror=` 属性，全部以实体形式呈现（`&lt;script&gt;`、`&quot;`、`&#39;`、`&amp;`）；页面唯一 `<script>` 对为自带 JS 块（1:1 合法） |
| C2 | ✓ | 工具输入 JSON 在 `<pre>` 内转义呈现，不解析为 HTML |
| C3 | ✓ | 图片仅 data: URI（media_type=image/png + base64 data）；产物无 http(s):// 外链 |

## D. 离线可用 — 3/3 ✓

| # | 结果 | 证据 |
|---|---|---|
| D1 | ✓ | 单 HTML：无 `<link>`、无 `<iframe>`、无外部 URL；CSS/JS 内嵌 |
| D2 | ✓ | 零外链即断网完整渲染（D1 成立）；data: URI 图片内嵌 |
| D3 | ✓ | 无 JS：conv 面板默认可见、其余 `hidden`，`<noscript>` 提示存在；JS 仅 tab/复制/打印增强 |

## E. 旧会话兼容 — 2/2 ✓

| # | 结果 | 证据 |
|---|---|---|
| E1 | ✓ | legacy 样本（无 share 文档、纯文本）：完整对话页（markdown 渲染 `<strong>`）+ Team/频道空态 + 四面板齐全，非降级路径 |
| E2 | ✓ | 缺 signature 的 thinking + 未知块类型：行级跳过（与 A2 同语义），stderr `skipped 2`，好行（好行三/四）全出，exit 0 不 panic |

## F. CLI 行为 — 4/4 ✓（+覆盖提示）

| # | 结果 | 证据 |
|---|---|---|
| F1 | ✓ | 无参会话名 → 取最近会话（transcript::list mtime 新→旧，与 --continue 同源）写出成功 |
| F2 | ✓ | 不存在会话：exit 1 + `STORAGE_ERROR` + 相近会话列表（`acc-legacy` 提示） |
| F3 | ✓ | `--output /nonexistent-dir-xyz/out.html`：exit 1 + 清晰 io 报错 |
| F4 | ✓ | `bingo share --help`：SESSION / `--output` / `--open` 全部文档化 |
| §3 | ✓ | 覆盖提示：二次导出输出 `[share] wrote <path> (overwritten)`；隐私警告（§7）stderr 恒打印 |

## G. 质量门槛

| # | 结果 | 证据 |
|---|---|---|
| G1 | ✓ | 独立重跑（基线 aec857f，日志 /tmp/bingo-share-acc/g1.log）：`cargo build` 0 / `cargo clippy -- -D warnings` 0（零警告）/ `cargo test` 607 passed 0 failed；dev 自报 03c3863（D5）后同样 607 全绿 + clippy 干净 |
| G2 | ✓ | guide.md 第 200 行已含 `bingo share [会话] [--output 路径] [--open]` 说明 + 敏感信息提示 |

## V. 视觉与结构（模板对齐）

| # | 结果 | 证据 |
|---|---|---|
| V1-V3 | ✓ 放行（有条件） | uiux-share #14：32 项断言全过（图片项数据缺位由代码+单测核验补足）+ DOM 复核通过（4 面板/锚点/复制按钮/助手卡计数）+ dev #13 CSS 逐规则零差异（去注释/空行后 diff 空）；模板定稿 MD5 c4a781c5（磁盘与 design.md 副本同步） |
| Minor D1 | ✓ 已修 | aec857f：`div.img-block` → `figure.img-block`（闭合 </figure>）；uiux #16 复核通过 |
| Minor D3 | ✓ 已修 | aec857f：频道头渲染 `◇ #{name}`；uiux #16 复核通过 |
| Minor D4 | ✓ 已修 | aec857f：color-scheme `light`；uiux #16 复核通过 |
| D5（a11y） | ✓ 已闭环 | 03c3863：aria-hidden 移至 `.line`（WCAG）；uiux 复核：评审脚本 34/34（两条新 a11y 规则）+ DOM 复核（441 锚点可聚焦、竖线全承载 aria-hidden） |

> D1/D3/D4 由 uiux #16 复核通过；D5 由 uiux #19 复核闭环；V 组至此全部完结。

## 备注与遗留项

1. **初跑 3 FAIL 均为断言缺陷**（非实现缺陷）：① `grep '<script>'` 误中页面自带 JS 块（注入实为 `&lt;script&gt;` 实体）；② `grep 'onerror='` 误中转义实体文本中的字样（`&lt;img src=x onerror=...`，无真实标签属性）；③ `grep 'session'` 与 help 大写 `[SESSION]` 大小写不匹配。修正为「真实标签注入特征」断言后全过。
2. **V 组修正跟踪（uiux 判定不阻塞）**：Minor D1（figure.img-block）/ D3（◇ #name 前缀）/ D4（color-scheme: light）；模板 v2.2 aria 变更（`.dec aria-hidden` → `.line`）CLI 侧同步——dev 修后 uiux 复核。
3. **模板 MD5 已定稿**：c4a781c5（uiux #14，磁盘与 design.md 副本同步）；PRD 引用已同步为 c4a781c5。
4. **E2 语义**：字段缺失按行级跳过（load_messages 统一语义，与 A2 一致），非块级跳过；行为满足「不 panic、页面其余完整」。
5. **界面语言**：页面英文（lang="en"）与 uiux #8 决策一致；CLI help 中文为 bingo 既有 CLI 语言，PRD F4 不涉及语言要求。

## 抽查汇总

- inject（27 项）/ legacy（6 项）/ badlines（3 项）/ F2（3 项）/ F3（2 项）/ 覆盖提示（1 项）/ F1（1 项）/ F4（3 项）/ E2（4 项）/ A3（3 项）：**全部 PASS**（含修正断言）
