# slash-ux docs patch (devex 提供 · dev 提交 5 取用)

> 状态：内容就绪，等 dev 提交 1-4 落地后并入提交 5。所有表述已对照代码事实核实
> （chat.rs THINK_LEVELS / toggle_thinking、api/types.rs thinking_param/effort_param、
> settings.rs 三层合并），与 main 裁决的 G2 口径一致。
> 设计契约已定稿 v0.4：TTL 分级规格见契约 §4.4（成功 2s / 错误与用法 ≥8s，首选常驻至下次输入、
> 8s 为下限）、错误码格式见 §4.5（`[error] code=… msg=…` 单行，qa 只断言 code）。
> 落笔前请按实际落地的行为再核对一遍（尤其 G3/G4 是否如约随提交 4 落地）。

## 1. src/skills/bundled/guide.md

### 1a. Config 表 `thinkingLevel` 行（G2 核心：6 档 + 缺省口径 + effort 语义）

现状（过时）：
```
| `thinkingLevel` | string | Thinking level: `off` sends no thinking param (DeepSeek-compatible, default); `low`/`medium`/`high` always send `{"type":"adaptive"}` adaptive thinking (the Claude 5 family removed budget_tokens; the level doesn't affect depth for now) |
```

替换为：
```
| `thinkingLevel` | string | Thinking level: `off` sends no thinking param (DeepSeek-compatible, default); `low`/`medium`/`high`/`xhigh`/`max` send `{"type":"adaptive"}` adaptive thinking plus `output_config.effort` (the Claude 5 family removed budget_tokens; below `high` saves tokens, `xhigh`/`max` think deeper) |
```

### 1b. 快捷键行：Alt+T 语义 + 忙时白名单

现状：
```
· Shift+Tab cycles permission modes (default → acceptEdits → plan) · Alt+T thinking toggle · while busy, Enter queues the message, sent automatically at turn end.
```

替换为：
```
· Shift+Tab cycles permission modes (default → acceptEdits → plan) · Alt+T thinking toggle (off ↔ the last non-off level, default medium) · while busy, Enter queues the message (sent automatically at turn end; /think /model /provider /theme /status /context /tasks /help /skills run immediately) ·
```

### 1c. Slash 快速参考：/think 行（6 档 + 选择器一句话）

现状：
```
`/think [off|low|medium|high]`（思考级别，持久化 settings）、`/theme`、
```

替换为：
```
`/think [off|low|medium|high|xhigh|max]`（思考级别，持久化 settings；无参打开档位选择器：●=当前生效、↑↓/1-6 浏览、Enter 确认、Esc 取消）、`/theme`、
```

## 2. notes/design/feedback-states.md — changelog 回填（追加到文件末尾）

```
- v1.21（2026-08-07）：slash command 交互对齐落地（Team A · feat/slash-ux）——
  忙时白名单即时命令（think/model/provider/theme/status/context/tasks/help/skills 忙时立即执行且 busy 不变；
  其余 slash 命令入队，TurnEnd 后按命令分派，不再作为纯文本发模型）；
  `/think` 档位选择器双标记（●=当前生效固定、❯=浏览选中）+ 1-6 直达 + footer `think {level} ▸` 预览态
  （Enter 落地 / Esc 还原）；slash 补全行 arg_hint 参数提示；
  无匹配提示行（`/zzz` → dim 行，属 chrome 提示非 error 级，不走错误码）；
  slash 错误结构化（UNKNOWN_COMMAND / BAD_ARGUMENT，`[error] code=… msg=…` 单行，qa 只断言 code）；
  slash 输出 TTL 分级（成功 2s / 错误与用法 ≥8s，首选常驻至下次输入——规格见设计契约 §4.4）——按提交 4 实际落地核对；
  defer 记案：子命令二级补全、/model 的 s session-only、模型/思考持久化层（Q1 待议）。
```

## 3. 复核清单（dev 提交 5 前逐条勾）

- [ ] THINK_LEVELS high 描述已删「（默认档位）」（chat.rs，随提交 2）
- [ ] guide.md 1a/1b/1c 三处已替换，无残留 4 档表述（`grep -n "low|medium|high"` 应无 `/think` 相关命中）
- [ ] feedback-states changelog 已回填 v1.21，且 G3/G4 条目与实际行为一致（如未落地则改为「已知 gap」注记）
- [ ] 忙时白名单清单与代码实际一致（grep whitelist 常量）
