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
- **子代理**：主 agent 派生命名子代理，异步执行、完成通知自动注入上下文；
  SendMessage 是唯一的发言工具——有名字的任何人都能写给有名字的任何人（D137），
  同侪之间直接对话；AgentControl（仅主会话）管理生命周期。
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
  30 天 TTL + 最近 100 个非活跃会话的有界保留策略（24 小时活跃保护，`/gc`），
  memdir 自动记忆 + CLAUDE.md/AGENTS.md 项目记忆。
- **Hooks 扩展点**：工具前后、会话起止、压缩、Stop、任务生命周期等事件的
  shell hook（stdin 喂 JSON、stdout 回传决策）。
- **一个内核，三个前端**：终端、`bingo app-server`（JSON-RPC over stdio，
  实验特性）与 `--print` 都是同一个会话 actor 的投影——GUI 驱动的是这个产品
  本身，而不是把它重写一遍。

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
bingo app-server            # 面向 GUI 的 JSON-RPC over stdio（实验特性）
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
| `bingo app-server` | 在 stdio 上提供 app-server 协议（见 [App-server](#app-server实验特性)） |
| `bingo app-server generate-schema --out <目录>` | 生成该协议的 JSON Schema bundle |
| `prompt` | 非交互提示词（缺省从 stdin 读取；交互模式忽略） |

## 使用界面

### 输入

- `Enter` 发送；`\`+Enter 或 Ctrl+J 换行（多行输入）。
- 输入 `!` 进入 bash 模式：命令直接执行、不经模型（`!echo hello`）；
  前缀粘性保留；空输入 Esc/退格/Ctrl+U 退出。交互式/TTY 命令
  （top/vim/ssh/fzf 等）会被拒绝，请用批处理等价物（`top -b -n 1`）。
- 大段粘贴自动折叠为 `[Pasted text #N +M lines]` 占位，发送时展开真实内容。
  粘贴不是打字：其中的换行仍是换行而不会发送，`@` 与 `/` 只是字符，不弹下拉。
- `Ctrl+R` 历史反向搜索；`↑↓`（或 `Ctrl+P`/`Ctrl+N`）历史回溯（多行输入内先移光标）。
- `Ctrl+S` 暂存/恢复输入、`Ctrl+Y` 粘回删除（紧接 `Alt+Y` 轮换 kill ring）、`Ctrl+_` 撤销。
- `Ctrl+G`（或 readline 组合键 `Ctrl+X Ctrl+E`）用 `$VISUAL`/`$EDITOR` 编辑草稿，
  保存后的内容作为一步撤销替换输入。
- 词首输入 `@` 打开 mention 下拉：项目文件（git 仓库内取已跟踪与未忽略的未跟踪文件，
  否则走有界目录遍历）与正在运行的 agent，按 `@` 之后的内容模糊过滤；
  `Tab`/`Enter` 插入相对于会话目录的路径，agent 则插入 `@name`。
  斜杠命令写完命令名之后，同一个下拉改为补全**参数**——`/model`、`/theme`、
  `/think`、`/resume`、`/provider login`——取值一律来自命令自身校验所用的同一份数据。
- 前台运行的命令会在自己那一行下方实时显示最后五行输出；`Ctrl+B` 把它转入后台
  且不重启进程：工具调用立即返回 task id，完成时按后台任务通知送达。

### 快捷键（空输入按 `?` 看全表）

| 键 | 功能 |
|---|---|
| `Esc` | 先关最上层弹窗/菜单/面板 / busy 时中断 / 双击清空输入，输入为空时双击打开 Rewind |
| `Ctrl+C` | busy 中断 / 有文本清空 / 空输入连按两次退出 |
| `Ctrl+T` | 显隐任务区 |
| `Ctrl+O` | 打开 transcript 视图：整段会话连同全部工具输出，独占一屏（`ctrl+e` 折叠 · `/` 搜索 · `o` 打开视野内的图片 · `q` 关闭） |
| `Ctrl+G` | 用 `$VISUAL`/`$EDITOR` 编辑当前草稿（也可用 readline 组合键 `Ctrl+X Ctrl+E`）；编辑器非零退出则保留原草稿 |
| `Ctrl+P` / `Ctrl+N` | 提示历史——与 `↑`/`↓` 完全同键，包含把排队消息取回 |
| `Alt+B` / `Alt+F` | 按词移动，在 `/` `-` `_` `.` 处停下，便于逐段走过路径 |
| `Alt+D` / `Alt+Backspace` | 向后/向前删一个词（`Ctrl+W` 仍删整个空白分隔的词） |
| `Ctrl+K` / `Alt+K` | 删到行尾 |
| `Ctrl+Y` / `Alt+Y` | 粘回最近一次删除；紧接着按 `Alt+Y` 在 10 条 kill ring 中轮换 |
| `Shift+Enter` | 插入换行（终端支持 kitty 键盘协议时可用） |
| `Ctrl+B` | 把正在前台运行的命令转入后台；没有命令在跑时打开后台对话框——agents / shells / rooms 三段（`Enter` 看详情，`f` 切到前台，`x` 停一个 agent） |
| `Ctrl+L` | 清屏重画 |
| `@` / `#` | 行首时把余下内容直接发给该 agent 或房间；行中 `@` 仍是 mention 项目文件或运行中的 agent（模糊下拉，`Tab`/`Enter` 插入） |
| `Tab` | 补全斜杠命令、命令参数、选中的 mention，或 `!` shell 历史前缀 |
| `Shift+Tab` | 循环权限模式（default → acceptEdits → plan）；审批对话框中直接选中「本会话不再询问」 |
| `Ctrl+E` | 审批对话框中展开完整命令/diff 预览与将写入的会话规则 |
| `Alt+T` | 思考开关 |
| busy 时回车 | 消息排队；正在运行的回合会在下一次工具调用处把它并入，否则回合结束时发送 |

### Slash 命令（`/help` 全量清单）

`/model [名]`（无参进入 provider → 模型两级选择器；provider 与模型作为
一对持久化）、`/provider [名称]`（列出/切换多 provider；`/provider login
<名> [--device-auth|--manual <token>]` 登录订阅端点、`logout` 退出）、
`/think [off|low|medium|high|xhigh|max]`（无参进入等级选择器，选择持久化）、
`/theme`、`/permissions [allow|deny|ask <规则>]` · `/permissions remove <allow|deny|ask> <规则>`、
`/mcp`（状态）· `/mcp enable|disable [name|all]` · `/mcp reconnect [name]`（省略名字 = 重连全部已启用服务器）、
`/skills`（清单，`/技能名` 直接执行）、
`/join #房间`、`/leave #房间`（加入房间以便发言，或退出）、
`/context`（用量）、`/status`、
`/config`（生效配置与来源：哪个层/环境变量赢了、当前端点、未知配置项提示）、
`/compact`（强制压缩）、`/resume [名称]`（恢复历史会话）、`/rename`、
`/gc`（清理过期会话数据）、`/share [--public] [--open]`、`/clear`、`/exit`。
`/share` 默认只在本地生成自包含 HTML；只有显式加 `--public` 才会上传为
任何人可访问的公开链接，且上传前会先显示敏感内容警告。`--open` 打开本地文件
或已发布链接。会话键缺省取最近**实际用过**的会话——启动即退出留下的空 transcript
会被跳过，与 `--continue` 同一套过滤。等价 CLI 为
`bingo share [会话] [--public] [--open] [-o 路径]`。
`/team`（项目团队）：`list`（图纸+运行区同屏）、`start`（拉起/幂等复用）、
`status`、`assign <成员> <任务>`（派活）、`stop`、`validate`、`new`
（脚手架生成 team.json + team-norms.md）、`norms`（团队规范）、`memory list|gc`。

### 主题、代码与 diff

两套主题全部以 RGB 写死，所见即 bingo 的调色板，而不是终端的 ANSI 映射（不支持
truecolor 的终端会拿到同一组颜色的 256 色近似）。文字只落在三档上：正文用一档，
说明正文的文字（结果行、工具输出、diff 上下文）用二档，纯装饰（提示、时间戳、
分隔线、diff 行号栏）用三档。

围栏代码块在标注了语言时高亮——`rust`、`python`、`javascript`/`typescript`、
`json`、`bash`/`sh`、`toml`、`yaml`、`markdown`、`diff` 及另外十余种；未知或缺失
的语言标签保持单色，不做猜测。diff（审批预览、完成的编辑行、transcript 视图三处
同源）带 old/new 行号栏，过长的行折行显示且续行的行号栏留空，代码列始终对齐。
`/theme` 切换即时生效。

### 图片渲染

模型回复中的 markdown 图片（`![alt](路径)`，支持 `~/`、相对路径、data:、http(s)）
在支持 kitty graphics 的终端（Ghostty/kitty 等）两种模式下都渲染真实图片：
fullscreen 在活动视口内放置图片，`--inline` 还会在内容落盘时写入 scrollback。
不支持的终端显示 `#[image]` 占位。tmux 内 bingo 会自动开启 passthrough
（`tmux set -p allow-passthrough on`），落盘图片在 Ghostty/kitty 外层终端下以
Unicode 占位符（U=1）渲染；WezTerm/Konsole 虽支持 graphics 协议但不支持占位符，
tmux 下仍显示 `#[image]` 占位（tmux 内的活动视口同样保持占位）。

## 配置（settings.json）

三层配置逐键合并、后层覆盖前层，有四个例外：`permissions` 三张表与
`disabledMcpServers` 跨层**累加**（local 层删不掉 project 层的 deny）、
`providers` 按 provider 名逐个合并、`experimental` 开关任一层开了就开
（`mcpServers` 整体替换）。UI 内的选择（/model /provider /theme /think）
写回「生效层」：某层已定义该键则更新该层，否则写 user 层——不会在任意目录凭空创建
`.bingo/`；`/permissions` 与 `/mcp disable` 属项目级状态，仍写项目层。

1. **user**：`~/.config/bingo/settings.json`（`XDG_CONFIG_HOME` 优先）
2. **project**：`.bingo/settings.json`（入库，注意别提交密钥）
3. **local**：`.bingo/local.json`（个人覆盖，不入库）

| 配置项 | 类型 | 说明 |
|---|---|---|
| `apiKey` | string | API key（settings 优先于 `ANTHROPIC_API_KEY`/`DEEPSEEK_API_KEY`） |
| `apiBaseUrl` | string | API 端点（settings 优先于 `ANTHROPIC_BASE_URL`；缺省官方） |
| `providers` | object | 命名 provider：`{名: {protocol?, apiKey?, envKey?, apiBaseUrl?, supportsImages?, oauth?, models?}}`，`/provider <名>` 切换；`protocol` 为 `"anthropic"`（缺省）或 `"openai"`（Responses API，Bearer 认证；`apiBaseUrl` 缺省 `https://api.openai.com`）；`envKey` 写环境变量名取 key（凭据顺序 `apiKey` > `envKey` > 存储的 key / OAuth）；`oauth: {kind: "codex"}` 启用 OAuth 登录（`/provider login`，apiKey 优先） |
| `model` | string | 默认模型（`/model` 选择写入）；优先级 `--model` > settings > 内置 `claude-sonnet-5` |
| `provider` | string | 当前 provider（`/provider` 与 `/model` 菜单持久化；缺省 `"default"` = 顶层 `apiKey`/`apiBaseUrl`）；名字失效时回落 default 并告警 |
| `sendImages` | bool | 默认端点是否发送消息框里的图片附件（默认 true；命名 provider 用各自的 `supportsImages`） |
| `models` | array | 默认 provider 的模型清单；各 provider 写在 `providers.<名>.models`。条目是模型 id（`"gpt-5.6-sol"`）或对象（`{id, display?, contextWindow?, maxTokens?, thinking?, vision?}`）。声明即权威：`/model` 只列这些且零请求，元数据覆盖内置表。`maxTokens` 是模型的输出上限——既作请求的 `max_tokens` 发出，也从输入窗口里预留出来，并 clamp 到窗口的一半，小 `contextWindow` 也留得下工作余量。`vision` 声明模型是否接受图片输入——系统提示会告诉模型自己的能力，无 vision 的模型发出的请求会丢弃图片块（与端点级的 `sendImages`/`supportsImages` 发送闸是两回事）。未声明的 provider 动态拉 `/v1/models`，结果落盘缓存 24 小时（菜单里 `r` 重拉） |
| `thinkingLevel` | string | `off` 不发 thinking 参数（兼容 DeepSeek，缺省）；`low`/`medium`/`high`/`xhigh`/`max` 发自适应 thinking + 对应档位的 `output_config.effort` |
| `permissionMode` | string | `default` / `acceptEdits` / `plan` / `dontAsk` / `bypassPermissions` |
| `theme` | string | `auto`（跟随终端背景）/ `dark` / `light` |
| `motion` | string | `auto`（缺省）/ `off`——一个开关管住全部动画表面；携带信息的两处颜色照常变化。环境变量 `BINGO_NO_MOTION`（任意值）等价 |
| `notifications` | string | 提醒通道：`auto`（缺省，按终端探测）/ `bell` / `iterm2` / `kitty` / `ghostty` / `off`。四个触发点：权限询问等待中、长回合结束、回合失败、agent 需要你 |
| `cacheControl` | bool | 发送 prompt caching（默认关：非官方端点不稳定） |
| `bashOutputMaxChars` | integer | Bash 工具返回的 stdout/stderr 合并字符上限（默认与上限均为 48,000） |
| `share` | object | `{baseUrl}`——覆盖分享服务地址（缺省 `https://bingo.ruobin.dev`） |
| `respondToBashCommands` | bool | `!` 命令执行后是否交模型回应（默认 true） |
| `shell` | string | Bash 工具与 hooks 使用的 shell。默认按平台：macOS `/bin/zsh`、其他 Unix `/bin/bash`、Windows `powershell.exe`。PowerShell 系用 `-Command` 执行；配置其他 shell（如 Git Bash 的 `bash.exe`）用 `-c` 执行 |
| `mcpServers` | object | 见下「MCP」 |
| `disabledMcpServers` | string[] | 禁用的 MCP 服务器名单（`/mcp disable` 写入） |
| `permissions` | object | `{allow[], deny[], ask[]}`，规则语法见「权限系统」 |
| `experimental` | object | 实验特性：`agentChannels`、`channelMessageLimit`（默认 500）、`agentMessageLimit`（默认 50）、`chatAvatars`（默认 false；所有头像的唯一开关——关 = 任何地方都没有头像装订线、色块与 watch 行头像，开 = 终端能画的地方都画） |
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

### 模型目录（model-catalog.json）

`~/.config/bingo/model-catalog.json`（首次启动生成，与 settings.json 同目录）存放按模型 id **前缀**
归类的家族默认值——`contextWindow`、`maxTokens`（输出上限）、`thinking`、`vision`——最长前缀逐字段胜出。
两段各有其主：

- `builtin` 归 bingo：编译进二进制的调研默认值的镜像，升级时重写，修正过的数字随版本到达；
  在这一段做的修改下次启动会被还原。
- `overrides` 归你：bingo 永不改写，生效层级在 settings `models` 声明与内置表之间。完整模型 id
  就是最长的前缀，所以 `"overrides": {"deepseek-v4-flash": {"maxTokens": 32000}}` 只抬高这一个
  型号的输出上限，`"deepseek"` 条目继续管家族里的其他型号。

逐字段优先级：settings `models` 声明 → `overrides` → 内置表 → 保守默认。文件损坏时降级用内置值并在
启动时告警，绝不覆盖改写（删除文件可重新播种）。

## 工具集

全部经统一 Tool trait（serde/schemars 生成 schema，单一来源）：

| 工具 | 说明 |
|---|---|
| `Bash` | 在独立进程组（Unix）/ 进程树（Windows）执行 shell 命令；超时/取消整树终止，不留孤儿进程；非交互命令 |
| `Read` / `Glob` / `Grep` | 只读检索；默认跳过 `.git`/`target`/`node_modules` 与隐藏目录 |
| `Edit` / `Write` | 文件编辑（产生 unified diff 供 UI 预览） |
| `WebFetch` / `WebSearch` | 网页抓取与搜索（共享 HTTP 连接池；预批准域名自动放行） |
| `Agent` | 派生命名子代理（异步执行，完成通知注入上下文；`background:false` 可同步等待） |
| `SendMessage` | 唯一的发言工具：`to` 是 agent（`name` / `@name`）或房间（`#name`）；名册上有名字的任何人都可写给有名字的任何人（D137）——同侪消息带 `[message from @name]` 标记到达；房间需要成员身份 |
| `AgentControl` | 子代理生命周期管理（仅主会话装配） |
| `Team` | 项目编队（仅主会话装配）：`status`/`validate` 只读免询问，`start`/`stop`/`save` 在任何权限模式下都要用户当面确认 |
| `TaskCreate`/`TaskUpdate`/`TaskGet`/`TaskList` | 任务追踪（磁盘存储，TUI 任务区同源，含生命周期 hook） |
| `ExperiencePropose`/`ExperienceCommit`/`ExperienceQuery`/`ExperienceOutcome`/`ExperienceForget` | 跨会话经验库（见下） |
| `AskUserQuestion` | 向用户提选择题（TUI 复用权限询问模态） |
| `Skill` | 技能调用（见下） |
| `mcp__<server>__<tool>` | MCP 接入的工具（见下） |
| `Channel` | 实验：房间管理（见下） |

## 子代理

- 主 agent（depth 0）装配 `Agent`/`AgentControl`；子代理（depth ≥ 1）只保留
  `Agent`（可再派生），无法管理兄弟——生命周期控制仍在枢纽。发言不再是（D137）：
  `SendMessage` 让名册上有名字的任何人写给有名字的任何人，两名成员直接把事情谈清，
  不必绕经管理者——同侪消息带 `[message from @name]` 标记到达，用
  `SendMessage(to: "@name")` 回答而不是写在回合文本里（同事读不到你的散文，
  那是交回给 main 的结果）。仍然把守的是发送者：名册上没有名字的会话只能写
  `main`，房间需要成员身份。
- **具名定义**：`~/.config/bingo/agents/*.md` 与 `.bingo/agents/*.md`
  （从 cwd 向上逐层查找，同名项目层优先）；frontmatter
  `name/description/model/provider/thinking/inherit_system`，正文 = 子代理
  system prompt；Agent 工具的 `agent` 参数引用定义。
- 派生实例有名字（`name` 参数，缺省取定义名/`agent`，重名自动 `-2`/`-3`），
  派生它的那一轮显示为 `◉ @名字: 任务`，运行期间行下挂着它最近做的三件事（窗口太矮
  时收成一行 `In progress… · 4 tool uses · 8.3k tokens`），结束后定格为
  `Done (12 tool uses · 8.3k tokens · 1m 4s)`；一轮里派发多个时合成一块
  `⏺ Running 2 agents…`，每个 agent 一行 `├─ @名字: 任务`。而在没有轮次运行时到达的
  生命周期事件不再写进 @main——改由会话行与 `Ctrl+B` 对话框里它自己那一行
  承载；完成后历史保留。
- `SendMessage` 向实例发后续指令（上下文保留）；实例忙时排队，当前回合结束
  自动送达。子代理的 `SendMessage(to: "main")` 落进主 agent 的收件箱，主 agent
  空闲时被唤醒——而在 @main **什么都不画**（D114）：这封信是 main 的邮件，不是
  用户的对话，用户看到的是状态层里发送者亮起的邮件信号。房间转发同样什么都不画。
  `urgent: true`（仅子代理→main）在到达那一刻额外触发终端提醒通道。
- 运行**失败**时在 @main 画一行 `⚠ @名字 · 原因`并触发提醒；main 自己这一轮派发的
  运行**完成**时留下一行暗色 `● @名字 completed · 任务`——投递唤醒的运行（房间发言、
  排队消息）完成时只进会话行与对话框，不进流；取消什么都不画。
  完全由用户自己直接写给实例而触发的那一轮，既不产生通知，也不唤醒主 agent。
- 被唤醒的那一轮（消化通知，而非用户发问）和别的回合一样收尾：说出来的话在 `@main`
  里就是主 agent 在说话。降噪靠的是唤醒去抖与派发行自身的状态，而不是一个渲染成
  空白的标记。
- `AgentControl` 可 `list`/`stop`/`delete`。
- 默认异步执行：立即返回实例名与 task_id，完成时自动通知注入下一轮上下文。

## Agent 团队（项目级）

团队把一组角色固定到一个项目：声明式编排层，成员引用具名定义（AgentDef）、
房间复用频道机制、生命周期控制仍在主会话。

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
- **固定团队优先用工**：项目钉了团队，main 的系统提示里就带着这份名册和随之而来的
  规矩——活先用 `SendMessage` 交给队内匹配的成员，只有队里没人覆盖的工作才另派
  子代理。给一个正闲着的成员再派一个替身，既浪费你在付钱养的队伍，也丢掉了它
  已经攒下的记忆。
- **临时招募不进队**：有固定团队时，Agent 工具派生出来的是「临时工」而非成员。
  它不会写进 `.bingo/team.json`；在 `/team list` 与 `AgentControl list` 里与队伍
  分开列（`crew` / `hire`）；会以 `type: hire` 记进队伍的 `decisions.md`；任务
  完成即回收——空闲、收件箱为空、没有欠着的回复，并留给 main 一轮追问的窗口。
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
  衰减很快的相关性。main 自己每个 session 也是干净启动的，crew 成员没有理由例外。
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

- 主 agent 与直接子代理都获得 `Channel` 工具：建频道、进出成员（建房只把发起者
  坐进去；成员是直接子代理加上 `main` 与 `user`，都要被点名才在场）；成员用
  `SendMessage(to: "#房间")` 发言，消息进全体成员上下文
  （同序）。主 agent 自己那一份按去抖消化：一阵密集发言只买一轮，而不是一条一轮。
  点到成员名字的 `@` 是一笔记录在案的债（D131）：直到它在那个房间发言才销账，
  会话行看得见谁欠着；五分钟无人应答，看门狗会把原话引回房间再问一次。
- `serial` 频道落后发言被弹回并附新增消息（agent 阅读后自行改口，报数式顺序
  由此涌现）；`free` 频道允许交叉。
- 超限自动冻结频道并通知主 agent（`channelMessageLimit`/`agentMessageLimit` 预算闸）。
- 频道在 transcript 显示为 `◇ #名字` 行；在输入框打 `#名字 <消息>` 即以 user 身份发言。

## 一条 transcript，以及能离开它的那一行

一个终端、一个会话：你和主 agent 的对话。流就是这份 transcript 本身，按顺序排列——
不会有别的东西插进来，也不会有回放；往回翻看到的是一条线索，而不是来回穿插的编织。

**不离开也能对别人说一句**：以名字开头的一行就是**直发**。`@scout 看一下 parser`
把后面的内容以**你的名义**投进 scout 的收件箱，完全绕过模型——空闲或已停止的实例会
被唤起，运行中的实例在下一次工具轮次取走。`#build 测试全绿` 以 `user` 身份发到那个
房间；你不是成员时会先把你加进去，并在房间自己的日志里写明。你得到的回执是输入框上方
一行**瞬时**的 `Sent to @scout`：既不进流，也不进模型的历史，下一次敲键就消失。

**名字对不上就是普通文本。**`@utils 解释一下这段代码` 既不是错误也没有魔法——原样发
给模型，开一轮普通对话。

下拉会说清楚现在是哪种。行首的 `@` 列出直发能触达的每个实例——
`@scout · send message · running`，已停止的也在列，因为一条消息就能把它唤起——项目文
件排在下面；`#` 列出房间，写作 `#build · post to room`，你不在其中时补一句
`· joins you`。行中的两个符号含义不变：`@` 是文件或 agent 引用，`#` 就是句子里的井号。

### Rewind（回退）

输入框为空时连按两次 `Esc`，bingo 会列出你开过的回合，最近的在上，并标出每个
回合及其之后一共动过几个文件。选中一个，它再问「回到」是什么意思：

1. `Restore code and conversation`（代码与对话一起回退）
2. `Restore conversation`（只回退对话）
3. `Restore code`（只回退代码）
4. `Summarize from here`（把这里往后总结成一段）
5. `Never mind`（算了）

**只回退对话**把会话历史截断到那条消息为止，并把它的原文放回输入框，方便换一种
问法。**只回退代码**把文件恢复成那个回合开始时的样子——回合新建的文件会被删掉，
改过的文件会还原——对话原样不动。**总结**用一段摘要替换那个回合及其之后的全部
内容，适合只要结论不要过程的时候。

前像由 `Edit` 和 `Write` 在动手之前采集，每回合每个文件只采一次，存放在
`~/.local/share/bingo/rewind/`，与 git 无关，每个会话上限 50 MB 或 200 个回合，
超出按最旧优先淘汰。某一半不可用的选项会置灰并说明原因。**不覆盖的部分**：
`Bash` 命令写下的任何东西。shell 能以任意方式改任意文件，动手之前没有前像可采，
所以那些改动会留在原地。

## 房间与团队（Rooms & the team）

**房间**是 bingo 里唯一的群聊，成员是团队的任意子集——不一定包含你：agent 之间会
自己开房间把事情谈清楚。建房间只会把发起者坐进去，`user` 和 `main` 只有被点名时
才在场。

在输入框打 `#名字 <消息>` 就能在房间里发言。你不是成员时，这一句会先把你加进去——
没有悄悄潜水再开口这回事：进出都会写进房间，成为每位成员都看得见的暗色行
`· user joined · 14:32`。`/join #名字` 是不说话地成为成员，`/leave #名字` 是它的
反面，退出后房间仍然可读。`Ctrl+B` 对话框的 Rooms 一段会列出每个房间及其成员，
你不在其中的标 `you're not in`。

**会话行**是回答「谁在干活」的那一层——D115 之后,也回答「你没看的时候谁说了话」。
它们常驻在输入框下方,一旦有人存在就一直在:`● @main` 打头——main 工作时圆点是实心
的——然后是每个实例一行(运行时 `● @scout: reading src/lib.rs…`——tool 数与 token
统计属于派发行和 `Ctrl+B` 详情,不在这里——等待时 `Idle for 14s`,停止后
`[stopped]`,欠着房间里一个 `@` 未答时是 `owes #build #7`),再后是你在的每个房间
(`#dev-team: 3 members`,有 `@` 悬而未答时是 `waiting on @dev · 3m`),名字各用自己
的身份色,最多同时显示三行,光标移动时窗口
跟着滚(边缘行右侧标 `↓ 2 more`)。每一行都戴着它那个会话的**徽章**:有未读是一个
点(`•`),点名到你——房间发言里 `@user` 或 `@all`——是强调色的计数(`•3`),并且每次点亮响
一次铃,读过房间才会再响;等你授权的 agent 行会变成强调色的
`waiting on you (permission)`。没有需要学的快捷键:提示历史翻到底时再按一次 `↓`
就落到行上(与 Claude Code 同款的三级下落),`↓/↑` 在行间移动,`Enter` 打开那一行
的会话页,`k` 停掉选中的运行中实例,顶部再按 `↑` 或 `Esc` 回到草稿,敲任何字母都直
接回到输入。进入即已读,读过徽章即清。任务面板(`Ctrl+T`)会把仍在名册上的负责人标
成 ` (@scout)`(用他的颜色),把被挡住的任务标成 ` › blocked by #3`——只是显示:这
里不指派、不认领、不解除阻塞。

**页**把任何一个会话整屏放在你面前,想看多久就看多久——画它的正是画 `@main` 的那条
管线,随着内容落定同样写进终端自己的 scrollback。在会话行上按 `Enter`,或在
`Ctrl+B` 对话框里按 `f`,屏幕就翻页:先是一行 `── @scout ──`,然后是这个 agent 的
**完整记录**,按顺序——创建它的任务、main 给它的指令、你自己发的消息、它的工作(折
叠方式与 `@main` 相同)、它的回答,以及此刻正在流式产出的那一轮。切换即翻页:离开
的那一页整体存进 scrollback,新页从屏幕顶端开始;回到 main 时重打最近一段尾巴。
**输入框保持可用,并且指向这个 agent**,边框与 `❯` 都染上它的身份色:你打的字以你
自己的名义进它的收件箱,以「排队中的消息」的样子出现在页上。**控制台自己的语法在页
上照常生效**(D135):`/` 是命令,`!` 是 shell 模式——只有散文跟着页走。命令作用于
控制台的会话,唯一例外是 `/compact`:在 agent 页上压缩的是**这个 agent 的**上下文
(它的回合运行中会被拒绝)。**房间页只有发言**——成员 `SendMessage` 到房间的内容,每条压着
发送者的名字;进出记录留在日志里、不上页;打字就是发到房间,不是成员时先把你加进
去。**`Esc` 有四重含义,按这个顺序**:先停掉*正在运行*的这一页的主人(历史保留,页
不关),再退出已进入的 shell 模式,再清掉未发的草稿,最后才回家——页开着时 main
自己的回合够不到 `Esc`(`Ctrl+C` 仍然是万能
打断)。`Shift+Tab` 轮换的是**这个 agent 的**权限模式,底部徽章跟着它走。页的主人
离开名册时页自己关闭;而一个*已完成*的 agent 的页会留着,因为读它正是目的。

**团队不是一个会话**——你没法对它说话，所以它是一份名册，而不是带未读徽标的看板。
前台没有命令在跑时按 `Ctrl+B` 打开**后台对话框**：标题 `Background tasks`、下面是
正在运行的计数，然后是有内容的那几段：**Agents** 是花名册和每个人正在做的事，未读
会以 `(3 unread)` 挂在行尾（点到你名字的那条用强调色）；**Shells** 是后台命令及其
状态；**Rooms** 是每个房间的成员与消息数，你不在的标 `you're not in`。只有一种以上
时才会出现分段标题，全空时只说一句 `No tasks currently running`。底部一行是
`↑/↓ to select · Enter to view · f to foreground · x to stop · ←/Esc to close`，
`f` 与 `x` 只出现在真的可以这么做的行上。行序是运行中优先、然后按最近变动排，光标
跟着**它那一行**而不是位置走，所以 `x` 停不掉刚滑到光标下的东西。`Enter` 打开的详
情会替换列表（实例的活动、开销、进度与提示词；shell 的状态、运行时长、命令与输出
尾巴；房间的成员、条数与最近几句），`←` 返回，`Esc`/`Enter`/`Space` 关闭。常驻版本
的同一份花名册是输入框下方的会话行，它以 `@main` 打头；对话框不列 main——它既不
可停止也无处可切，它的会话就是你按下这个键时看着的那一屏。

**流为一段它并不亲历的生命所展示的**，一共四层，不多不少。**派发**是
`◉ @scout: fix the parser`，运行期间行下挂着这个实例最近做的三件事——窗口太矮时收
成一行 `In progress… · 4 tool uses · 8.3k tokens`——运行结束时定格为
`Done (12 tool uses · 8.3k tokens · 1m 4s)`，进入 scrollback 的正是这一行。一轮里
派发多个则合成一块 `⏺ Running 2 agents…`，每个 agent 一行 `├─ @名字: 任务`。
**完成**多一行暗色 `● @scout completed · fix the parser`——只属于这一轮自己的
`Agent` 派发；投递唤醒的运行（房间发言、排队消息）完成进树与对话框，永不进流
（D114）。**失败**多一行
`⚠ @scout · connection reset` 并触发提醒通道。除此之外——实例启动、转为空闲、被停
止、房间里的一条发言、agent 寄给 main 的信——什么都不写：状态是树的事，邮件是 main
的事，而每条房间发言都画一行正是消化去抖存在要挡住的那种洪水。

**谁说了什么由一次遍历决定**，因为发送者不是一个字段：吸收进来的收件箱是一整段平铺
的提示词，能留下来的只有那些字面标记。agent 的页保留这次遍历找到的每一个对端；未读计
数只保留一个，就是你和这个 agent 的那一对。所以创建实例的那个任务、main 给它的指
令、房间转发、别的 agent 寄来的信、催它回话的追问，都是别人的会话，不计入你的未读
——而默认归属会随记录属于谁而翻转：在实例的历史里，没有标记的散文是主 agent 在说
话；在主 agent 自己的历史里，那就是你。

**头像**（`experimental.chatAvatars`，默认关——所有头像的唯一开关）：开启后，能放置
kitty 图片的终端（与内联图片同一能力：Ghostty/kitty，以及开了 passthrough 的 tmux）
为每位发言者分配八张内置[动漫风格头像](assets/avatars/)之一，名字左侧 4×2 格——每张
图只传输一次，靠 Unicode 占位符格子定位。team 成员的头像钉在 `.bingo/team.json`
（`"avatar": "sora"`），一支队伍就有固定班底；其余实例按名字取脸。不支持图片的终端
保留首字母色块，subagent 的 watch 行用它的头像替换 `◉`。开关关着（默认）则任何地方
都没有装订线、色块与 watch 行头像——身份色不受影响，因为颜色不是头像。

**开启后每个会话都戴，`@main` 也不例外**：脸放在左侧装订线里——正文换行之前先从宽度里
扣掉 4 到 5 格——同一个人连续说话时只有第一行带头像，之后各行留空；工具步骤与系统行
只取缩进、不带脸。main 有一张专属头像，任何队友既分不到也钉不上，所以每次开机的
控制台都是同一张脸。一处已知退化：终端清空图片存储时（resize 会），还在屏幕上的脸
会重画，已经滚进 scrollback 的行则留下空白列。被合成一块的派发里的行不戴头像,
因为树枝加上身份色的名字已经说清了是谁。

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
  `~/.local/share/bingo/logs/mcp-<name>.log`；修好后 `/mcp reconnect [name]`
  （省略名字 = 重连全部已启用服务器）。
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

### 审批对话框

需要询问时，对话框先展示「将要发生什么」——Bash 的命令行、Edit/Write 的
试运行 diff（不落盘计算）——再给三个选项：

1. `Yes`
2. `Yes, and don't ask again this session`——`Shift+Tab` 可直接选中。
   仅当权限引擎能推出真正管用的最窄规则（`Bash(cargo:*)`、`Edit(/路径/)`、
   `WebFetch(domain:…)` 或裸工具名）时才出现；你自己的 `ask` 规则与敏感路径 /
   `confirm_reason` 检查排在 allow 之前，这类询问不显示该选项。
   规则只存在于本会话内存中，不写入 settings。
3. `No, and tell bingo what to do differently (esc)`——回车展开反馈输入行，
   输入的内容会随拒绝一起交给模型；任何位置按 `Esc`、以及空反馈提交，
   都是不带反馈的普通拒绝。

`Ctrl+E` 展开完整预览，并显示选项 2 将写入的会话规则。对话框出现后的
0.4 秒内忽略回车与数字键，避免已在途的按键误批。

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
  每行一条 Message；坏行跳过不阻塞恢复。会话一开始就把文件打开，因此
  还没说话就能被列出、恢复与重命名。`--continue` 续的是最近**用过**的会话
  （启动即退出会留下一个空文件，交给 `/gc` 回收），
  `/resume [名]` 列出/切换，`/rename` 重命名。启动清理与 `/gc` 最多保留最近
  100 个非活跃会话，并删除超过 30 天的会话；最近 24 小时有活动的会话不受数量
  上限清理；对应 share 快照随 transcript 删除。
  输入历史文件同样采用 30 天 TTL 与 100 个文件上限；本地导出的 HTML 与任务
  清单不会被自动删除。
- **上下文预算**：窗口与输出预算按模型取值（settings 声明、`model-catalog.json`
  或内置家族表；未知模型按 200k/64k 保守假设），有效输入窗口 = 窗口 − 输出预算；
  自动压缩阈值 = 有效窗口的 90%（当前 Claude 系约 785k），提前 20k 提醒（`/context`）。
  压缩 = 摘要旧消息 + 保留最近 12 条；压缩切点安全推进到 tool_result 边界之外，
  避免孤儿 tool_result 导致 400。连续压缩失败 3 次熔断（`/compact` 手动触发）。
  非 Anthropic 端点（无 count_tokens）自动改用本地估算（ASCII 约 4 字符/token、
  CJK 约 1 token/字）。
- **记忆**：memdir 自动记忆（`~/.config/bingo/memdir/<项目名>-<路径哈希>.md`，
  完整路径哈希避免同名项目串味）+ 项目 CLAUDE.md 与 AGENTS.md 作为 system 记忆。

## App-server（实验特性）

`bingo app-server` 用 JSON-RPC 2.0 把 bingo 的应用状态开在 stdio 上，
每行一个 JSON 对象（NDJSON）。它的存在理由只有一个：让 GUI 驱动终端所驱动的
同一个会话——同样的提交、回合、审批、agent、房间与动作，而不是重新实现一遍。

- **stdout 只有协议帧**，诊断一律走 stderr。
- **会话归服务端**。`session/start` / `session/resume` 开一个；
  `session/read` 与 `conversation/read` 给出权威快照，快照之后的一切以有序
  通知到达，序号无洞。
- **只有一条提交路径**。`conversation/submit` 决定输入是起一个回合、排队、
  在工具屏障处 steer，还是投递给别人——客户端不选。不常用的操作走
  `action/execute`，用的是 `/help` 打印的同一张表。
- **审批是服务端发起的交互**。权限请求与 `AskUserQuestion` 的寿命长于宣告它
  的那次调用，用 `interaction/respond` 回答，因此客户端重连后仍可作答。

契约由 Rust 类型生成，不手写：

```bash
bingo app-server generate-schema --out schema/app-server
```

已提交的 bundle 在 [`schema/app-server`](schema/app-server)：一份 manifest 把
每个方法映射到方向、params、result 与声明的错误——通知映射到方向与 params——
外加各自的 Draft-7 schema。客户端应从它生成 TypeScript，而不是手抄第二份类型。

**状态：实验特性。** manifest 里协议记作 1.0，wire 形状有逐变体的往返 fixture
与黑盒场景覆盖，但还没有已发布的消费者，因此不承诺兼容性。设计与其修订见
[`notes/design/gui-app-server.md`](notes/design/gui-app-server.md)。

1.0 不做：两个客户端同时控制一个会话、持久事件日志、网络传输、透出 provider
原生流帧、终端布局状态。详见设计文档的 non-goals。

## 架构

```text
TUI (ratatui)        bingo app-server        --print
      \                    |                  /
       +--------------- AppCore ---------------+
       | 独占一根线程的单会话 actor：
       | conversations · turns · items · 输入队列
       | interactions · attention · agents · rooms · tasks
       | 动作表 · catalogs · 服务端铸造的 id
       +-------------------+-------------------+
                           |
                    engine：query loop、工具、agent run
                    EngineEvent → actor（唯一定序点）
```

一切变更都发生在 actor 内部，由它盖上无洞的序号并发布；前端是这条流的投影，
自己不持有规则。终端只保留终端才有的东西——行、折叠、滚动、按键、图片的
单元格几何——而一张受检的账（`src/app/parity.rs`）逐条声明每个 slash 命令、
动作、通知、提交分支与终端事件属于哪一边。

actor 之下：

```text
settings 三层合并 (user/project/local)
Messages API 客户端 (reqwest + SSE 流式)
query loop: 工具调用 → 权限门 → 并发执行 → 结果回填
  ├─ Tool Registry (trait + schemars schema)
  ├─ MCP 适配层 (rmcp: stdio / streamable HTTP)
  ├─ 子代理 (异步 + 通知；D137 起同侪直接互信)
  ├─ Hooks (shell, JSON 契约)
  ├─ Task 存储 / 频道 / 技能 / 记忆 / transcript
  └─ 预算监控与压缩
```

核心循环语义：**模型只产出 tool_use 意图；权限、并行、副作用、压缩、记忆与
UI 由本地 harness 负责**。设计决策见 [`notes/research.md`](notes/research.md)。

## 项目结构

```text
src/
  main.rs          CLI 入口（clap）、会话启动链
  app/             应用内核：会话 actor 及其所有物
                   （command / event / snapshot / projection / queue /
                   interaction / attention / 动作表 / parity 账）
  app_server/      `bingo app-server`：wire 类型、stdio 传输、schema bundle
  engine/          内核把工作交出去的那一侧：run loop、动作 handler、
                   EngineEvent（工作重新进入 actor 的唯一入口）
  print.rs         `--print`：内核的第三个客户端
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
  tui/             ratatui 界面（chat / view / input / markdown / highlight / gfx …）
                   其中 `store.rs` 是它对内核的客户端投影
  ui.rs            与渲染器无关的事件与对话框契约
  system.rs        system prompt 拼装（基座 + 记忆 + 三前端共享的主会话
                   附加块：crew note / 房间礼仪 / 经验索引）
tests/
  fixtures/        集成测试夹具
schema/
  app-server/      已提交的 app-server 协议 JSON Schema bundle
notes/
  research.md      技术决策记录
  design/          协议与界面设计
```

## 开发约定

- Rust 2024 edition；错误处理用 thiserror，生产代码不 unwrap/expect。
- 代码写成周围代码的样子；无注释优先，注释只解释"为什么"。
- 不加不需要的依赖；造轮子前先看 crates.io。
- 改动涉及用户可见行为（配置项 / slash 命令 / 工具 / 错误信息 / 能力地图）时，
  同步更新内置技能 `src/skills/bundled/guide.md`（AGENTS.md 的同步规则）。
- 改动架构前先对表 `notes/research.md` 的决策记录。
- 每次改动跑 `cargo build` 与 `cargo clippy -- -D warnings`，相关逻辑必带测试。
