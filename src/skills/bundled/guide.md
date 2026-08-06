---
name: guide
description: >-
  bingo 使用指南与诊断手册：settings 配置、slash 命令、模式、MCP、故障排查。
  Use when the user asks how to use/configure bingo, or reports a problem
  ("为什么", "怎么配置", "怎么诊断", "不工作").
when_to_use: >-
  User asks how to configure or use bingo · reports a bug or unexpected
  behavior · asks about settings.json / slash commands / MCP / permissions.
---

# bingo 使用与诊断指南

回答用户问题时按本指南定位配置项、命令与排查路径；结论给具体文件路径、
命令与验证步骤，不臆测功能（能力以实际行为为准，拿不准时读源码确认）。

## 快速上手

- 启动需要 API key：`ANTHROPIC_API_KEY`（Anthropic）或 `DEEPSEEK_API_KEY`
  （DeepSeek）；自定义端点用 `ANTHROPIC_BASE_URL`。也支持写进 settings.json
  （`apiKey`/`apiBaseUrl`，见下，settings 优先于环境变量）。缺失时启动即报错。
- 输入 `!` 进入 bash 模式（直接执行命令不经模型，前缀 `!` 粘性保留）；
  输入 `!echo hello` 试试。交互式/全屏命令（top/vim/ssh/fzf/lazygit）会被拒绝，
  用批处理替代：`top -b -n 1`、`vim file` → 用 Edit 工具。
- 快捷键（空输入按 `?` 看全表）：Enter 发送 · `\`+Enter / Ctrl+J 换行（多行输入）·
  Esc busy 中断 / 关下拉与面板 / 双击清空输入 · Ctrl+C busy 中断 / 有文本清空 /
  空输入连按两次退出 · ↑↓ 历史回溯（多行输入内先移光标；busy 空输入 ↑ 取回排队消息）·
  Ctrl+R 历史反向搜索 · Ctrl+A/E 行首尾 · Alt+B/F 按词移动 · Ctrl+W/U/K 删词/删到
  行首/行尾 · Ctrl+Y 粘回删除 · Ctrl+S 暂存/恢复输入 · Ctrl+_ 撤销 · ctrl+o
  展开/闭合切换（展开 = 重放完整 transcript 供终端上滑翻看；再按闭合折回
  聚合态并清屏收拢）· Ctrl+T 显隐任务区 · Ctrl+L 清屏重画 · Shift+Tab 循环权限
  模式（default → acceptEdits → plan）· Alt+T 思考开关 · busy 时回车把消息排队，
  回合结束自动发送。
- 大段粘贴自动折叠为 `[Pasted text #N +M lines]` 占位，发送时展开真实内容
  （经终端 bracketed paste 事件精确识别；不支持该特性的终端退回按键突发
  启发式，极快连打可能误判，停顿即恢复）。

## 配置指南（settings.json）

三层配置浅层合并，后者覆盖前者：
1. **user**：`~/.config/bingo/settings.json`（`XDG_CONFIG_HOME` 优先）
2. **project**：`.bingo/settings.json`
3. **local**：`.bingo/local.json`（个人覆盖，不入库）

| 配置项 | 类型 | 说明 |
|---|---|---|
| `apiKey` | string | API key（settings 优先于 `ANTHROPIC_API_KEY`/`DEEPSEEK_API_KEY`）；建议放 user 层，项目层会入库 |
| `apiBaseUrl` | string | API 端点（settings 优先于 `ANTHROPIC_BASE_URL`；缺省官方） |
| `providers` | object | 命名 provider（Anthropic 协议）：`{名: {apiKey, apiBaseUrl}}`，`/provider <名>` 切换 |
| `thinkingLevel` | string | 思考级别：`off` 不发 thinking 参数（兼容 DeepSeek，缺省）；`low`/`medium`/`high` 一律发 `{"type":"adaptive"}` 自适应思考（Claude 5 家族已移除 budget_tokens，级别暂不影响深度） |
| `permissionMode` | string | `default` / `acceptEdits` / `plan` / `dontAsk` / `bypassPermissions` |
| `theme` | string | `auto`（跟随终端背景）/ `dark` / `light` |
| `cacheControl` | bool | 发送 prompt caching；非官方端点不稳定时关闭 |
| `respondToBashCommands` | bool | `!` 命令执行后是否交模型回应（默认 true；false = 纯执行） |
| `mcpServers` | object | `{name: {type?, command, args, env}}`（stdio，缺省）或 `{name: {type: "http", url, headers?}}`（streamable HTTP） |
| `disabledMcpServers` | string[] | 禁用的 MCP 服务器名单（/mcp disable 写入） |
| `permissions` | object | `{allow[], deny[], ask[]}`，规则语法 `Tool(content)`，`:*` 为前缀通配（如 `Bash(git push:*)`）；Bash 规则按子命令逐段匹配，路径规则匹配前归一化（详见诊断 4） |
| `hooks` | object | PreToolUse/PostToolUse/PreCompact/PostCompact/UserPromptSubmit/Stop/SessionStart/SessionEnd/TaskCreated/TaskCompleted，matcher + command；matcher 为整串锚定正则（`Edit\|Write`、`mcp__.*`），非法正则退回全等匹配 |

示例（.bingo/settings.json）：
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

## slash 命令速查

`/help` 全量清单。常用：`/model [名]`、`/provider [名称]`（列出/切换多 provider）、
`/think [off|low|medium|high]`（思考级别，持久化 settings）、`/theme`、
`/permissions [allow|deny|ask] [规则]`、
`/mcp`（状态）· `/mcp enable|disable [name|all]` · `/mcp reconnect <name>`、
`/skills`（清单，`/技能名` 直接执行）· `/context`（用量）· `/status` ·
`/compact`（强制压缩）· `/resume [名称]`（恢复历史会话）· `/rename` · `/clear` · `/exit`。

## 诊断指南（常见问题 → 排查路径）

1. **启动报错 missing API key**：设 `ANTHROPIC_API_KEY` 或 `DEEPSEEK_API_KEY`，
   或在 settings.json 写 `apiKey`（settings 优先）。
2. **模型请求失败/超时**：`/status` 看当前模型，`/model` 切换；多 provider 用
   `/provider <名称>` 切换（settings 的 providers 段）；`/context` 看用量，
   接近上下文窗口时 `/compact`（自动压缩阈值 = 有效窗口（200k − 64k 输出预算）
   的 90% ≈ 122k，约总窗口 61%）。非 Anthropic 端点（DeepSeek/ollama）无
   count_tokens 接口时自动改用本地估算（字符数/4），首次回退告警一次。
3. **MCP 服务器不工作**：`/mcp` 查看状态——`✗ failed: <详情>` 按详情修
   （命令不存在/spawn 失败/握手失败；http 服务器另查 url 可达性与 headers 鉴权）；
   stdio 服务器自身的报错输出在 `~/.local/share/bingo/logs/mcp-<名>.log`
   （每次连接重写，不会打进界面）；修好后 `/mcp reconnect <name>`。`type: sse/ws` 会报"不支持（stdio / http）"。
   禁用/启用：`/mcp disable|enable [name|all]`
   （禁用名单持久化到 settings.json）。MCP 工具名为 `mcp__<server>__<tool>`，
   权限规则请用全名。
4. **权限弹窗/拒绝不符合预期**：`/permissions` 列出当前规则；规则语法
   `Tool(内容)`，`:*` 为前缀通配（如 `Bash(git push:*)`）。Bash 规则按
   shell 操作符（`&&` `;` `|` 等）切成子命令逐段匹配：deny/ask 任一子命令
   命中即生效；allow 需**单条规则覆盖全部子命令**才免询问，含 `$()`/子 shell/
   未闭合引号的命令一律不自动放行（`Bash(git log)` 放行 `git log` 但不放行
   `git log | head`）。文件类规则匹配前做路径归一化（`~` 展开、相对路径按
   cwd 展开、消解 `..`），`Read(src/)` 也能匹配 cwd 下的绝对路径。MCP 工具
   不因服务器自报只读而免询问，需显式 allow（`mcp__server` 或
   `mcp__server__tool`）。改 `permissions.allow/deny/ask` 或切换
   `permissionMode`（bypassPermissions 全放行、plan 只读）。
5. **`!` 命令被拒**：交互式/TTY 命令（top/vim/ssh/sudo -i/fzf 等）设计上拒绝，
   用非交互等价物（`top -b -n 1`、`ssh host 'cmd'`）。
6. **bash 模式退不出来/误触**：空输入时 Esc/退格/Ctrl+U 均可退出 bash 模式；
   `!` 前缀非空输入时是普通字符；Tab 从本会话 `!` 历史前缀补全。
7. **找不到历史会话**：transcript 存 `~/.local/share/bingo/transcripts`（`--continue`
   续上次，`/resume` 列出/切换）。
8. **工具输出被折叠**：ctrl+o 展开全部折叠项并把完整 transcript 重放到
   终端（上滑滚动翻看；已打印的折叠旧拷贝留在更上方，属正常）；全展开
   态再按 ctrl+o 闭合——折回聚合态并清屏收拢；长输出显示 `+N lines`。
9. **slash 下拉没出现想要的命令**：输入前缀过滤（如 `/m` 匹配 mcp/model/meye）；
   Esc 关闭菜单；技能在 `/skills` 清单，`/技能名` 执行。
10. **Grep/Glob 搜不到东西**：默认跳过 `.git`/`target`/`node_modules` 与 `.`
   开头目录（把 `path` 显式指向它们时照常搜索）；pattern 对齐搜索根的相对
   路径（`src/**/*.rs` 生效），不含 `/` 的 pattern 按文件名匹配任意深度
   （`*.rs` 全树命中）；结果达上限即停止遍历。
11. **超时/中断后有进程残留**：Bash 命令在独立进程组运行，超时与取消整组
   终止（孙进程不再孤儿化）；Esc 中断回合后未完成工具会补占位结果，
   会话保持可恢复（不会因孤儿 tool_use 导致后续请求 400）。

## 能力地图（问"bingo 能做什么"时对照）

- **内置工具**：Bash（经权限门）、Read/Glob/Grep、Edit/Write、WebFetch/WebSearch、
  Agent（子代理）、Task 族（任务追踪）、AskUserQuestion、Skill（技能调用）。
- **技能**：内置 `guide`（本指南）+ `~/.config/bingo/skills/` 与 `.bingo/skills/`
  目录技能（同名磁盘技能覆盖内置）；模型经 SkillTool 调用，用户经 `/技能名` 执行。
- **图片**：模型回复中的 markdown 图片（`![alt](路径)`，支持相对路径/data/http(s)）
  在支持 kitty graphics 的终端（Ghostty/kitty/WezTerm 等）内联渲染，其余终端显示
  `#[image]` 占位。tmux 内：外层终端为 Ghostty/kitty 且 `tmux set -g
  allow-passthrough on` 时经 Unicode 占位符（U=1）渲染，图片随文本正常滚动；
  passthrough 未开会收到一次性提示；外层为 WezTerm/Konsole（不支持 U=1）或
  screen 走占位。图片随消息自动加载并在消息定稿落盘时渲染，不需要额外命令。
- **MCP**：stdio 与 streamable HTTP（`type: "http"`，可带自定义 headers）服务器工具接入（见上）。
- **记忆**：memdir 自动记忆（`~/.config/bingo/memdir/`，文件名
  `<项目名>-<路径哈希>.md`，同名目录不串味）+ 项目 CLAUDE.md（Anthropic 惯例）。
- **会话**：transcript 持久化（JSONL），`--continue`/`/resume` 恢复，`/compact` 压缩。
