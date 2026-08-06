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
- **TUI**：ratatui 双模式（默认 inline 嵌入终端 scrollback，`--fullscreen` 备用屏
  canvas），kitty graphics 内联渲染图片，历史反向搜索，slash 命令菜单。
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
bingo                       # 交互式 TUI（默认 inline 模式）
bingo -p "修复这个 bug"       # headless：prompt 参数，结果打到 stdout
bingo -p < prompt.txt       # headless：从 stdin 读 prompt
bingo --continue            # 恢复最近一次会话
```

启动时缺 API key 会直接报错。

## 命令行参数

| 参数 | 说明 |
|---|---|
| `-p, --print` | headless 模式：直接把回复打到 stdout（prompt 取参数或 stdin） |
| `--fullscreen` | 全屏模式（备用屏 canvas，输入吸底、app 内滚动）；默认 inline（历史在终端 scrollback） |
| `--model <名>` | 使用指定模型（默认 `claude-sonnet-5`） |
| `--permission-mode <模式>` | 权限模式：`default`/`acceptEdits`/`plan`/`dontAsk`/`bypassPermissions`（默认取 settings） |
| `--continue` | 恢复最近的会话继续对话 |
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
| `Ctrl+G` | agent / 频道选择器（agent 视图看实例完整对话，频道视图微信式群聊房间） |
| `Ctrl+L` | 清屏重画 |
| `Shift+Tab` | 循环权限模式（default → acceptEdits → plan） |
| `Alt+T` | 思考开关 |
| busy 时回车 | 消息排队，回合结束自动发送 |

### Slash 命令（`/help` 全量清单）

`/model [名]`、`/provider [名称]`（列出/切换多 provider）、
`/think [off|low|medium|high]`、`/theme`、`/permissions [allow|deny|ask] [规则]`、
`/mcp`（状态）· `/mcp enable|disable [name|all]` · `/mcp reconnect <name>`、
`/skills`（清单，`/技能名` 直接执行）、`/context`（用量）、`/status`、
`/compact`（强制压缩）、`/resume [名称]`（恢复历史会话）、`/rename`、
`/clear`、`/exit`。

### 图片渲染

模型回复中的 markdown 图片（`![alt](路径)`，支持相对路径 / data: / http(s)）在
支持 kitty graphics 的终端（Ghostty/kitty/WezTerm 等）内联渲染，其余终端显示
`#[image]` 占位。tmux 内需外层终端支持且 `tmux set -g allow-passthrough on`。

## 配置（settings.json）

三层配置浅层合并，后者覆盖前者：

1. **user**：`~/.config/bingo/settings.json`（`XDG_CONFIG_HOME` 优先）
2. **project**：`.bingo/settings.json`（入库，注意别提交密钥）
3. **local**：`.bingo/local.json`（个人覆盖，不入库）

| 配置项 | 类型 | 说明 |
|---|---|---|
| `apiKey` | string | API key（settings 优先于 `ANTHROPIC_API_KEY`/`DEEPSEEK_API_KEY`） |
| `apiBaseUrl` | string | API 端点（settings 优先于 `ANTHROPIC_BASE_URL`；缺省官方） |
| `providers` | object | 命名 provider（Anthropic 协议）：`{名: {apiKey, apiBaseUrl}}`，`/provider <名>` 切换 |
| `thinkingLevel` | string | `off` 不发 thinking 参数（兼容 DeepSeek，缺省）；`low`/`medium`/`high` 发自适应 thinking |
| `permissionMode` | string | `default` / `acceptEdits` / `plan` / `dontAsk` / `bypassPermissions` |
| `theme` | string | `auto`（跟随终端背景）/ `dark` / `light` |
| `cacheControl` | bool | 发送 prompt caching（默认关：非官方端点不稳定） |
| `respondToBashCommands` | bool | `!` 命令执行后是否交模型回应（默认 true） |
| `mcpServers` | object | 见下「MCP」 |
| `disabledMcpServers` | string[] | 禁用的 MCP 服务器名单（`/mcp disable` 写入） |
| `permissions` | object | `{allow[], deny[], ask[]}`，规则语法见「权限系统」 |
| `experimental` | object | 实验特性：`agentChannels`、`channelMessageLimit`（默认 500）、`agentMessageLimit`（默认 50） |
| `hooks` | object | 各事件 hook，见「Hooks」 |

示例：

```json
{
  "apiKey": "sk-ant-xxxx",
  "apiBaseUrl": "https://api.anthropic.com",
  "providers": {
    "deepseek": { "apiKey": "sk-ds", "apiBaseUrl": "https://api.deepseek.com" },
    "local": { "apiKey": "sk-any", "apiBaseUrl": "http://127.0.0.1:11434/v1" }
  },
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
| `Bash` | 在独立进程组执行 shell 命令；超时/取消整组终止，不留孤儿进程；非交互命令 |
| `Read` / `Glob` / `Grep` | 只读检索；默认跳过 `.git`/`target`/`node_modules` 与隐藏目录 |
| `Edit` / `Write` | 文件编辑（产生 unified diff 供 UI 预览） |
| `WebFetch` / `WebSearch` | 网页抓取与搜索（共享 HTTP 连接池；预批准域名自动放行） |
| `Agent` | 派生命名子代理（异步执行，完成通知注入上下文；`background:false` 可同步等待） |
| `SendMessage` / `AgentControl` | 子代理续话与生命周期管理（仅主会话装配） |
| `TaskCreate`/`TaskUpdate`/`TaskGet`/`TaskList` | 任务追踪（磁盘存储，TUI 任务区同源，含生命周期 hook） |
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

## 频道（实验特性）

`settings.experimental.agentChannels: true` 开启后：

- 主 agent 获得 `Channel`/`Post` 工具：建频道、进出成员（成员限直接子代理，
  主 agent 名 `main` 自动入席）；成员用 `Post` 发言，消息进全体成员上下文（同序）。
- `serial` 频道落后发言被弹回并附新增消息（agent 阅读后自行改口，报数式顺序
  由此涌现）；`free` 频道允许交叉。
- 超限自动冻结频道并通知主 agent（`channelMessageLimit`/`agentMessageLimit` 预算闸）。
- 频道在 transcript 显示为 `◇ #名字` 行；Ctrl+G 可打开全屏群聊房间直接以
  user 身份发言。

## 技能（Skills）

- 加载顺序（优先级从高到低）：用户层 `~/.config/bingo/skills/` → 项目层
  `.bingo/skills/`（从 cwd 向上逐层，近者优先）→ 内置 `guide`（编译进二进制，
  仅作兜底）；同名磁盘技能覆盖内置。
- 每个技能一个目录：`<name>/SKILL.md`，YAML frontmatter
  （`description`/`when_to_use`/`arguments`）+ markdown 正文。
- 调用：模型经 `SkillTool` 自动调用；用户经 `/技能名 [参数]` 直接执行。
- 内置 `guide`：bingo 使用与诊断手册（回答"怎么配置/为什么/不工作"时对照）。

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
- hook 以 `/bin/zsh -c` 执行，stdin 喂事件 JSON（`hook_event_name`、
  `tool_name`、`tool_input`、`permission_mode` 等），stdout 回传 JSON。
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
（D1–D24）。

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
  research.md      技术决策记录（D1–D24）
```

## 开发约定

- Rust 2024 edition；错误处理用 thiserror，生产代码不 unwrap/expect。
- 代码写成周围代码的样子；无注释优先，注释只解释"为什么"。
- 不加不需要的依赖；造轮子前先看 crates.io。
- 改动涉及用户可见行为（配置项 / slash 命令 / 工具 / 错误信息 / 能力地图）时，
  同步更新内置技能 `src/skills/bundled/guide.md`（AGENTS.md 的同步规则）。
- 改动架构前先对表 `notes/research.md` 的决策记录。
- 每次改动跑 `cargo build` 与 `cargo clippy -- -D warnings`，相关逻辑必带测试。
