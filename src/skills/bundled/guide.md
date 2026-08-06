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
- 快捷键：Enter 发送 · Esc 空闲切输入 / busy 中断 · Ctrl+C 退出 ·
  ctrl+o 展开/折叠工具输出 · ↑↓/Tab 补全 slash 命令 · j/k/G/g/PageUp/Down 滚动。

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
| `thinkingLevel` | string | 思考级别：`off` / `low`(2048) / `medium`(8192) / `high`(16384) budget tokens；缺省不发 thinking 参数（兼容 DeepSeek） |
| `permissionMode` | string | `default` / `acceptEdits` / `plan` / `dontAsk` / `bypassPermissions` |
| `theme` | string | `auto`（跟随终端背景）/ `dark` / `light` |
| `cacheControl` | bool | 发送 prompt caching；非官方端点不稳定时关闭 |
| `respondToBashCommands` | bool | `!` 命令执行后是否交模型回应（默认 true；false = 纯执行） |
| `mcpServers` | object | `{name: {type?, command, args, env}}`（stdio，缺省）或 `{name: {type: "http", url, headers?}}`（streamable HTTP） |
| `disabledMcpServers` | string[] | 禁用的 MCP 服务器名单（/mcp disable 写入） |
| `permissions` | object | `{allow[], deny[], ask[]}`，规则语法 `Tool(content)`，如 `Bash(git push:*)` |
| `hooks` | object | PreToolUse/PostToolUse/PreCompact/PostCompact/UserPromptSubmit/Stop/SessionStart/SessionEnd/TaskCreated/TaskCompleted，matcher + command |

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
   接近上下文窗口时 `/compact`（自动压缩阈值默认 ~窗口 90%）。
3. **MCP 服务器不工作**：`/mcp` 查看状态——`✗ failed: <详情>` 按详情修
   （命令不存在/spawn 失败/握手失败；http 服务器另查 url 可达性与 headers 鉴权）；
   修好后 `/mcp reconnect <name>`。`type: sse/ws` 会报"不支持（stdio / http）"。
   禁用/启用：`/mcp disable|enable [name|all]`
   （禁用名单持久化到 settings.json）。MCP 工具名为 `mcp__<server>__<tool>`，
   权限规则请用全名。
4. **权限弹窗/拒绝不符合预期**：`/permissions` 列出当前规则；规则语法
   `Tool(内容)`，Bash 命令支持通配（如 `Bash(git push:*)`）；改 `permissions.allow/deny/ask`
   或切换 `permissionMode`（bypassPermissions 全放行、plan 只读）。
5. **`!` 命令被拒**：交互式/TTY 命令（top/vim/ssh/sudo -i/fzf 等）设计上拒绝，
   用非交互等价物（`top -b -n 1`、`ssh host 'cmd'`）。
6. **bash 模式退不出来/误触**：空输入退格退出 bash 模式；`!` 前缀非空输入时是普通字符。
7. **找不到历史会话**：transcript 存 `~/.local/share/bingo/transcripts`（`--continue`
   续上次，`/resume` 列出/切换）。
8. **工具输出被折叠**：ctrl+o 展开/折叠；长输出显示 `+N lines`。
9. **slash 下拉没出现想要的命令**：输入前缀过滤（如 `/m` 匹配 mcp/model/meye）；
   Esc 关闭菜单；技能在 `/skills` 清单，`/技能名` 执行。

## 能力地图（问"bingo 能做什么"时对照）

- **内置工具**：Bash（经权限门）、Read/Glob/Grep、Edit/Write、WebFetch/WebSearch、
  Agent（子代理）、Task 族（任务追踪）、AskUserQuestion、Skill（技能调用）。
- **技能**：内置 `guide`（本指南）+ `~/.config/bingo/skills/` 与 `.bingo/skills/`
  目录技能（同名磁盘技能覆盖内置）；模型经 SkillTool 调用，用户经 `/技能名` 执行。
- **图片**：模型回复中的 markdown 图片（`![alt](路径)`，支持相对路径/data/http(s)）
  在支持 kitty graphics 的终端（Ghostty/kitty/WezTerm 等）内联渲染，其余终端显示
  `#[image]` 占位；图片随消息自动加载，不需要额外命令。
- **MCP**：stdio 与 streamable HTTP（`type: "http"`，可带自定义 headers）服务器工具接入（见上）。
- **记忆**：memdir 自动记忆（`~/.config/bingo/memdir/`）+ 项目 CLAUDE.md（Anthropic 惯例）。
- **会话**：transcript 持久化（JSONL），`--continue`/`/resume` 恢复，`/compact` 压缩。
