# bingo

> **English**: [README.md](README.md) — English documentation is here.

Rust 实现的本地 agent CLI（agent harness）。在终端里驱动大模型完成编程与系统任务：
工具调用、权限审批、子代理编排、任务追踪、上下文压缩、记忆与 MCP 扩展，
全部在本地运行，模型只产出意图，副作用由 harness 统一把关。

## 特性一览

- **流式主循环**：Messages API 流式响应，工具调用 → 权限门 → 执行 → 结果回填，
  单轮可并发执行多个安全工具。
- **统一权限门**：五种权限模式 × 规则表（allow/deny/ask）→ 放行 / 拒绝 / 询问。
- **工具集**：Bash、Read/Glob/Grep、Edit/Write、WebFetch/WebSearch、Task 族、
  AskUserQuestion、Skill、Agent（子代理）与 MCP 工具，全部经同一 Tool trait。
- **子代理（hub-and-spoke）**：主 agent 派生命名子代理，异步执行、完成通知自动
  注入上下文；SendMessage 续话、AgentControl 管理生命周期。
- **Agent 团队（项目级）**：`.bingo/team.json` 把一组角色固定到项目，启动自动
  拉起（成员零 token 待命），`/team` 命令族管理；钉了团队的项目里，团队就是默认
  用工对象——活先派给队内成员，另派的子代理只是不进队的临时工；
  `.bingo/team-norms.md` 是这支队伍的协作约定，每个成员随启动带着它。
  跨会话记忆按「项目路径 +
  分支」隔离。
- **经验库（Experience）**：agent 按项目沉淀可复用的操作经验
  （trigger/summary/steps/verify），跨会话复利，并记录经验证的 helpful/harmful
  结果，但不会自行晋升或降级。
- **TUI**：ratatui 双模式（默认 fullscreen 备用屏 canvas，`--inline` 将已完成
  内容保留在终端 scrollback 并启用 kitty graphics 图片渲染），历史反向搜索，
  slash 命令菜单。
- **技能（Skills）**：`SKILL.md`（YAML frontmatter + markdown）即插即用，
  内置 `guide` 技能 + 用户/项目目录技能。
- **MCP**：stdio 与 streamable HTTP 服务器接入，自动适配为同构工具。
- **上下文管理**：token 预算监控、自动压缩（保留最近消息 + 结构化摘要）、
  手动 `/compact`、压缩失败熔断。
- **会话与记忆**：transcript JSONL 持久化（`--continue`/`/resume` 恢复），
  memdir 自动记忆 + CLAUDE.md/AGENTS.md 项目记忆。
- **Hooks 扩展点**：工具前后、会话起止、压缩、Stop、任务生命周期等事件的
  shell hook（stdin 喂 JSON、stdout 回传决策）。

## 构建与安装

要求：Rust 2024 edition（稳定版工具链，`rustup` 安装即可）。

### 直接从 GitHub 安装（cargo install）

```bash
cargo install --git https://github.com/yexrob/bingo --locked
```

- 安装到 `~/.cargo/bin/bingo`（确保 `~/.cargo/bin` 在 `PATH` 中）。
- `--locked` 使用仓库已提交的 `Cargo.lock`，保证依赖版本可复现。
- 依赖 `rsmarkdown-core` 同为 git 依赖，cargo 会自动一并拉取。

更新到最新版：

```bash
cargo install --git https://github.com/yexrob/bingo --locked --force
```

### 官方预编译二进制（GitHub Releases）

每个版本标签发布预编译二进制（Windows 为 ZIP，macOS/Linux 为 tar.gz，
均附带 `checksums.txt` SHA-256 校验文件）：

| 平台 | 文件 |
|---|---|
| Windows x86_64 | `bingo-x86_64-pc-windows-msvc.zip`（内含 `bingo.exe`） |
| macOS（Apple Silicon） | `bingo-aarch64-apple-darwin.tar.gz` |
| macOS（Intel） | `bingo-x86_64-apple-darwin.tar.gz` |
| Linux x86_64 | `bingo-x86_64-unknown-linux-gnu.tar.gz` |

无需 WSL 或 Rust 工具链——下载解压直接运行 `bingo` / `bingo.exe`。
Windows 构建基于原生 `x86_64-pc-windows-msvc` 目标，默认 shell 为
PowerShell（见下文 `shell` 配置）。

### 从源码构建

```bash
cargo build --release          # 构建二进制：target/release/bingo
cargo install --path .         # 或安装到 ~/.cargo/bin
```

验证：

```bash
cargo test          # 单元测试
cargo clippy -- -D warnings   # lint 必须零告警
```

## 快速开始

1. **配置 API key**（二选一）：
   - 环境变量：`export ANTHROPIC_API_KEY=sk-ant-...`（或 DeepSeek：`DEEPSEEK_API_KEY`）；
   - settings 文件：`~/.config/bingo/settings.json` 写 `{"apiKey": "..."}`
     （settings 优先于环境变量）。
   - 自定义端点用 `ANTHROPIC_BASE_URL` 或 settings 的 `apiBaseUrl`/`providers`。
2. **启动**：

```bash
bingo                       # 交互式 TUI（默认 fullscreen 模式）
bingo --inline              # inline 模式：历史保留在终端 scrollback
bingo -p "修复这个 bug"       # headless：prompt 参数，结果打到 stdout
bingo -p < prompt.txt       # headless：从 stdin 读 prompt
bingo --continue            # 恢复最近一次会话
```

没有任何凭据也能启动：欢迎卡会给出引导（`/provider login codex` 使用 ChatGPT 订阅，或在 settings 写 `apiKey`），配好凭据前请求会快速失败并提示下一步。

## 命令行参数

| 参数 | 说明 |
|---|---|
| `-p, --print` | headless 模式：直接把回复打到 stdout（prompt 取参数或 stdin） |
| `--inline` | inline 模式：不使用默认全屏 canvas，已完成内容保留在终端 scrollback；与 `--fullscreen` 互斥 |
| `--fullscreen` | 显式选择默认全屏模式（备用屏 canvas，输入吸底、app 内滚动）；为兼容旧调用保留；与 `--inline` 互斥 |
| `--model <名>` | 使用指定模型（缺省依次回落 settings `model`、内置 `claude-sonnet-5`） |
| `--no-team` | 不自动拉起项目团队（覆盖 settings `team.autoStart`） |
| `--permission-mode <模式>` | 权限模式：`default`/`acceptEdits`/`plan`/`dontAsk`/`bypassPermissions`（默认取 settings） |
| `--continue` | 恢复最近的会话继续对话 |
| `bingo share [会话] [--public] [--open] [-o 路径]` | 默认仅在本地导出自包含 HTML；`--public` 才显式发布任何人可访问的链接（上传前显示敏感内容警告） |
| `prompt` | 非交互提示词（缺省从 stdin 读取；交互模式忽略） |

## 使用界面

### 输入

- `Enter` 发送；`\`+Enter 或 Ctrl+J 换行（多行输入）。
- 输入 `!` 进入 bash 模式：命令直接执行、不经模型（`!echo hello`）；
  前缀粘性保留；空输入 Esc/退格/Ctrl+U 退出。交互式/TTY 命令
  （top/vim/ssh/fzf 等）会被拒绝，请用批处理等价物（`top -b -n 1`）。
- 大段粘贴自动折叠为 `[Pasted text #N +M lines]` 占位，发送时展开真实内容。
- `Ctrl+R` 历史反向搜索；`↑↓` 历史回溯（多行输入内先移光标）。
- `Ctrl+S` 暂存/恢复输入、`Ctrl+Y` 粘回删除、`Ctrl+_` 撤销。

### 快捷键（空输入按 `?` 看全表）

| 键 | 功能 |
|---|---|
| `Esc` | busy 时中断 / 关闭下拉与面板 / 双击清空输入 |
| `Ctrl+C` | busy 中断 / 有文本清空 / 空输入连按两次退出 |
| `Ctrl+T` | 显隐任务区 |
| `Ctrl+O` | 展开/闭合切换：展开 = 重放完整 transcript 供上滑翻看 |
| `Ctrl+G` | agent / 频道选择器 → Slack 式工作区（整屏一栏消息流 + 输入框；Ctrl+K 换会话、alt+↑↓ 上下会话） |
| `Ctrl+L` | 清屏重画 |
| `Shift+Tab` | 循环权限模式（default → acceptEdits → plan） |
| `Alt+T` | 思考开关 |
| busy 时回车 | 消息排队，回合结束自动发送 |

### Slash 命令（`/help` 全量清单）

`/model [名]`（无参进入 provider → 模型两级选择器；provider 与模型作为
一对持久化）、`/provider [名称]`（列出/切换多 provider；`/provider login
<名> [--device-auth|--manual <token>]` 登录订阅端点、`logout` 退出）、
`/think [off|low|medium|high|xhigh|max]`（无参进入等级选择器，选择持久化）、
`/theme`、`/permissions [allow|deny|ask] [规则]`、
`/mcp`（状态）· `/mcp enable|disable [name|all]` · `/mcp reconnect <name>`、
`/skills`（清单，`/技能名` 直接执行）、`/context`（用量）、`/status`、
`/config`（生效配置与来源：哪个层/环境变量赢了、当前端点、未知配置项提示）、
`/compact`（强制压缩）、`/resume [名称]`（恢复历史会话）、`/rename`、
`/share [--public] [--open]`、`/clear`、`/exit`。
`/share` 默认只在本地生成自包含 HTML；只有显式加 `--public` 才会上传为
任何人可访问的公开链接，且上传前会先显示敏感内容警告。`--open` 打开本地文件
或已发布链接。等价 CLI 为 `bingo share [会话] [--public] [--open] [-o 路径]`。
`/team`（项目团队）：`list`（图纸+运行区同屏）、`start`（拉起/幂等复用）、
`status`、`assign <成员> <任务>`（派活）、`stop`、`validate`、`new`
（脚手架生成 team.json + team-norms.md）、`norms`（团队规范）、`memory list|gc`。

### 图片渲染

模型回复中的 markdown 图片（`![alt](路径)`，支持 `~/`、相对路径、data:、http(s)）
在支持 kitty graphics 的终端（Ghostty/kitty 等）两种模式下都渲染真实图片：
fullscreen 在活动视口内放置图片，`--inline` 还会在内容落盘时写入 scrollback。
不支持的终端显示 `#[image]` 占位。tmux 内 bingo 会自动开启 passthrough
（`tmux set -p allow-passthrough on`），落盘图片在 Ghostty/kitty 外层终端下以
Unicode 占位符（U=1）渲染；WezTerm/Konsole 虽支持 graphics 协议但不支持占位符，
tmux 下仍显示 `#[image]` 占位（tmux 内的活动视口同样保持占位）。

## 配置（settings.json）

三层配置浅层合并，后者覆盖前者。UI 内的选择（/model /provider /theme /think）
写回「生效层」：某层已定义该键则更新该层，否则写 user 层——不会在任意目录凭空创建
`.bingo/`；`/permissions` 与 `/mcp disable` 属项目级状态，仍写项目层。

1. **user**：`~/.config/bingo/settings.json`（`XDG_CONFIG_HOME` 优先）
2. **project**：`.bingo/settings.json`（入库，注意别提交密钥）
3. **local**：`.bingo/local.json`（个人覆盖，不入库）

| 配置项 | 类型 | 说明 |
|---|---|---|
| `apiKey` | string | API key（settings 优先于 `ANTHROPIC_API_KEY`/`DEEPSEEK_API_KEY`） |
| `apiBaseUrl` | string | API 端点（settings 优先于 `ANTHROPIC_BASE_URL`；缺省官方） |
| `providers` | object | 命名 provider：`{名: {protocol?, apiKey, apiBaseUrl?, supportsImages?, oauth?}}`，`/provider <名>` 切换；`protocol` 为 `"anthropic"`（缺省）或 `"openai"`（Responses API，Bearer 认证；`apiBaseUrl` 缺省 `https://api.openai.com`）；`oauth: {kind: "codex"}` 启用 OAuth 登录（`/provider login`，apiKey 优先） |
| `model` | string | 默认模型（`/model` 选择写入）；优先级 `--model` > settings > 内置 `claude-sonnet-5` |
| `thinkingLevel` | string | `off` 不发 thinking 参数（兼容 DeepSeek，缺省）；`low`/`medium`/`high`/`xhigh`/`max` 发自适应 thinking + 对应档位的 `output_config.effort` |
| `permissionMode` | string | `default` / `acceptEdits` / `plan` / `dontAsk` / `bypassPermissions` |
| `theme` | string | `auto`（跟随终端背景）/ `dark` / `light` |
| `cacheControl` | bool | 发送 prompt caching（默认关：非官方端点不稳定） |
| `respondToBashCommands` | bool | `!` 命令执行后是否交模型回应（默认 true） |
| `shell` | string | Bash 工具与 hooks 使用的 shell。默认按平台：macOS `/bin/zsh`、其他 Unix `/bin/bash`、Windows `powershell.exe`。PowerShell 系用 `-Command` 执行；配置其他 shell（如 Git Bash 的 `bash.exe`）用 `-c` 执行 |
| `mcpServers` | object | 见下「MCP」 |
| `disabledMcpServers` | string[] | 禁用的 MCP 服务器名单（`/mcp disable` 写入） |
| `permissions` | object | `{allow[], deny[], ask[]}`，规则语法见「权限系统」 |
| `experimental` | object | 实验特性：`agentChannels`、`channelMessageLimit`（默认 500）、`agentMessageLimit`（默认 50）、`chatAvatars`（默认 false = 主聊天不带脸；工作区视图不受此开关管辖） |
| `team` | object | 团队启动行为：`{"autoStart": true}`（缺省 true = 项目绑定 team 时启动自动拉起；`--no-team` 或 false 关闭） |
| `hooks` | object | 各事件 hook，见「Hooks」 |

示例：

```json
{
  "apiKey": "sk-ant-xxxx",
  "apiBaseUrl": "https://api.anthropic.com",
  "providers": {
    "deepseek": { "apiKey": "sk-ds", "apiBaseUrl": "https://api.deepseek.com" },
    "local": { "apiKey": "sk-any", "apiBaseUrl": "http://127.0.0.1:11434/v1" },
    "openai": { "protocol": "openai", "apiKey": "sk-...", "apiBaseUrl": "https://api.openai.com" }
  },
  "model": "claude-sonnet-5",
  "thinkingLevel": "medium",
  "permissionMode": "acceptEdits",
  "mcpServers": {
    "files": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."] },
    "remote": { "type": "http", "url": "https://mcp.example.com/mcp", "headers": { "Authorization": "Bearer xxx" } }
  },
  "permissions": { "deny": ["Bash(git push:*)"] }
}
```

## 工具集

全部经统一 Tool trait（serde/schemars 生成 schema，单一来源）：

| 工具 | 说明 |
|---|---|
| `Bash` | 在独立进程组（Unix）/ 进程树（Windows）执行 shell 命令；超时/取消整树终止，不留孤儿进程；非交互命令 |
| `Read` / `Glob` / `Grep` | 只读检索；默认跳过 `.git`/`target`/`node_modules` 与隐藏目录 |
| `Edit` / `Write` | 文件编辑（产生 unified diff 供 UI 预览） |
| `WebFetch` / `WebSearch` | 网页抓取与搜索（共享 HTTP 连接池；预批准域名自动放行） |
| `Agent` | 派生命名子代理（异步执行，完成通知注入上下文；`background:false` 可同步等待） |
| `SendMessage` / `AgentControl` | 子代理续话与生命周期管理（仅主会话装配） |
| `Team` | 项目编队（仅主会话装配）：`status`/`validate` 只读免询问，`start`/`stop`/`save` 在任何权限模式下都要用户当面确认 |
| `TaskCreate`/`TaskUpdate`/`TaskGet`/`TaskList` | 任务追踪（磁盘存储，TUI 任务区同源，含生命周期 hook） |
| `ExperiencePropose`/`ExperienceCommit`/`ExperienceQuery`/`ExperienceOutcome`/`ExperienceForget` | 跨会话经验库（见下） |
| `AskUserQuestion` | 向用户提选择题（TUI 复用权限询问模态） |
| `Skill` | 技能调用（见下） |
| `mcp__<server>__<tool>` | MCP 接入的工具（见下） |
| `Channel` / `Post` | 实验：agent 频道互发（见下） |

## 子代理

- 主 agent（depth 0）装配 `Agent`/`SendMessage`/`AgentControl`；子代理（depth ≥ 1）
  只保留 `Agent`（可再派生），无法管理兄弟——hub-and-spoke 拓扑。
- **具名定义**：`~/.config/bingo/agents/*.md` 与 `.bingo/agents/*.md`
  （从 cwd 向上逐层查找，同名项目层优先）；frontmatter
  `name/description/model/provider`，正文 = 子代理 system prompt；
  Agent 工具的 `agent` 参数引用定义。
- 派生实例有名字（`name` 参数，缺省取定义名/`agent`，重名自动 `-2`/`-3`），
  transcript 显示为 `◉ 名字 · 任务`；完成后历史保留。
- `SendMessage` 向实例发后续指令（上下文保留）；实例忙时排队，当前回合结束
  自动送达。
- `AgentControl` 可 `list`/`stop`/`delete`。
- 默认异步执行：立即返回实例名与 task_id，完成时自动通知注入下一轮上下文。

## Agent 团队（项目级）

团队把一组角色固定到一个项目：声明式编排层，成员引用具名定义（AgentDef）、
房间复用频道机制、控制面仍是 hub-and-spoke。

- **定义**：`.bingo/team.json`（camelCase，进版本库）——`name` + `channel{mode,
  messageLimit}` + `members[{name, agent, avatar?, model?, provider?, thinking?}]`；`name` 即消息上显示的名字（取人名而非角色代号），`avatar` 钉住内置头像之一；成员引用 AgentDef，人格单一事实来源
  仍在 `.bingo/agents/<名>.md`，一人格可入多 team。
- **一个 team 多个房间**：`channels[{name, mode?, messageLimit?, members?}]` 声明这支
  队伍开哪些房间，各房间成员可以不一样——像一个部门有站会、有发布群、有设计评审，
  同一个人在其中一些里、不在另一些里。`members` 省略即全队；写了 `channels` 就以它
  为准，不再另建一个以 team 命名的大房间（没人要的房间就是没人读的房间）。什么都不
  写时仍是老样子：一个叫 team 名、装全体成员的房间，旧文件不用改。
- **多层 team（部门制）**：`teams[{name?, path}]` 声明子队伍的图纸在哪儿，可递归到
  任意深度（上限 8 层）。`path` 相对于本 team 自己的目录（拒绝绝对路径——进了版本库
  的组织架构得跟着仓库走），可以指目录，也可以直接指那份 `team.json`。每个 team 都以
  **自己的目录**为根：角色定义、团队规范、git 分支、记忆分区全都取自那里——所以从根
  会话进到某个部门，和直接在那个目录开会话拿到的是同一支队伍；子树也因此能单独成立。
  队名、成员名、房间名在整棵树内唯一，于是 `SendMessage("Linh")` 不带前缀就能点到三
  层之下的人。房间的成员只能取自本 team 及其子树——上级可以召集自己这一摊，平级不能
  征调别的部门。子树里的成员会在系统块里被告知自己团队的目录（工具路径是相对**会话**
  的工作目录解析的，不是相对它的团队）。`/team status|start|stop|validate|memory`
  与 `Team` 工具的各动作都作用于整棵树。
- **逐成员的引擎**：`model` / `provider` / `thinking` 钉住这名成员跑在什么上面。
  谁用哪个模型是编队的一部分，不是每次派生临时决定的事，所以写进进版本库的图纸——
  一支队伍可以让评审跑便宜快的端点、让架构跑贵的。三者都是先落回 AgentDef、再落回
  会话，与 `Agent` 调用的同名参数同一套优先级；`provider` 指到会话之外的端点时必须
  同时给 `model`（模型名换个端点就不认识了）。`/team list` 与 `AgentControl list`
  会报出每个在跑实例实际所在的引擎。
- **启动自动拉起**：`settings.team.autoStart`（缺省 true）时启动即拉起**整棵树**——
  先派生全树成员，再开所有房间（两段分明：上级的房间可能装着下级的人，人没起就开房，
  房里会缺人），但**不唤醒**（成员 Idle 零 token 待命，等 `/team assign` 或
  频道消息才开跑）。opt-out：`--no-team` 或 `team.autoStart: false`。
  幂等：以实例名为键，重复 `/team start` 复用不重派。整树校验先跑：树里任何一处引用
  有问题，一个成员也不派。
- **命令**：`/team list`（图纸+运行区同屏）、`start`、`status`
  （●待命 ◐忙碌 ✗异常 ○离线）、`assign`、`stop`、`validate`（与 start 同源
  校验：validate 能过 start 必成）、`new`（脚手架，产物必过 validate，并附一份
  团队规范初稿）、`norms`（读团队规范）、`memory list|gc`。
- **固定团队优先用工**：项目钉了团队，hub 的系统提示里就带着这份名册和随之而来的
  规矩——活先用 `SendMessage` 交给队内匹配的成员，只有队里没人覆盖的工作才另派
  子代理。给一个正闲着的成员再派一个替身，既浪费你在付钱养的队伍，也丢掉了它
  已经攒下的记忆。
- **临时招募不进队**：有固定团队时，Agent 工具派生出来的是「临时工」而非成员。
  它不会写进 `.bingo/team.json`；在 `/team list` 与 `AgentControl list` 里与队伍
  分开列（`crew` / `hire`）；会以 `type: hire` 记进队伍的 `decisions.md`；任务
  完成即回收——空闲、收件箱为空、没有欠着的回复，并留给 hub 一轮追问的窗口。
  这一回收只在队伍确实起着时才跑：没有团队的项目里，临时子代理的生命周期和过去
  一模一样。
- **团队规范**：`.bingo/team-norms.md`，与图纸并列进版本库，是这支队伍的协作约定
  ——写成散文而非 schema，因为它既要被模型读，也要被人评审。它随启动进入每个成员
  以及每个临时工的系统块，无需每次口头重申，并且自带优先级条款：显式指令在它所
  针对的那一点上压过规范，其余规范照旧生效。`/team new` 会生成一份初稿（已存在
  则不覆盖），`/team norms` 打印磁盘上的内容。
- **跨会话记忆**：成员历史 + append-only 决策记录存
  `~/.config/bingo/teams/<项目哈希>/<分支>/<team>/`——按「项目路径 + 分支」隔离，
  main 与特性 worktree 记忆互不污染。每个成员一份 `<名>.md`（可读转录）
  和一份 `<名>.json`（精确记录）。
- **只告诉位置，不预载**：成员以空上下文启动，只多一行说明自己的转录在哪儿，
  需要之前的结论时自己去读。预载的做法是在成员的第一轮上收一笔不断增长又看不见的
  税——那个文件无界且单调增长，每个 session 追加、没有任何东西修剪——换来的是
  衰减很快的相关性。hub 自己每个 session 也是干净启动的，crew 成员没有理由例外。
  `/team memory list` 看磁盘上有什么，`.md` 直接打开就能读。
- **`Team` 工具**（仅主会话）把同一套能力给模型：`status`（图纸 + 成员运行态 +
  可用定义清单）、`validate`、`start`、`stop`、`save`（写图纸，整份覆写，须给完整名单）。
  读免询问；**任何变更都由用户当面确认**——询问在所有权限模式下都出现（含
  `bypassPermissions`），`allow` 规则也不能预授权，只有 `deny` 压得住。确认行给的是
  变化而非文件（`改写 .bingo/team.json · dev-room · 4 名成员（-ui +qa）`）；用
  Write/Edit 手改 `.bingo/team.json` 问同一个问题。派活不在工具里，用 `SendMessage`。
  整份覆写有一个例外：`teams`（组织架构）每次 save 原样带过——它指向别的目录、是人手
  搭起来的，改名册不构成重新决定组织架构的理由，确认行会写明「保留 N 个子 team」。
  房间可以改：给了 `channels` 就整体替换，不给就保留；对一支已声明 `channels` 的队伍
  再传 `mode`/`message_limit` 会被拒绝而不是猜——照做就会删掉它描述不了的那些房间。

## 频道（实验特性）

`settings.experimental.agentChannels: true` 开启后：

- 主 agent 获得 `Channel`/`Post` 工具：建频道、进出成员（成员限直接子代理，
  主 agent 名 `main` 自动入席）；成员用 `Post` 发言，消息进全体成员上下文（同序）。
- `serial` 频道落后发言被弹回并附新增消息（agent 阅读后自行改口，报数式顺序
  由此涌现）；`free` 频道允许交叉。
- 超限自动冻结频道并通知主 agent（`channelMessageLimit`/`agentMessageLimit` 预算闸）。
- 频道在 transcript 显示为 `◇ #名字` 行；Ctrl+G 打开全屏工作区，直接以
  user 身份发言。

## 工作区视图（Ctrl+G）

整屏只有一栏会话：顶部一行标题（频道或实例名，右端是队名）、消息流、输入框。
没有 rail、没有侧栏，也不画任何自己的底色——透出的是终端本身的背景。换会话靠
**Ctrl+K**（快速跳转，列出全部会话及未读数）与 **alt+↑↓**；Tab 在消息区与输入框
之间切换，Esc 返回。

**头像**：能放置 kitty 图片的终端（与内联图片同一能力：Ghostty/kitty，以及开了
passthrough 的 tmux）为每位发言者分配八张内置
[动漫风格头像](assets/avatars/)之一，名字左侧 4×2 格——每张图只传输一次，靠
Unicode 占位符格子定位。team 成员的头像钉在 `.bingo/team.json`（`"avatar": "sora"`），
一支队伍就有固定班底；其余实例按名字取脸。不支持的终端保留首字母色块；两种皮肤行数一致，只有装订线
不同。

**主聊天**在 `experimental.chatAvatars`（默认关）后面用同一批脸：每条消息上面多一条
带子，头像挨着名字——hub 是 `main`，你自己
的消息是 `You`，都是房间里本来就用的名字。带子底下的正文一列没动，仍按整个终端宽度
排版，消息内部的 `⏺` 也仍然负责把正文和工具行分开。能放图的终端给两行带子，退化时
一行，带子底下没有东西依赖它的高度。一处已知退化：终端清空图片存储时（resize 会），
还在屏幕上的脸会重画，已经滚进 scrollback 的消息则留下 4 列空白，名字还在。开关关掉
则整条带子不出现，subagent 的 watch 行也保留 `◉`；开关只管主聊天，DM、频道、team
视图照旧带脸——那里的头像占的是排版本来就花掉的装订线。

## 技能（Skills）

- 加载顺序（优先级从高到低）：用户层 `~/.config/bingo/skills/` → 项目层
  `.bingo/skills/`（从 cwd 向上逐层，近者优先）→ 内置 `guide`（编译进二进制，
  仅作兜底）；同名磁盘技能覆盖内置。
- 每个技能一个目录：`<name>/SKILL.md`，YAML frontmatter
  （`description`/`when_to_use`/`arguments`）+ markdown 正文。
- 调用：模型经 `SkillTool` 自动调用；用户经 `/技能名 [参数]` 直接执行。
- 内置 `guide`：bingo 使用与诊断手册（回答"怎么配置/为什么/不工作"时对照）。

## 经验库（Experience）

跨会话沉淀项目内反复出现的操作经验：agent 反复做同一件事时，可提议、提交并
后续查询可复用的经验——价值跨会话复利。

- **存储**：`~/.config/bingo/experience/<项目键>/entries/<id>.md`
  （user 全局，绝不触碰项目工作区）；按项目隔离。
- **条目结构**：`trigger`（关键词）、`summary`、`steps`、`verify`、`evidence`
  （来源），以及显式 helpful/harmful 结果计数和用 SHA-256 绑定证据的仅追加结果历史——
  frontmatter + 自由正文。
- **工具**：
  - `ExperiencePropose`——生成带稳定 id 的候选条目；不写盘。
  - `ExperienceCommit`——持久化条目（过权限门）；相同内容映射同一 id，重复提交
    是更新而非重复；`status: stale` 标记失效后不再注入新会话但仍可查询。
  - `ExperienceQuery`——按任一 trigger 关键词子串匹配（不区分大小写）；
    active 排在 stale/degraded 之前，再按显式观察结果排序，旧的重复提交计数作为
    最后的兼容信号；结果暴露 outcome 计数和历史。
  - `ExperienceOutcome`——真正采用查询到的经验后，经权限确认记录 `helpful` 或
    `harmful` 与具体证据；它仅追加历史，不会自动改变生命周期 `status` 或
    `verified_at`。
  - `ExperienceForget`——删除条目。
- **状态生命周期**：`active` → `degraded` → `stale`；active 条目注入新会话，
  stale 仅可查询。

## MCP

`mcpServers` 配置，`rmcp` 官方 Rust SDK 驱动，连接后自动列出工具并适配为
bingo 的 Tool trait：

```json
"mcpServers": {
  "files": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."] },
  "remote": { "type": "http", "url": "https://mcp.example.com/mcp", "headers": { "Authorization": "Bearer xxx" } }
}
```

- 传输：stdio（缺省 `command`/`args`/`env`）与 streamable HTTP（`type: "http"`，
  可带自定义 headers 鉴权）。`sse`/`ws` 暂未落地，配置后连接时报错。
- 工具名：`mcp__<server>__<tool>`；权限规则用全名（如 `mcp__server` 前缀或
  完整工具名）。
- 诊断：`/mcp` 查看状态；stdio 服务器自身输出在
  `~/.local/share/bingo/logs/mcp-<name>.log`；修好后 `/mcp reconnect <name>`。
- 禁用/启用：`/mcp disable|enable [name|all]`（持久化到 settings.json）。

## 权限系统

### 权限模式（`--permission-mode` 或 settings 的 `permissionMode`）

| 模式 | 行为 |
|---|---|
| `default` | 只读工具直接放行；其余询问（可带规则表免问） |
| `acceptEdits` | 编辑类工具（Edit/Write 等）自动允许 |
| `plan` | 只读 + 任务列表管理；其余拒绝（计划模式） |
| `dontAsk` | 非只读一律拒绝（不询问） |
| `bypassPermissions` | 全放行（但 deny/ask 规则与敏感路径检查仍然生效） |

### 规则语法（settings `permissions` 段）

- 形式：`Tool(content)`；`:*` 为前缀通配（如 `Bash(git push:*)`）；`*` 匹配一切。
- **Bash**：按 shell 操作符（`&&` `;` `|` `$()` 等）切成子命令逐段匹配——
  deny/ask 任一子命令命中即生效；allow 需单条规则覆盖**全部**子命令才免询问；
  含未闭合引号的命令一律不自动放行。
- **文件类**（Read/Edit/Write/Grep/Glob）：路径归一化后前缀匹配
  （`~` 展开、相对路径按 cwd 展开、消解 `..`），`Read(src/)` 也匹配绝对路径。
- **WebFetch**：支持 `domain:` 规则与 URL 前缀；预批准域名自动放行。
- **Skill**：`Skill(name)` 精确、`Skill(name:*)` 前缀。
- **MCP**：不因服务器自报只读而免询问，需显式 allow。
- 顺序：deny → ask →（只读/预批准）→ 敏感路径检查 → bypass → acceptEdits →
  allow 规则 → 询问。deny/ask 规则在 bypass 模式下仍生效；
  写 `.git`/`.claude`/`.vscode`/`.idea` 等敏感目录的破坏性操作必须提示。

示例：

```json
"permissions": {
  "allow": ["Read(src/*)", "Bash(git status)", "WebFetch(domain:github.com)"],
  "deny":  ["Bash(git push:*)", "Bash(rm -rf)"],
  "ask":   ["Bash(git push)"]
}
```

## Hooks

`hooks` 配置的事件：`PreToolUse` / `PostToolUse` / `PreCompact` / `PostCompact` /
`UserPromptSubmit` / `Stop` / `SessionStart` / `SessionEnd` / `TaskCreated` /
`TaskCompleted`。每个事件一组 `{matcher, hooks:[{type:"command", command}]}`：

- matcher 为整串锚定正则（`Edit\|Write`、`mcp__.*`），空 = 匹配一切；
  编译失败退回全等比较并告警。
- hook 以配置的 shell 执行（`-c` 风格；Windows 默认 PowerShell 用 `-Command`），
  stdin 喂事件 JSON（`hook_event_name`、`tool_name`、`tool_input`、
  `permission_mode` 等），stdout 回传 JSON。
- 退出码语义：0 = 成功；2 = blocking（stderr 注入模型 / 阻断本轮）；
  其他非零 = 仅用户可见、不阻断。
- `PreToolUse` 支持 `{"decision":"deny|ask","reason","updatedInput"}` 改写输入。
- 普通 hook 超时 60s（SessionEnd 1.5s 快速收尾），超时 kill 不留残留。

示例（PreToolUse 拒绝 Bash）：

```json
"hooks": {
  "PreToolUse": [{
    "matcher": "Bash",
    "hooks": [{ "type": "command", "command": "echo '{\"decision\":\"deny\",\"reason\":\"no\"}'" }]
  }]
}
```

## 会话、压缩与记忆

- **Transcript**：`~/.local/share/bingo/transcripts/<项目>-<ts>.jsonl`，
  每行一条 Message；坏行跳过不阻塞恢复。`--continue` 续最近会话，
  `/resume [名]` 列出/切换，`/rename` 重命名。
- **上下文预算**：窗口 200k，输出预算 64k，有效输入窗口 = 窗口 − 输出预算；
  自动压缩阈值 = 有效窗口的 90%（≈122k），提前 20k 提醒（`/context`）。
  压缩 = 摘要旧消息 + 保留最近 8 条；压缩切点安全推进到 tool_result 边界之外，
  避免孤儿 tool_result 导致 400。连续压缩失败 3 次熔断（`/compact` 手动触发）。
  非 Anthropic 端点（无 count_tokens）自动改用本地估算（字符数/4）。
- **记忆**：memdir 自动记忆（`~/.config/bingo/memdir/<项目名>-<路径哈希>.md`，
  完整路径哈希避免同名项目串味）+ 项目 CLAUDE.md 与 AGENTS.md 作为 system 记忆。

## 架构

```text
CLI (clap)
  → settings 三层合并 (user/project/local)
  → Messages API 客户端 (reqwest + SSE 流式)
  → query loop: 工具调用 → 权限门 → 并发执行 → 结果回填
  → TUI (ratatui inline/fullscreen + crossterm) | headless --print
       ├─ Tool Registry (trait + schemars schema)
       ├─ MCP 适配层 (rmcp: stdio / streamable HTTP)
       ├─ 子代理 (hub-and-spoke, 异步 + 通知)
       ├─ Hooks (shell, JSON 契约)
       ├─ Task 存储 / 频道 / 技能 / 记忆 / transcript
       └─ 预算监控与压缩
```

核心循环语义：**模型只产出 tool_use 意图；权限、并行、副作用、压缩、记忆与
UI 由本地 harness 负责**。设计决策见 [`notes/research.md`](notes/research.md)
（D1–D36）。

## 项目结构

```text
src/
  main.rs          CLI 入口（clap）、会话启动链
  api/             Messages API 客户端（client / SSE / types）
  query.rs         主循环（queryLoop）、slash 可变运行时
  tools.rs         工具装配（按 depth/实验开关分发）
  tool/            各工具实现 + Tool trait 契约
  permission.rs    统一权限门（模式 × 规则表）
  hooks.rs         shell hooks（事件 / matcher / JSON 契约）
  agents.rs        子代理会话与历史、具名定义加载
  tool/agent.rs    Agent / SendMessage / AgentControl 实现
  team.rs          team 解析/校验/拉起编排 + team 记忆（D31）
  team_cmd.rs      /team 命令族
  tool/team.rs     Team 工具（模型侧，变更须用户确认，D46）
  experience.rs    跨会话经验库
  tool/experience.rs  ExperiencePropose/Commit/Query/Outcome/Forget 工具
  channels.rs      频道注册表（实验特性）
  tasks.rs         任务存储（Task 工具族）
  skills.rs        技能加载 / frontmatter / 参数替换
  mcp.rs           MCP 管理器（stdio / streamable HTTP）
  settings.rs      三层配置加载与合并
  transcript.rs    会话持久化（JSONL）
  compact.rs       自动/手动压缩
  budget.rs        token 预算常量
  memory.rs        memdir 记忆提取与加载
  watch.rs         后台任务注册与通知
  tui/             ratatui 界面（chat / view / input / markdown / gfx …）
  ui.rs            headless hooks 与共享渲染
  system.rs        system prompt 拼装（记忆 + 项目记忆 + 技能清单）
tests/
  fixtures/        集成测试夹具
notes/
  research.md      技术决策记录（D1–D36）
```

## 开发约定

- Rust 2024 edition；错误处理用 thiserror，生产代码不 unwrap/expect。
- 代码写成周围代码的样子；无注释优先，注释只解释"为什么"。
- 不加不需要的依赖；造轮子前先看 crates.io。
- 改动涉及用户可见行为（配置项 / slash 命令 / 工具 / 错误信息 / 能力地图）时，
  同步更新内置技能 `src/skills/bundled/guide.md`（AGENTS.md 的同步规则）。
- 改动架构前先对表 `notes/research.md` 的决策记录。
- 每次改动跑 `cargo build` 与 `cargo clippy -- -D warnings`，相关逻辑必带测试。
