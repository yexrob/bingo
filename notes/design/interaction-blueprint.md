# Interaction Blueprint — implementation program (D76–D92)

> Source: the 2026-08-14 interaction audit ("bingo TUI 审判书", 30 findings vs Claude Code 2.1.88
> leaked source + Codex CLI) and the approved interaction blueprint (unified conversation model,
> CC design language). This document is the authoritative spec for the implementation program.
> Design + review: session lead. Implementation: delegated agents, one decision record per batch.
>
> Global rules for every batch (in addition to AGENTS.md):
> - Branch: `feat/blueprint` (this worktree). One batch = one commit (or a few coherent commits).
> - Append your decision record to `notes/research.md` as `### D7x. <title>` (English, keep the
>   existing style: problem → decision → consequences).
> - If the batch touches user-visible feedback states, update `notes/design/feedback-states.md`
>   and its changelog; if it touches config keys / slash commands / capabilities, sync
>   `src/skills/bundled/guide.md`.
> - Design language is Claude Code's: chrome copy in English, CC wording verbatim where a CC
>   equivalent exists ("[Request interrupted by user]", "Press up to edit queued messages", …).
> - Never `git add -A`; stage files explicitly. Commit trailers (both lines):
>   `Co-authored-by: bingo <id+bingo@users.noreply.github.com>`
>   `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`
> - Gates before commit: `cargo fmt --all -- --check` · `cargo check --locked --all-targets` ·
>   `cargo clippy --locked --all-targets -- -D warnings` · `cargo test --locked --all-targets`.
> - Do not "fix" pre-existing unrelated CI debt (e.g. chat.rs line-count) inside a batch.

Program phases (order matters — later batches build on earlier ones):

| Phase | Batches | Theme |
|---|---|---|
| 1 | D76–D79 | Debt repayment: correctness of interrupt/terminal/visibility/attention |
| 2 | D80–D83 | Semantic layer: Esc stack, approval, transcript view, queue-next |
| 3 | D84–D87 | Capability: live tail, completion, editor/readline, motion tokens |
| 4 | D88–D90 | Unified conversation model (buffers, host merge, routing/#team) |
| 5 | D91–D92 | Rewind + snapshots; theme/highlighting |

---

## Phase 1 (this dispatch)

### D76 — Interrupt accounting and an honest Interrupted state

**Problem.** A stream-time interrupt discards the whole turn (`src/query.rs` aborted path skips
`record()`): the user keeps seeing the partial reply on screen while the model never learns it
said anything — an institutionalized split-brain. The only user signal is a 10s transient warning
(`chat_tail.rs` `finish_turn`), and interrupted tool rows render with the Done green glyph
(`chat.rs` maps `ToolCallStatus::Interrupted → ToolStatus::Done`).

**Design.**
1. In the abort path of `run_query`/`query_turn`: if the aborted turn accumulated any assistant
   content (text and/or thinking), record the partial assistant message to the transcript and
   in-memory history, then record a user-role message with the exact CC marker string
   `[Request interrupted by user]`. For interrupts that landed during tool execution (assistant
   already recorded, orphan tool_results already filled), append the CC variant
   `[Request interrupted by user for tool use]` as the user-role marker instead. Model-facing
   strings must be exactly these.
2. TUI rendering: a user message equal to either marker renders as a dim/error single line
   (no user bubble): `[Request interrupted by user]` in `theme.error`, not a `❯` block.
   Reuse the existing user-message render path with a special-case, not a new message kind,
   unless a `MessageKind` already fits.
3. Replace the transient "turn interrupted" warning with this persistent transcript marker
   (delete the warning emit; the marker is the record).
4. Add a real `ToolStatus::Interrupted` arm in the TUI: glyph `⏺` in `theme.warning`, result
   line text `Interrupted` (English). `activities.rs::dot_style` + the `chat.rs` status mapping.
5. `interrupted` auto-continue suppression stays as-is.

**Acceptance.**
- Unit: interrupt mid-stream with N chars accumulated → transcript contains partial assistant
  message + `[Request interrupted by user]` user message; next turn's request history contains both.
- Unit: interrupt during tools → orphan results filled AND `…for tool use` marker recorded.
- Unit: interrupt with zero accumulated content → no empty assistant message recorded, marker only.
- TUI test: interrupted tool row renders warning color and `Interrupted` text (not Done/green).
- The 10s "turn interrupted" warning no longer exists; no double-signal.
- Gates green; `feedback-states.md` updated (interrupt state).

### D77 — Terminal safety net + compaction warnings that reach the user

**Problem.** (a) No panic hook: a panic outside the turn task leaves the terminal in raw mode /
alt screen. (b) The pre-compaction warning goes to `eprintln!` (`compact.rs`) which the alt
screen swallows; (c) footer context bands (70/90% of raw window) don't match the real auto-compact
trigger (90% of effective window, `budget.rs`).

**Design.**
1. `std::panic::set_hook` installed at TUI setup (`src/tui/mod.rs`): best-effort emit
   DisableBracketedPaste/DisableMouseCapture/LeaveAlternateScreen/disable_raw_mode/Show cursor
   (ignore errors), gated by an `AtomicBool` "tui active" flag set on setup and cleared on clean
   teardown; then delegate to the previous hook. No allocation-heavy work in the hook.
2. `compact.rs` pre-threshold warning + progress prints: when a UI notify callback exists, route
   through it (existing `notify` → `UiEvent::Warning`); keep `eprintln!` only for headless/quiet
   paths. English copy: `context at {tokens} tokens; auto-compact at {threshold}`.
3. `context_usage.rs` bands derive from the auto-compact trigger point (as a percentage of the
   same denominator the footer shows): Warning ≥ trigger − 20pt, Danger ≥ trigger − 5pt.
   Keep the label format unchanged.

**Acceptance.** Hook installs once, restores idempotently (unit-test flag + hook fn directly);
warning surfaces as `UiEvent::Warning` in a capture test; band boundary tests updated to the new
thresholds; no behavior change for `--print`. Gates green; feedback-states.md changelog entry.

### D78 — Grouped tool output is retained (black-hole fix)

**Problem.** `chat.rs` `ToolDone`: `hint.set_content(...)` only runs in the `!in_group` branch, so
every grouped Read/Grep/Glob result is dropped forever; `Activity::expandable()` is content-based,
so expanding a group shows only summary rows.

**Design.** In the in-group branch, also store bounded content on the member activity (same
materialization + truncation policy as the ungrouped path). Group expansion (ctrl+o / click)
then reveals per-call content. Do not change collapse-summary rows. Memory bound: reuse the
existing per-result char budget; if none applies at this layer, cap at the ungrouped path's
effective budget — no new config keys.

**Acceptance.** Regression test: grouped Read×3 → expand group → each member has non-empty content
lines rendered; collapsed view unchanged; a large grouped result is truncated by the same rule as
ungrouped. Gates green.

### D79 — Attention channel: bell, OSC notifications, terminal title

**Problem.** Zero notification surface: no bell, no OSC 9/99/777, no terminal title. The user who
switches away never learns the turn finished or a permission dialog is waiting.

**Design.** New module `src/tui/notify.rs`:
1. `NotifyChannel { Auto, Bell, Iterm2, Kitty, Ghostty, Disabled }`, settings key
   `notifications` (default `auto`), wired into `settings::KNOWN_KEYS`, three-layer merge, and
   `/config` interpretation. `Auto` detection: `TERM_PROGRAM=iTerm.app → Iterm2`,
   `kitty` (TERM_PROGRAM or TERM=xterm-kitty) `→ Kitty`, `TERM_PROGRAM=ghostty → Ghostty`,
   else `Bell`.
2. Sequences (write via the term layer's writer between frames — term.rs stays the single
   escape-sequence owner; add a narrow API there, e.g. `emit_osc(&str)`):
   - Bell: raw `\x07` (unwrapped, so tmux bell-action fires).
   - iTerm2: `OSC 9 ; {body} BEL`.
   - Kitty: `OSC 99 ; i={id}:d=0:p=title ; {title} BEL` + body part + done part (3-part, CC-style).
   - Ghostty: `OSC 777 ; notify ; {title} ; {body} BEL`.
   - When inside tmux, wrap OSC (not bell) in the passthrough envelope (reuse the pattern from
     `gfx.rs`).
3. Terminal title (all channels except Disabled): `OSC 2 ; {title} BEL`.
   States: busy → `✳ bingo — working…`, waiting approval → `✳ bingo — waiting for permission`,
   idle → `bingo — {cwd_short}`; restore the plain title on exit (best-effort; also in the
   panic hook from D77 if trivially reachable — else skip, note in the D-record).
4. Trigger points: (a) permission ask surfaced (`drain_asks` accepts a request); (b) `TurnEnd`
   for turns whose wall time ≥ 10s; (c) turn error (Full-level). Notify = title update + channel
   notification with a one-line body (English, e.g. `Turn complete · 214 tests passed` is out of
   scope — use `Turn complete`, `Waiting for permission`, `Turn failed`).
5. No focus tracking in this batch (notify unconditionally; CC parity enough).

**Acceptance.** Unit tests: sequence bytes per channel (goldens), tmux wrapping, auto-detection
matrix, Disabled emits nothing; integration: ask-pending and long TurnEnd invoke the notifier
(mock writer). Settings docs + `guide.md` config table updated. Gates green;
feedback-states.md gains the attention-channel section.

---

## Phase 2 — semantic layer (specs to be detailed at dispatch)

- **D80 Esc context stack**: rewrite `on_key_at` dispatch as an explicit ordered stack
  (dialog › menus › completion › info › interrupt › esc-esc); `pending_ask` cancelled on
  interrupt/TurnEnd (no orphaned oneshot); plain `↑` always history (agent selector moves out);
  digit/enter guards preserved. Mostly `chat_tail.rs`.
- **D81 Approval, CC three-option shape**: `PermissionRequest { options, scope, explanation }`;
  options exactly `Yes` / `Yes, allow all edits during this session (shift+tab)` (scope text
  generated by the permission engine: directory / command-prefix / domain variants) /
  `No, and tell bingo what to do differently (esc)` with inline feedback input (empty submit =
  plain deny); pre-approval diff for Edit/Write (dry-run unified_diff); `ctrl+e` explanation;
  `shift+tab` confirm:cycleMode; 0.4s enter-guard; deny feedback embedded in the
  `<permission_error>` body. `ui.rs`, `permission.rs`, `query.rs`, dialog rendering.
- **D82 Transcript view**: `ctrl+o` toggles an alt-screen pager over the full session (all tool
  outputs, incl. D78-retained group content), `ctrl+e` show-all (thinking raw), `/` search,
  `j/k`/PgUp/PgDn, `q`/`ctrl+o` exit; reuse the `entity.rs` alt-screen pattern; works from both
  hosts. This is the write-once compensation — no in-place mutation of scrollback.
- **D83 queue-next steering**: queued user messages drain at the next tool barrier into the
  current turn (CC `next` priority; plumbing exists via the InboxWake pattern in `query_turn.rs`);
  transcript `↪` marker; composer placeholder `Press up to edit queued messages`; `↑` pull-back
  kept; instant commands unchanged.

## Phase 3 — capability

- **D84 Bash live tail + ctrl+b backgrounding**: stream foreground bash stdout/stderr into a
  bounded 3–5 line tail on the `⎿` row + line counter; `ctrl+b` while a foreground tool runs
  moves it to the background task registry (model notified on completion); otherwise ctrl+b keeps
  its current meaning until D90 absorbs it.
- **D85 Completion**: `@` unified mention popup (files via ignore-aware walk + agents), fuzzy
  subsequence, Tab longest-common-prefix then accept, dirs append `/`; slash argument-level
  completion per command (`/model <name>`, `/mcp enable <srv>`, `/provider login <name>`, aliases
  registered).
- **D86 Editor & readline**: `ctrl+g` and `ctrl+x ctrl+e` → `$VISUAL`/`$EDITOR` on the draft
  ("Save and close editor to continue…" placeholder while active); push kitty keyboard
  enhancement flags at setup (Shift+Enter reliable) + paste-burst demotion (burst-Enter asks
  instead of silently newline); `ctrl+p/n` history; ctrl/alt+arrow word motion; `alt+d`,
  `alt+backspace`; kill-ring (10) + `alt+y` yank-pop.
- **D87 Motion tokens**: `src/tui/motion.rs` — `pulse` (120ms/frame), `beam` (6-cell sweep,
  1 cell/tick over the status verb), `stall` (3s → warning color, 6s → error + `(stalled?)`),
  `settle` (dim → accent 120ms → ok/error), `ease` (token counter, delta×0.15/frame, shown after
  30s), `breath` (existing banner, unchanged), all behind the single `motion` gate (fixes
  motion:"off" half-coverage); CC spinner verb list + `✻ {Churned} for {N}s` completion line;
  `✳` title prefix animation is optional stretch.

## Phase 4 — unified conversation model

- **D88 Buffer engine**: conversation = buffer (hub, DM, channel, #team board); per-buffer
  transcript with its own write-once cursor; switching = clear + rehydrate tail (reuse the
  resume/redraw machinery); one composer/key surface/approval/motion set for all buffers.
- **D89 Host merge**: the ctrl+g workspace modal retires; `entity.rs`/`slack.rs` palette and
  crippled composer die; day dividers / presence / sender grouping / avatars become transcript
  decorations of DM/channel buffers; single demand-gated tick loop.
- **D90 Conversation bar + routing + #team**: persistent bar above the composer
  (`1 ●hub 2 ●@scout 4 #build +2 …`), `ctrl+k` switcher (roster + task + unread, `x` stop agent —
  absorbs the ctrl+b manager and the ↑ entity selector), `alt+↑/↓`/`alt+1..9`, line-leading
  `@agent` / `#channel` routing with delivery receipts, `[DM from user]` marker preserved (D64),
  `esc` in a non-hub buffer returns to hub (navigation before interrupt), auto-created `#team`
  board fed by lifecycle events (spawn/done/assign/ack), `/team` output posts there,
  DM tinting via CC teammate vocabulary (composer border + `❯` in agent color,
  `(esc to interrupt <name>)` in the status row).

## Phase 5

- **D91 Rewind + snapshots**: double-Esc on empty input → CC two-stage Rewind (message list with
  per-step file-change stats → Restore code and conversation / Restore conversation / Restore
  code / Summarize from here / Never mind); Edit/Write snapshot files before mutation (bounded
  store, git-independent); transcript fork on restore.
- **D92 Theme & highlighting**: dark palette fully RGB (no named ANSI in the markdown/diff/code
  layer), the three-tier dim hierarchy actually used (subtle gets an accessor + jobs), syntax
  highlighting for fenced code (evaluate `synoptic`/`syntect` — smallest dependency that fits),
  diff line-number gutter + rendered expand hint (dead `diff_edit` token either used or deleted).
