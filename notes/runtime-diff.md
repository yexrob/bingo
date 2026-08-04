# runtime diff：bingo vs Claude Code（leak 2.1.88）vs Codex

> 2026-08-04。Claude Code 语义来源 `~/Episodes/Resources/research/claude-code-re/leaked-src/`（文件内行号见子项）；
> Codex 来源 deepwiki(openai/codex) + GitHub 源文件核对；bingo 为当前工作树实测语义。
> 结论优先：**bingo 主循环骨架与 CC 一致（模型只产 tool_use，本地管权限/副作用），差距集中在停止语义、预算管理、权限规则、hooks 协议四块。**

## 1. 三方对照总表

| 维度 | Claude Code 2.1.88 | Codex | bingo（现状） |
|---|---|---|---|
| 停止信号 | **不用 stop_reason**，以流中实际 tool_use 块为唯一退出信号（query.ts:553-557）；终止原因全集 10 种 | `needs_follow_up`（有 tool call 或 compact 触发继续采样）；纯文本回复即回合结束 | 仅 `tool_uses.is_empty()`；**stop_reason 从不读取**（client.rs 记录但 query.rs 不用） |
| max_tokens 截断 | 两级恢复：① 同请求 8k→64k 升级（`ESCALATED_MAX_TOKENS=64_000`，每回合一次）；② 多轮 `MAX_OUTPUT_TOKENS_RECOVERY_LIMIT=3`，注入 "Output token limit hit. Resume directly" isMeta 消息 | 无专门恢复路径；ContextWindowExceeded → compact | **无**。截断即静默结束；我们直接固定 64k（≈CC 升级后的值，但缺恢复语义） |
| 中断 | AbortController `reason==='interrupt'`；工具 `interruptBehavior: 'cancel'\|'block'`（默认 block）；中断时所有 tool_use 生成 synthetic tool_result 保证消息合法 | Ctrl+C → turn 中止、不重发消息、返回礼貌 FunctionCallOutput | **无中断路径**（Esc 仅切输入模式） |
| 并发 | `partitionToolCalls`：连续 safe 一批（`CLAUDE_CODE_MAX_TOOL_USE_CONCURRENCY` 默认 10）；StreamingToolExecutor 动态判定（无执行中 || 本工具 safe && 全部执行中 safe）；**Bash 出错杀兄弟工具**；context modifier 不支持并发 | RwLock：并行工具 read lock 并发、串行工具 write lock 独占（全局互斥，比 CC 简单） | 连续 safe 前缀分批、上限 10（≈CC partitionToolCalls）；无兄弟错误杀、无 interruptBehavior |
| 输入预算 | `effectiveWindow = 200k − min(maxOut, 20k)`；compact 阈值 = effective − **13k**（≈77.5%）；warning/error buffer 各 **20k**；blocking = effective − 3k（仅 autoCompact 关时）；**连续失败 3 次熔断**；count_tokens 计入 compact summary 预留（p99.99=17,387） | compact 阈值 = 窗口 **90%**（clamp 封顶）；effective 95% 预留；`BASELINE_TOKENS=12_000`；bytes 近似计数（非精确 tokenizer） | `COMPACT_FRACTION=0.9`（180k/200k，≈Codex 语义）；`count_tokens` API 精确计数；**无 warning/error 分级、无熔断、无输出预算动态管理** |
| 工具结果大小 | `DEFAULT_MAX_RESULT_SIZE_CHARS=50_000` 超限持久化到 `<session>/tool-results/`，模型收 2_000 字节预览 + `<persisted-output>` | `log_preview()` 截断 | Read 单次 20k 字符硬截断（读不完再读，无 preview 协议）；Bash 输出无大小限制 |
| Hooks | **26+ 事件**；command/prompt/agent/http 四型；exit 0/2/其他三档语义；**超时**：工具 10min、prompt 30s、SessionEnd 1.5s；并行执行、permission 聚合 deny>ask>allow；`CLAUDE_ENV_FILE` 会话环境脚本；async hooks；Stop hook 防循环（`stopHookActive`） | 11 事件（`ClaudeHooksEngine`，同源）；command 型；`additional_context_limit` 缺省 2_500 tokens | 4 事件（Pre/PostToolUse、Pre/PostCompact）；仅 command 型；60s 超时；**失败一律视为 allow**；顺序执行；无 exit 2 阻塞语义 |
| 权限 | 模式 × **规则表**（`ToolName(ruleContent)` 语法，多来源合并）+ 工具自身 `checkPermissions` + `safetyCheck`（**bypass 免疫**）+ 内容 ask 规则（bypass 也尊重）+ speculative bash classifier 竞速 | **沙箱兜底**：read-only / workspace-write / danger-full-access × approval_policy（Never/OnRequest/UnlessTrusted/Granular）；未匹配命令按策略 Allow/Prompt/Forbidden | 模式 × 工具属性二元；**AcceptEdits 与 Default 行为相同**；无规则表；`is_destructive` 无任何工具覆写（仅装饰）；Plan 直接 deny 全部非只读 |
| 重试 | `DEFAULT_MAX_RETRIES=10`、退避 `min(500·2^n, 32k)+25% 抖动`、Retry-After 优先；**400 max_tokens 超限重算** `max(3000, C−A−1000)`；529 ×3 → fallback 模型；**流空闲 90s 看门狗** → 非流式重试；持久重试心跳 30s | 上限 `stream_max_retries()`（默认值未查到）；指数退避；重试尽 → fallback transport（WS→HTTPS）；ContextWindowExceeded/UsageLimitReached 不重试 | `MAX_RETRIES=2`（共 3 次尝试）、仅 5xx/429、退避 500ms→2s；**无 400 重算、无流看门狗**；摘要/记忆/count_tokens 零重试 |
| 会话 | claude.json 项目历史；`<session>/tool-results/` 目录 | rollout JSONL + SQLite 索引；**resume/fork 一等公民**（fork 用 history_base 引用不整段复制） | transcript JSONL + mtime 找 latest + `--continue`（语义≈两者）；无 fork |
| 记忆 | memdir + memory-tool 交互读写 + prefetch + extract-memories stop hook | notes/history 跨窗口保留（compact 时） | 自动提取 facts 到 memdir 文件；无交互读写、无 prefetch（够用但弱） |

## 2. 差距分级

### P0 — 语义缺失，现在就该修

1. **max_tokens 截断续写**（query.rs:280-283）。CC 有完整恢复链；DeepSeek 64k 下截断少见但会发生（实测过一次 32k 截断）。修法：`stop_reason=='max_tokens' && tool_uses.is_empty()` → 注入 "Output token limit hit. Resume directly, no apology." 用户消息重试，上限 3 次。
2. **中断/取消**。无任何 abort 路径：用户想停只能等回合自然结束。修法：TUI Esc/ctrl-c → watch channel → abort → 为已收集 tool_use 合成 `<tool_use_error>cancelled</tool_use_error>` 回填（保证消息合法），回合按 aborted 终止。工具层 `interruptBehavior: cancel|block` 留作工具属性（Bash 默认 block 继续跑完更安全）。
3. **400 max_tokens 重算**。上下文超限 400 时 CC 重算 `max(3000, window − input − 1000)` 重试而非放弃；我们目前直接 Protocol 错误终止。

### P1 — 预算保护与协议对齐

4. **输入预算分级**：warning（20k buffer）/compact（13k buffer）/熔断（连续失败 3 次）。当前 90% 一刀切，压缩失败无保护性降级（我们压缩失败用占位摘要继续——已有兜底，但无熔断，连续失败每轮都打 count_tokens+摘要）。
5. **工具结果大小**：Bash 输出加 50k 字符上限（CC 同值），超限持久化 + 2k 预览文本；Read 保持 20k 截断（等效可用）。防止单次大输出撑爆上下文。
6. **hook exit 2 语义**：exit 2 → stderr 注入模型为 blocking 消息（当前一律视为 allow）；`if` 条件规则语法。
7. **重试增强**：MAX_RETRIES 2→5、退避 cap 32s、`Retry-After` 已有；SSE 流中断尝试重连一次。

### P2 — 功能增量（对应对标清单第 6、7 项）

8. **工具面**：Glob/Grep/Edit/Write/WebFetch。没有写工具，AcceptEdits 模式无意义——**这解释了 acceptEdits 语义缺失**。
9. **Hooks 事件扩展**：UserPromptSubmit/Stop/Notification/SessionStart/SessionEnd（SessionEnd 超时 1.5s 是 CC 的务实默认）。
10. **权限规则表**：`ToolName(rule)` 语法 + `safetyCheck`（bypass 免疫）。Codex 走沙箱路线（另一方向，bingo 初期不引入——research.md D13 已定）。

## 3. 与 research.md 决策的一致性检查

- D7 主循环语义 ✓（停止信号不依赖 stop_reason——已实现）
- D12 输出侧 max_tokens 动态管理 —— **未实现**（P0-1）；输入侧阈值 —— 已实现但简化（P1-4）
- D2 权限门 —— 缺规则表（P2-10），fail-closed 默认 ✓
- D11 transcript —— ✓ 基本对齐
- 一个发现：**bingo 的 90% compact 阈值与 Codex 相同、与 CC（~77.5%）不同**——两者都是"合理"选择，保留现状即可，但要补 buffer 分级。

## 4. 建议动作顺序

1. ✅ P0-1 max_tokens 恢复 → P0-2 中断 → P0-3 400 重算（query.rs/client.rs，2026-08-04 完成）
2. ✅ P1-4 预算分级 + 熔断（budget.rs/compact.rs，完成）
3. ✅ P1-6 hook exit 2（hooks.rs，完成）
4. ✅ P1-5 工具输出上限（query.rs 统一裁剪，完成）
5. ✅ P2（2026-08-04 完成）：工具面 Glob/Grep/Edit/Write/WebFetch → acceptEdits 语义 → 权限规则表 + safetyCheck → hooks 事件扩展

> 注：Codex 未查到项（stream_max_retries 默认值、退避参数、模型窗口表）不阻塞——那些是模型商细节，bingo 用 CC 数值即可。

## 5. P0+P1 落地记录（2026-08-04）

- **P0-1**：`stop_reason == "max_tokens"` 且无 tool_use → 注入恢复消息重试，上限 3 次（对标 `MAX_OUTPUT_TOKENS_RECOVERY_LIMIT`）。注：CC 还有 8k→64k 升级路径，bingo 直用 64k 所以只有多轮恢复。
- **P0-2**：`watch::Sender<bool>` 中断通道，TUI busy 时 Ctrl+C/Esc 触发；流读取 `select!` 立即中止；已入队工具照常跑完回填（对标 `interruptBehavior: 'block'`，Bash 默认 block 更安全）；中断整轮丢弃（assistant 半截消息不进上下文）；UI 顶部显示 "回合已中断"。headless/子代理不传 cancel。
- **P0-3**：400 含 "exceed context limit" → 解析 "A + B > C" 重算 `max(3000, C−A−1000)`（floor 3000）重试一次。
- **P1-4**：`AUTOCOMPACT_THRESHOLD` 90% 窗口（=180k，Codex 语义）保留；新增 `WARNING_THRESHOLD` 160k（effective−20k）；连续压缩失败 3 次熔断（`MAX_COMPACT_FAILURES`，Session.compact_failures）。
- **P1-5**：工具结果回填统一裁剪 50k 字符（`MAX_RESULT_CHARS`）并标注。注：简化自 CC 的"落盘 + 2k 预览"，未持久化。
- **P1-6**：hook 退出码语义——exit 2 = blocking（PreToolUse 拒绝工具且 stderr 作为原因注入；PostToolUse 阻断继续）；其他非零 = 仅用户可见警告；0 = 继续。
- **P1-7 部分**：请求前重试 2→5 次、退避 cap 32s。**流中断重连未做**——流中断时 UI 事件已非幂等消费（delta 重放会重复文本），等价物（CC 的流空闲 90s 看门狗 + 非流式重试）留作后续。

### 验证（2026-08-04 实测）

- 工具执行中 C-c → sleep 60 照常跑完回填 ✓ → "回合已中断" ✓
- 流式输出中 C-c → 散文 5 秒处截断，回合立即结束 ✓
- 单元测试 45 个（新增：400 解析、裁剪、阈值层级、hook exit 2/PostToolUse block、上下文重算保底）

## 6. P2 落地记录（2026-08-04）

- **工具面**（对标 CC 工具池）：
  - `Glob`：globset 递归、上限 500 条、跳过符号链接目录
  - `Grep`：regex 搜索、单文件 2MB/二进制跳过、200 行上限
  - `Edit`：old_string 精确替换（replace_all 支持）、is_destructive + is_edit_tool
  - `Write`：覆盖写 + 自动建父目录
  - `WebFetch`：HTML→纯文本轻量转换、30s 超时、100k 截断（简化自 CC 的 readability 管线）
- **acceptEdits 语义**：`is_edit_tool` trait 标记（Edit/Write），acceptEdits 模式自动允许；Bash 等其他工具照常询问——**未实现**：CC 的 diff 预览（依赖编辑器侧，本地 CLI 无 IDE 集成）。
- **权限规则表**：settings `permissions.allow/deny/ask`，规则语法 `Tool(content)`（`Bash(git *)` 命令前缀、路径工具前缀匹配、`mcp__server` 前缀）；判定顺序对标 CC：deny → ask 规则（**bypass 也尊重**）→ 只读 → safetyCheck → bypass → acceptEdits → allow 规则 → 模式默认。
- **safetyCheck**：写工具目标含 `.git/.claude/.vscode/.idea` 段 → 必须 ask（bypass/acceptEdits 免疫）。未做 CC 的 shell 配置（.zshrc 等）与 Read 侧检查。
- **hooks 事件扩展**：UserPromptSubmit（exit 2 / decision:block 阻止提交）、Stop（exit 2 → stderr 注入模型重试一次，防循环）、SessionStart、SessionEnd（1.5s 快速超时，对标 `SESSION_END_HOOK_TIMEOUT_MS_DEFAULT`）。
- **未做**（留后续）：WebSearch（需外部服务）、diff 预览、CLAUDE_ENV_FILE 会话环境、plugin/权限 hook（prompt/http/agent 型 hook）。

### P2 验证（2026-08-04 实测）

- acceptEdits：`✓ Write` 无模态直接执行；Glob 列出 26 个 rust 文件 ✓
- 规则 deny：`Bash(rm -rf)` 在 bypassPermissions 下仍被拒绝，模型如实汇报 ✓
- 单元测试 51 个（新增：acceptEdits 分支、deny/ask/allow 规则、safetyCheck 优先级、UserPromptSubmit/Stop block）
