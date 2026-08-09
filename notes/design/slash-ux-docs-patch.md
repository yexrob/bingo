# slash-ux docs patch (provided by devex · picked up in dev commit 5)

> Status: content ready; merge into commit 5 after dev lands commits 1-4. Every statement
> has been verified against the code facts (chat.rs THINK_LEVELS / toggle_thinking,
> api/types.rs thinking_param/effort_param, settings.rs three-layer merge), consistent
> with main's G2 ruling.
> Design contract finalized v0.4: TTL grading spec in contract §4.4 (success 2s / error
> and usage ≥8s, preferred: stay until the next input, 8s is the floor), error-code
> format in §4.5 (`[error] code=… msg=…` single line, qa asserts on code only).
> Before writing, re-verify against the actual landed behavior (especially whether
> G3/G4 landed with commit 4 as promised).

## 1. src/skills/bundled/guide.md

### 1a. Config table `thinkingLevel` row (G2 core: 6 levels + default semantics + effort meaning)

Current (outdated):
```
| `thinkingLevel` | string | Thinking level: `off` sends no thinking param (DeepSeek-compatible, default); `low`/`medium`/`high` always send `{"type":"adaptive"}` adaptive thinking (the Claude 5 family removed budget_tokens; the level doesn't affect depth for now) |
```

Replace with:
```
| `thinkingLevel` | string | Thinking level: `off` sends no thinking param (DeepSeek-compatible, default); `low`/`medium`/`high`/`xhigh`/`max` send `{"type":"adaptive"}` adaptive thinking plus `output_config.effort` (the Claude 5 family removed budget_tokens; below `high` saves tokens, `xhigh`/`max` think deeper) |
```

### 1b. Shortcut row: Alt+T semantics + busy whitelist

Current:
```
· Shift+Tab cycles permission modes (default → acceptEdits → plan) · Alt+T thinking toggle · while busy, Enter queues the message, sent automatically at turn end.
```

Replace with:
```
· Shift+Tab cycles permission modes (default → acceptEdits → plan) · Alt+T thinking toggle (off ↔ the last non-off level, default medium) · while busy, Enter queues the message (sent automatically at turn end; /think /model /provider /theme /status /context /tasks /help /skills run immediately) ·
```

### 1c. Slash quick reference: /think row (6 levels + one picker line)

Current:
```
`/think [off|low|medium|high]` (thinking level, persists to settings), `/theme`,
```

Replace with:
```
`/think [off|low|medium|high|xhigh|max]` (thinking level, persists to settings; no arg opens the level picker: ●=in effect, ↑↓/1-6 to browse, Enter confirms, Esc cancels), `/theme`,
```

## 2. notes/design/feedback-states.md — changelog backfill (append to the end of the file)

```
- v1.21 (2026-08-07): slash command interaction alignment landed (Team A · feat/slash-ux) —
  busy-time instant whitelist (think/model/provider/theme/status/context/tasks/help/skills run
  immediately while busy, busy stays unchanged; other slash commands queue and dispatch per
  command after TurnEnd, no longer sent to the model as plain text);
  `/think` level picker dual markers (●=in effect, fixed; ❯=browsing selection) + 1-6 direct
  jump + footer `think {level} ▸` preview state (Enter commits / Esc reverts); slash
  completion rows gain the arg_hint parameter hint;
  no-match hint row (`/zzz` → dim row, chrome-level hint not error-level, no error code);
  structured slash errors (UNKNOWN_COMMAND / BAD_ARGUMENT, `[error] code=… msg=…` single
  line, qa asserts on code only);
  slash output TTL grading (success 2s / error and usage ≥8s, preferred: stay until the next
  input — spec in design contract §4.4) — verify against what commit 4 actually landed;
  defer records: subcommand secondary completion, /model session-only `s`, model/thinking
  persistence layer (pending Q1).
```

## 3. Checklist (tick each item before dev commit 5)

- [ ] THINK_LEVELS high description no longer carries "(default level)" (chat.rs, with commit 2)
- [ ] guide.md 1a/1b/1c all three replaced, no leftover 4-level wording (`grep -n "low|medium|high"` should have no /think-related hits)
- [ ] feedback-states changelog backfilled to v1.21, and the G3/G4 entries match actual behavior (if not landed, change to a "known gap" note)
- [ ] busy whitelist list matches the code (grep the whitelist constant)
