# PRD: `bingo share` 子命令（HTML 会话分享页）

> 状态：v1.1 修订稿（验收锚点 + 范围边界；正文视图集/数据源由 dev 定案版更新，本版由 pm 收口）
> **v1.1 修订（pm，2025-08-07）**：① 视图集与数据源已按用户指令定案（四视图 = 对话 / Team 名册 / 私聊 / 频道，ShareStore 增量持久化，事实源 share-page-design.md v2.0 §9 / 模板 MD5 c4a781c5）；② 界面语言按 uiux-share 决策（#team-share #8）跟模板走英文：lang="en"、UI 标签英文、数据内容原样；③ 视觉验收锚点 = share-page-template.html（唯一事实源），机械评审方法见 share-page-design.md §10.1（19 项断言，uiux 已实测：当前实现 12 FAIL / 7 PASS，与差距清单吻合）；④ 图片内嵌 data: URI 为必验收项（B2）。
> 关联：worktree `feat/share`；数据源现状见 [research.md](../../research.md) D11（transcript JSONL）
> 视觉实现：dev 按 [share-page-design.md](./share-page-design.md)（HTML 生成契约）+ `share-page-template.html`（唯一事实源，MD5 c4a781c5）输出，本文只定义验收与范围

## 1. 目标与用户场景

**一句话**：`bingo share` 把一次会话导出为单个离线可打开的 HTML 文件，让没装 bingo 的人也能读对话。

**用户场景**：

1. **复盘**：用户跑完一次复杂任务（多轮工具调用 + 子代理协作），导出 HTML 存档或回看。
2. **协作/评审**：把会话发给同事/朋友（不装 bingo、不碰终端），对方浏览器打开即读。
3. **上报 bug / 演示**：把失败的排查过程发给维护者，或演示 bingo 的团队协作能力（对话 + Team 名册 + 私聊 + 频道四视图同页呈现）。

**非目标**：不做在线托管、不做实时协作、不做回放执行——分享的是**已发生的事实**。

## 2. 范围边界（v1 明确不做）

| 不做 | 理由 |
|---|---|
| 多会话对比/合并导出 | 单会话单文件，组合需求尚无真实用户 |
| 在线托管/上传、生成可分享链接 | 需要服务器与账号体系，v1 纯本地生成 |
| 权限控制（密码/过期/阅后即焚） | 文件即公开；v1 只在输出时警告「含敏感信息请自行判断传播」 |
| 敏感信息自动脱敏/红action | 无法可靠识别语义敏感内容；由用户自行把关 |
| 任务视图 | 用户指令定案 v1 视图集 = 对话/Team/私聊/频道；任务视图不在 v1（模板 §9：如需扩展可追加面板） |
| 交互式过滤/搜索/主题切换 | 静态页做减法；P2 若有必要再加轻量 JS |
| 多语言页面 | v1 界面英文（lang="en"，UI 标签与空态英文，与模板一致；数据内容原文）；不做国际化 |

## 3. CLI 接口

```
bingo share [会话名] [--output <路径>] [--open]
```

| 参数 | 说明 |
|---|---|
| `[会话名]` | 位置参数，可选。与 `/resume` 同一命名体系（`{slug}-{ts}` 或 rename 后的 `{slug}-{ts}-{name}`）。缺省 = 最近会话 |
| `--output <路径>` | 输出文件路径。缺省 = 当前目录下 `<会话名>.html`；已存在则直接覆盖并提示 |
| `--open` | 生成后用系统默认浏览器打开（`open` / `xdg-open`） |

**默认行为**：选择会话 → 读取并解析 → 生成单 HTML 文件 → 打印输出路径（与 bingo 现有 headless 输出风格一致，非 TTY 下输出 `[share] wrote <路径>` 单行可 grep 格式）。会话不存在 / 文件解析失败 → 走统一错误码出口（`STORAGE_ERROR` 等既有契约，见 `src/error.rs`）。

**错误信息**：会话名不匹配时列出相近的可用会话（沿用 `/resume` 的列表演示风格）。

## 4. 数据源与四视图

**实现方案（dev 已落地，main 定案）**：会话运行时经 `ShareStore` 把子代理实例（含完整 history）与频道日志**增量持久化**到 `~/.local/share/bingo/shares/<session-stem>.json`（单文件 JSON，原子写 tmp+rename，损坏备份重建；存储失败只告警、不阻塞会话——share 是增强不是契约）。`bingo share` 读取该文档 + transcript 生成 HTML。

| 视图 | 内容 | 数据源 | 可得性 |
|---|---|---|---|
| **对话视图** | 主会话消息流：user/assistant 文本、thinking（折叠）、工具调用（折叠：输入 JSON + 结果）、图片 | transcript JSONL（`~/.local/share/bingo/transcripts/<slug>-<ts>.jsonl`） | 必然可得（share 文档缺失时回落空文档，仅此视图） |
| **Team 名册** | 子代理实例总览（name / def / state / history 条数 / description） | ShareDoc `agents[]`（运行时 upsert：insert/finish/stop 事件同步） | 会话运行时记录；无则空态 |
| **私聊视图** | 每实例完整私聊历史（SendMessage 续话即该实例的 history） | ShareDoc `agents[].history` | 会话运行时记录；无则空态 |
| **频道视图** | 频道元数据（模式/成员）+ 消息流（serial 顺序） | ShareDoc `channels[]`（create/invite/kick/post 事件同步） | 会话运行时记录；无则空态 |

**视图定义以 `share-page-design.md` v2.0 / `share-page-template.html` 为唯一事实源**（用户指令已定案：四视图 = 对话 / Team 名册 / 私聊 / 频道，取代早前草案的子代理活动+任务方案；交互为 tab 方案 role=tablist + hash 直达，JS 只切显示不碰数据，无 JS 时默认对话面板）。PRD 只验收不定义视觉。

**界面语言**：跟模板走英文（uiux-share #team-share #8 决策）——`lang="en"`，UI 标签（Conversation/Team/DM/Channels/Thinking/Show result/Print）与空态（`No …`）英文；数据内容（会话文本、工具输入/输出）原样不动。

## 5. 验收标准（每项可验证）

### A. 数据完整性
- A1. transcript 中每条消息都出现在对话视图，顺序与文件一致（逐条比对，含 thinking / tool_use / tool_result / image 块）。
- A2. 坏行跳过语义与 `Transcript::load_messages` 一致：单行损坏不导致整个导出失败，好行全出，且输出 warning 到 stderr。
- A3. 空会话（0 条消息）与仅一条消息的会话都能产出合法 HTML，不 panic、不产出空文件。
- A4. 工具调用的输入 JSON 与结果**原样呈现**（转义后），不截断不丢失（与 TUI 折叠「+N lines」的截断不同——导出页是完整事实记录）。

### B. 四视图内容
- B1. 对话视图含 thinking 折叠块与工具调用折叠块，默认收起、可展开（`<details>`）。
- B2. 含图片块的消息按模板契约以 `figure.img-block` + `<img src="data:{media_type};base64,{data}" alt="">` 内嵌渲染，离线可见（uiux 确认模板要求 data: URI 内嵌，media_type 校验 `image/*`、data 转义）。
- B3. Team 名册视图：含子代理的会话显示实例（name/def/state/历史条数）；无任何子代理活动的会话显示空态文案（非报错）。
- B4. 私聊视图：每个有 history 的实例呈现完整对话历史；无 history 显示占位（「No messages」类文案）；无实例显示空态。
- B5. 频道视图：含频道消息的会话按序呈现频道名、模式、成员与消息流；无频道活动显示空态。
- B6. 空态不破坏页面结构（四面板恒存在，无数据时显示英文 `No …` 占位；tab 交互下无 JS 时默认对话面板、其余 hidden）。

### V. 视觉与结构（模板对齐，v1.1 新增）
- V1. 产物结构/class/CSS 令牌与 `share-page-template.html`（唯一事实源，MD5 c4a781c5）一致：浅色文档底令牌（--bg:#FAFAF7 / --accent:#B05227 / --accent-border:#E7C4B2 等，design.md §2 全表）、消息 `.dec(.anchor+.line)+.content(.w-sm/md/lg)`、助手 `.card` 细边框卡（1px+4px+0.5rem）、用户无框、工具两段式 `.tool-title` + `details.tool-result.w-sm`（Show result 展开 input/result 全文）、元信息 `.meta` 小号大写、`id="msg-N"` 锚点 + 复制链接（JS 渐进增强不碰数据）。
- V2. 保留类 `.roster/.dm-*/.ch-*/.tabs/data-view/.empty` 结构不动只换样式；打印/空态/reduced-motion 段落存在。
- V3. 以 design.md §10.1 的 19 项断言为机械评审方法（uiux 执行对照评审，任一 FAIL 打回附差异行），评审通过为 V 组验收通过的证据。

### C. 转义安全
- C1. 所有动态文本（用户输入、模型输出、工具输入/输出、会话名、频道名、成员名、实例名/描述）经 HTML 转义；构造含 `<script>`、`<img onerror=...>`、`&"<>'` 的测试会话，导出 HTML 中数据部分**无未转义注入**（grep 断言无 `<script>` 标签、无 `onerror=` 出自数据）。
- C2. 工具输入 JSON 以 `<pre>` + 转义呈现（不解析为 HTML）。
- C3. 图片仅允许 `data:` URI（由 base64 块构造），不透传任何外部 URL 或 html 内容。

### D. 离线可用
- D1. 产物为**单个** HTML 文件：无外部 CDN、无外链 CSS/JS/字体、无 `<iframe>` 外部内容；`file://` 协议直接打开完整渲染（断网可验证）。
- D2. 在无网络环境用浏览器打开产物，四面板、图片、折叠均正常（Network 面板 0 请求）。
- D3. 无 JS 时页面仍完整：对话面板默认可见，其余面板 hidden（JS 仅做面板切换增强，不拼数据）。

### E. 旧会话兼容
- E1. 对**没有任何**子代理/频道/任务数据、且无 share 相关元数据的旧 transcript，仍产出完整对话页（这是 v1 的主路径，非降级路径）。
- E2. 字段缺失容错：thinking 无 signature、tool_result 无 content、role 之外的未知块类型——跳过该块不 panic，页面其余部分完整。

### F. CLI 行为
- F1. 无参会话名 → 取最近会话（与 `--continue` 同一来源 `Transcript::latest`）；`--output` 生效；`--open` 调用系统 opener。
- F2. 会话名不存在 → 非零退出 + 统一错误码输出 + 相近会话列表提示。
- F3. `--output` 指向不可写路径 → 清晰报错，非零退出。
- F4. `bingo share --help` 文档化全部参数。

### G. 质量门槛
- G1. `cargo build`、`cargo clippy -- -D warnings`、`cargo test` 全绿；相关逻辑带单测（解析/转义/视图提取至少各一组）。
- G2. 用户可见行为（新子命令、错误信息）同步内置技能 `src/skills/bundled/guide.md`。

## 6. 验收顺序建议（依赖关系）

1. 数据层（transcript 解析 + 四视图提取）→ 2. HTML 生成与转义 → 3. CLI 装配与错误路径 → 4. 兼容性（E 组）与安全（C 组）→ 5. 文档与 guide 同步。

## 7. 风险与未决项

- **ShareDoc 与 transcript 的一致性**：share 文档是运行期增量快照（增强不是契约），若会话中途崩溃/存储失败，子代理/频道视图可能缺尾部数据——v1 接受（文档化：分享页的对话视图始终完整，团队/频道视图以运行期快照为准）。
- **大文件性能**：数百条消息 + 大量 base64 图片可能产出 MB 级 HTML；v1 不做流式分页，但解析须 O(n) 单遍（P2 再考虑图片降采样）。
- **隐私警告**：导出时向 stderr 打印「此文件包含完整对话与工具输出（可能含敏感信息），分享前请自行审阅」。
