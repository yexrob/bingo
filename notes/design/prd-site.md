# PRD: bingo 官网（项目门面站点）

> 状态：v1 定义稿
> 关联任务：新建站点推送 yexrob 公开仓库 → dokploy nginx 部署

## 1. 目标

官网是 bingo 的门面，服务两类访客：

- **潜在用户**：30 秒内看懂「bingo 是什么、能干什么、怎么装」；看完愿意 `cargo install` 试试。
- **潜在贡献者**：快速理解项目理念与架构（harness 心智模型），找到仓库与贡献入口。

**一句话定位（Hero 锚点）**：bingo is a local agent CLI written in Rust — the model produces intent, the harness gates every side effect.

**成功量尺**：访客能从首页导航到仓库 / README / 安装命令，完成「理解 → 尝试」的最小闭环。

## 2. 内容结构（栏目清单）

站内导航（顶部固定，8 项以内）：

| # | 栏目 | 要点 |
|---|---|---|
| 1 | **Hero（首屏）** | 一句话定位 + 安装命令（`cargo install --git https://github.com/yexrob/bingo --locked`）+ 两个 CTA：GitHub 仓库 / 快速开始 |
| 2 | **Features** | 8 张特性卡，每张一句话 + 一个具体行为示例：流式主循环、统一权限门、工具集（Tool trait）、子代理团队（hub-and-spoke）、serial 频道、slash 命令、Hooks 扩展点、Experience 复用机制、MCP、Skills（选 8-10 项，卡面克制：图标 + 标题 + 1-2 行） |
| 3 | **How it works（理念）** | 一段话讲透 harness 心智模型：「模型只产出意图；权限、并行、副作用、压缩、记忆与 UI 由本地 harness 负责」+ 简化架构图（文本/SVG，无 JS） |
| 4 | **Quick start** | 3 步：配置 API key → `bingo` 启动 → 第一条指令；附 `--print` headless 示例；链接 README 全文 |
| 5 | **Share 样例**（`/share/`） | 展示 `bingo share` 产出的会话 HTML（单文件、离线可用，天然适合静态站内嵌）。v1 用**真实感的会话样例**（含子代理/频道活动，手工构造或节选真实会话），占位诚实标注；CLI 团队产出真实 `bingo share` 产物后可整体替换 |
| 6 | **Docs / 文档入口** | 指向 GitHub README、README.zh-CN、设计文档（error code 契约、feedback-states）；不在站内复制全文，静态站只做索引 |
| 7 | **Contributing** | 项目理念（Rust 2024、无 unsafe、默认做减法）+ worktree 工作流 + 验证命令（build/clippy/test）+ 提交规范；链接仓库 CONTRIBUTING 或 AGENTS.md |
| 8 | **Footer** | 仓库链接、License（MIT？以仓库为准）、致谢/参考（goose、iocraft 等） |

## 3. 文案基调

**建议：英文主站 + 中文节选页（`/zh/` 或单页中文摘要）**，而非全站双语。

理由：

1. **目标受众**：GitHub 开源项目，潜在用户与贡献者以英文生态为主；README 主体已是英文，站英文与仓库一致，维护成本最低。
2. **既有资产**：README.zh-CN.md 已存在且质量高——中文节选页直接节译/链接该文件，不重复造。
3. **中文社区是真实的第二受众**（项目文档有中文版、issue 有中文讨论），提供中文节选是低成本高诚意的信号。
4. **纯静态约束**：全站双语需要语言切换机制（URL 前缀或 JS 切换），v1 做减法——英文为主站、`/zh/` 单页放中文摘要 + 链接中文 README。

文案风格：短句、具体、避免营销腔；技术名词不翻译（harness、permission gate、sub-agent）。

## 4. 技术约束

- **纯静态**：HTML + CSS + 少量原生 JS（可选，最好零 JS）；无构建步骤、无框架、无包管理器——仓库里直接是静态文件，改完即部署。
- **部署**：dokploy nginx 静态服务；文件结构 `public/`（或根目录）直接映射站点根。
- **响应式**：移动端可读（终端工具站点的用户可能在手机上看 README）。
- **可扩展**：share 样例（单 HTML 文件）直接放入站点 `/share/` 目录作为独立页面（main 定案：v1 用真实感样例，CLI 产物可替换）；站点结构已预留 `/share/` 路径。
- **无外部依赖**：不引 CDN 字体/库（离线可看、加载快、无追踪）。图标用内联 SVG。
- **视觉实现**：dev 按 `site-visual-direction.md`（token 表 + 组件清单）严格实现；`/share/` 样例页与 CLI 分享页同源同令牌（参考 `share-page-design.md` / `share-page-template.html`）。

## 5. 验收标准

### A. 内容完整性
- A1. 八个栏目全部实现，Hero 含安装命令与两个 CTA。
- A2. 特性卡覆盖产品形态：流式主循环、Tool 协议、统一权限门、Hooks、子代理（hub-and-spoke）、serial 频道、slash 命令、错误码契约、Experience 机制——每项一句话 + 一个行为示例（从 README 提炼，不照抄）。
- A3. 「How it works」含一句话心智模型与简化架构图。
- A4. Quick start 三步可独立完成，所有链接（仓库、README、README.zh-CN）有效（验收时逐一点击）。
- A5. `/share/` 样例页存在：含四视图（对话/子代理/频道/任务）或至少对话 + 子代理 + 频道活动，内容真实感（不编造华丽假数据）；页面结构可被真实 `bingo share` 产物整体替换而不改站点布局。

### B. 响应式
- B1. 375px（手机）与 1440px（桌面）宽度下：无横向滚动、无文字溢出、CTA 可点。
- B2. 导航在窄屏折叠为可用的菜单形式（或简化为单列堆叠）。

### C. 加载
- C1. 无外部资源请求（断网打开页面除字体图标外完整可读）；CSS/内联 SVG 内嵌或同域。
- C2. 首屏（Hero）无阻塞资源；总页面体量克制（< 200KB 文本内容为佳）。

### D. SEO 基础
- D1. `<title>` + `<meta description>` + Open Graph（og:title/og:description/og:image 可省略或占位）。
- D2. 语义化 HTML（`<header>/<main>/<nav>/<h1-h3>/<footer>`），正文为真实文本（非图片）。
- D3. `robots.txt` 与 `sitemap.xml`（域名定稿后补，含站点 URL）。

### E. 部署
- E1. dokploy 部署后：`bingo.ruobin.dev` 可访问、HTTP 200、静态资源同域加载、404 页不丑陋（nginx 默认可接受，P2 再自写）。
- E2. 仓库推送 yexrob 公开仓库（与 bingo 主仓库分离，站点独立仓库）。
- E3. **License（main 定案 2026-08-07）**：License = MIT——官网 footer 标 MIT；官网仓库含 LICENSE 文件（MIT 全文）；主仓库 LICENSE 由 CLI 侧处理。推送前必须补上。
- E4. **部署域名（main 定案）**：`bingo.ruobin.dev`（已解析到 dokploy，后续可改）；sitemap.xml / OG 的占位域名 `bingo.example.com` 部署前替换为正式域名。

### F. 质量
- F1. 站点仓库有 README 说明「这是官网，内容在 X，部署走 dokploy nginx」。
- F2. HTML 通过基础校验（w3c validator 无 error 级问题，或人工抽查无未闭合标签）。

## 6. 优先级

| 优先级 | 内容 |
|---|---|
| **P0**（首版必须） | Hero、Features（≥8 卡）、How it works、Quick start、Footer、导航；响应式基础；SEO title/description/OG；无外部资源；推送公开仓库；dokploy 部署可达 |
| **P1**（紧随） | Contributing 栏目、Docs 索引（链接 README 中英）、`/share/` 样例页（真实感会话样例，含子代理/频道活动）、`/zh/` 中文节选页、sitemap/robots、404 页 |
| **P2**（有真实需求再做） | 用真实 `bingo share` CLI 产物替换 `/share/` 样例、主题切换、博客/变更日志、更多语言 |

## 7. 依赖与顺序

1. 域名/部署目标确认（dokploy 上建站点 → 拿到 URL）→ 2. 内容与页面（P0 先行）→ 3. 推送公开仓库 → 4. dokploy 部署 + 验收（A/E 组）→ 5. P1 补全。

## 8. 风险与未决项

- **域名已定**：`bingo.ruobin.dev`（main 定案 2026-08-07，已解析到 dokploy）；sitemap/OG 占位域名部署前替换。
- **License 已定**：MIT（main 定案 2026-08-07）；官网仓库推送前补 LICENSE 文件 + footer 标 MIT。
- **站点与仓库分离**：官网独立仓库（如 `yexrob/bingo-site`），避免官网改动污染主仓库提交史。
