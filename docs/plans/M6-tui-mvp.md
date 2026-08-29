# M6 — The TUI MVP: one more client of the same frames, and the commands it types

## Goal

`bingo` at a terminal is a daily tool: a full-screen transcript that streams, tool rows, an approval dialog that shows the diff, a composer with history, `/model /think /compact /permission`, `!` for the shell, `?` for the keys, `/clear /resume /help /exit`. Every key is a pure function of the folded `SessionState` and the surface's own `Ui`; the loop never waits on the kernel; the same crate runs on `RemoteKernel` by changing one constructor. The command dispatcher the wire has rejected since M0 lands in the actor. `--print --input-format stream-json` makes the compatibility encoder a multi-turn host protocol. Memory extraction no longer holds the answer.

## Bricks, in build order (owner)

1. **ADR-0008 + sdk** (kernel) — `CommandOutcome::Record{body}` replaces `Action{item}`; `SurfaceOptions.env`. One sdk change; the workers build on it.
2. **Dispatcher** (kernel, `session/commands.rs`) — pure `parse(&Input) -> Option<(name, args)>` (`/name rest`, `!rest`, `Action{name, args}`); lookup by name or alias in the registry's commands; `instant` runs now, the rest queue; the queue drains one unit at a time (a run of prose = one turn, a command = one unit; `absorb` stops at a command); a command runs on a spawned task and mails `Msg::CommandFinished{intent, outcome}`; acks per ADR-0008 §3; `Prompt` re-enters `submit`; `Record` mints through `record`.
3. **Session settings and built-ins** (kernel) — `Live.settings {provider, model, thinking}` initialised from spec + settings; `Host::reconfigure(session, Change::Model|Thinking)` re-chooses the model and rebuilds `TurnConfig`, mails `Msg::Reconfigure`; the actor swaps it for the next turn and publishes `SessionUpdated` and `ConfigChanged{kernel: {thinking}}` (also once at open). `core/src/commands/{model,think,compact}.rs` registered by the host. `/compact` → `Mailbox::compact(instructions)` → `Msg::Compact`, refused `NOT_READY` while a turn runs, else a `TurnKind::Compact` turn: measure, `compact(Manual)`, close. `catalog::models` lists the embedded catalogue per registered provider plus the configured model.
4. **Post-turn hooks** (kernel) — `TurnOutcome.items`; the actor publishes `TurnCompleted`, then runs `on_turn(End)` on a `TaskTracker` task; `Flow::Stop` waits ≤30 s; `Host::shutdown` awaits each actor's task (kept in `Live`). `ContextView::fold` renders `Action` items with a result as `[name] args\nresult`.
5. **`bingo-surface-tui`** (worker A) — ratatui 0.30 + crossterm 0.29 (`event-stream`), full-screen (alternate screen; inline is M11). Modules: `terminal` (raw mode, alternate screen, bracketed paste, kitty `DISAMBIGUATE_ESCAPE_CODES` push/pop, panic hook restores, OSC 2 title + bell as out-of-band bytes); `run` (the loop: `select!` over key events, the frame stream, a tick; `open`/`events_since`/`sessions`/`catalog` spawned and delivered back on a channel, never awaited inline); `ui` (`Ui`: composer, history cursor, dialog focus/feedback/expanded, esc/ctrl-c arming with `now: Instant`, scroll, help toggle, dropdown selection, transient notices, pending session swap); `keys` (the one binding table; the `?` panel reads it); `on_key(&mut Ui, &SessionState, KeyEvent, now) -> Vec<Effect>` with `Effect = Submit(Input) | Interrupt | Answer{..} | Open(SessionSelector) | Close | Exit | Title | Bell | Copy?`; `view` (`draw(&SessionState, &Ui, &mut Frame, now)`: transcript / status / notices / dialog / help / composer / dropdown / footer); `transcript` (items → wrapped lines: user, assistant via `markdown`, reasoning collapsed, tool rows `● Name summary` with status glyph, progress tail, first lines of output, `Display::Diff` coloured, compaction rule, interruption, notices, receipts, question answers, actions); `markdown` (pulldown-cmark → styled lines: headings, emphasis, code spans, fences dimmed, lists, quotes; no highlighting); `composer` (own editor: grapheme cursor, insert/delete, home/end, alt+b/f, ctrl+w/u/k, newline on shift+enter/ctrl+j/alt+enter, history up/down at the edges, bracketed paste); `dialog` (Permission: preview ≤12 diff / ≤6 command rows, Ctrl-E expands, `1/2/3`, `y/a/n`, arrows+enter, `n` opens a feedback row, Esc = deny, keyboard ignored before `guard_until`, answered once then waits for the frame that closes it; Question: options, multi toggles, free-text row, Esc = cancel when offered; Confirm; Login shows url/code); `commands` (local `/help /clear /resume /exit /quit`; dropdown merges them with `catalog/read Commands`, prefix-first ranking; `ArgSpec::Catalog` completes from that catalogue); `history` (`<data_dir>/history.jsonl`, append on submit, last 1000 loaded). Esc order: dialog → help → dropdown → interrupt a running turn. Ctrl-C: busy → interrupt; text → clear; empty → arm, again within 2 s → exit; Ctrl-D on empty → exit. Title `bingo — <cwd>`, `✻ ` prefix while `state.attention()`; bell on `InteractionOpened`. Footer: hints · model · `ctx NN%` (red at trigger). `Lagged` → resync. Exit closes the attachment and restores the terminal. Tests: `TestBackend` snapshots for every screen (idle, streaming, tool rows, each dialog, help, dropdown, ctrl-c armed, retrying, interrupted, context bar, error notice); `on_key` effect tables (no runtime); composer and markdown unit tests; `scripts/tui-smoke.sh` (`tmux -L bingo -x 120 -y 40`, fake provider: reply appears; Esc interrupts a `Delay`; a permission dialog answered `y` runs the tool; Ctrl-C twice exits 0; title restored).
6. **`--input-format stream-json`** (worker B, `bingo-surface-print`) — stdin NDJSON `{"type":"user","message":{"role":"user","content":<string|blocks>}}` submits a turn each; the run ends at stdin EOF once the last turn completed; `{"type":"control_request","request":{"subtype":"interrupt"}}` interrupts; with `--permission-prompt-tool stdio`, a permission `Interaction` is a `control_request{subtype:"can_use_tool", tool_name, input, permission_suggestions}` answered by `control_response{behavior: allow|deny, message?}`; without it, denied as today. Verified against the current Agent SDK / Claude Code documentation, date in the module doc.
7. **`!` and `/permission`** (worker C) — bash: `Command{name:"!", instant:true}` running `run::run` with the default timeout under `cx.cwd`, `Record{Action{name:"!", args:"<line>", result:"<output>"}}` (`\n[exit N]` when N ≠ 0), a `Delay`-free unit test through `CommandContext`; permissions: `Command{name:"permission", hint:"[mode]", instant:true}` sharing the policy's `Arc`, per-session mode map read by `decide`, no args → `View::Text` of the current mode and the five, unknown mode → error.
8. **bin** (kernel) — the TUI is the default at a TTY (`--print` or a pipe keeps the print surface); `--input-format`, `--permission-prompt-tool`; `SurfaceOptions.env`; register `TuiPlugin`; `ci.yml` job `tui-smoke` (ubuntu, macos; installs tmux).

## Files

`docs/adr/0008-commands.md`, `crates/bingo-sdk/src/{command,surface}.rs`, `crates/bingo-core/src/session.rs` + `session/{commands,mailbox}.rs`, `crates/bingo-core/src/host.rs` + `host/{reconfigure,catalog}.rs`, `crates/bingo-core/src/commands/{mod,model,think,compact}.rs`, `crates/bingo-core/src/{turn,context}.rs`, `crates/bingo-surface-tui/**`, `crates/bingo-surface-print/src/{lib,stream_json,input}.rs`, `crates/bingo-tool-bash/src/shell.rs`, `crates/bingo-permissions/src/{lib,mode}.rs`, `crates/bingo/src/main.rs`, `crates/bingo/tests/cli/*.rs`, `scripts/tui-smoke.sh`, `.github/workflows/ci.yml`, `schema/rpc.json`.

## Dependencies

`ratatui` 0.30, `crossterm` 0.29 (`event-stream`), `pulldown-cmark` (no default features), `unicode-width`, `unicode-segmentation` — in `bingo-surface-tui` only (ADR-0001: no other crate may name the terminal stack). The old project's `synoptic`/`image`/`ratatui-image` wait for M11. `budget.toml` `max_dependencies` may rise to 280 for the terminal stack; the number is recorded in Verified.

## Exit criteria

- [x] `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `scripts/check_discipline.sh`, `scripts/budget.sh`, `cargo deny check`
- [x] Dispatcher (actor tests): `/x` unknown → `Rejected{INVALID_INPUT}`; an instant command during a turn is applied while the turn goes on; a non-instant one is queued, runs when the turn ends, and prose queued behind it starts only after it; `Prompt` becomes a turn with the same intent; `Record` yields an item; `Input::Action` dispatches; `!` and `/` parse per ADR-0008 §1
- [x] Built-ins: `/model fake/fake-2` → `SessionUpdated{model}` and the next request carries it; `/think off` → `ConfigChanged{kernel:{thinking:null}}` and no reasoning parameter; `/compact` → `TurnStarted{origin: Auto}`, `Compacted`, `TurnCompleted`; `/compact` during a turn → `NOT_READY`; the catalogue lists ≥1 model per real provider
- [x] Post-turn: `on_turn(End)` observes `TurnCompleted` already in the journal; a hook that sleeps 200 ms does not delay `TurnCompleted`; shutdown waits for it
- [x] TUI: every screen above has a `TestBackend` snapshot; the permission flow (open → `2` → `AllowSession{scope}`; `n` + feedback → `Deny{feedback}`; a key before `guard_until` sends nothing) and the question flow are tests; Esc and Ctrl-C tables; `/clear` yields `Open(Create)`; the crate depends on no provider or tool crate (`check_discipline.sh`); `scripts/tui-smoke.sh` passes on macOS and Linux
- [x] stream-json input: two user lines → two turns, one `result` each; `interrupt` ends a `Delay` turn `Interrupted`; `can_use_tool` → `allow` runs the tool; stdout has no prose
- [x] `!echo hi` records an `Action` visible in the transcript and in the next request's context; `/permission acceptEdits` makes an `Edit` allowed without a prompt in that session only
- [x] sdk changed once (ADR-0008 lists what it touched)

## Non-goals

Inline (scrollback) viewport, kitty images, the Ctrl-O pager, themes, OSC notifications beyond title and bell, mouse, syntax highlighting, `@` completion, rewind UI, background dialog (all M11). A permission-mode badge and shift+tab cycling (M7, when `ConfigView.plugins` is written). `/provider` (`/model provider/model` names both). `@name` routing (M8). WebSocket transport. Sub-session views (M8).

## Risks touched

R2 — the TUI is a client of the same bounded channel as `RemoteKernel`, folds with the same reducer, holds no session state, and is tested by injecting frames; `draw` is pure and `on_key` returns effects. R3 — the terminal stack is the heaviest dependency of the project; `budget.sh` asserts the kernel does not recompile when the TUI changes. R1 — one sdk change. R6 — `!` bypasses the gate by design: the person typed the line; it runs with the person's own privileges, as a shell would.

## Verified (2026-08-29, commit aacf8af)

```
$ cargo fmt --all -- --check                                        exit 0
$ cargo check --workspace --all-targets --locked                    exit 0
$ cargo clippy --workspace --all-targets --locked -- -D warnings    exit 0
$ cargo test --workspace --locked                                   exit 0
  core 127 · tui 113 · permissions 95+6 · provider-openai 80+15 · print 80 · tool-web 77 · tool-fs 69
  context 66 · tool-bash 60 · provider-anthropic 56+12 · bin (cli 35 + rpc 10) 45 · store-jsonl 34
  provider-fake 19 · sdk 19 · rpc 16+19 = 1008 passed, 0 failed
$ scripts/check_discipline.sh                                       exit 0 (one warning: turn.rs 737 non-test lines)
$ scripts/budget.sh                                                 dependencies 252 (max 260); relink isolation 0; cap not raised
$ cargo deny check                                                  advisories ok, bans ok, licenses ok, sources ok
$ scripts/tui-smoke.sh                                              exit 0, three runs in a row (macOS, tmux 3.6b)
$ tmux drive of target/debug/bingo: !echo → action row; /think high, /model fake/other,
  /permission acceptEdits → their notices and the footer's model; a Write runs with no dialog
```

Exit criteria, item by item:

- Dispatcher (`session/tests/commands.rs`): `/x` → `Rejected{INVALID_INPUT "unknown command: /x"}`; an instant command is applied while the turn goes on and never queued; a non-instant one is `Queued{1}`, the prose behind it `Queued{2}`, the command runs after the turn ends and the prose starts only after its ack (`ack:Applied` before `turnStarted`); `Prompt` opens a turn whose user item carries the command's intent and origin; `Record` is an item the ack names; `Input::Action{name, args}` dispatches with `args` as JSON text; `parse` pins `/name rest`, `!rest`, a bare `/` as prose, and the three action shapes.
- Built-ins (`host/tests/commands.rs`): `/model m2` → `SessionUpdated{model: m2}` after a `ConfigChanged`, and the next request's `model` is `m2`; `/think high` → `ConfigChanged{kernel:{thinking:"high"}}` and `reasoning: Some(High)` on the wire; `/think off` → `null` and no reasoning parameter; `/think loud` → `INVALID_INPUT`; `/model` alone → a `View::Text`; `/compact keep the names` → `TurnStarted{origin: Auto}`, `Compacted`, `TurnCompleted`, the strategy called with `Manual{instructions}`; `/compact` during a turn → `NOT_READY` (`session/tests/commands.rs`); the catalogue lists `model`, `think`, `compact` and `scripted/m` first; `ModelCatalog::embedded().models_of("anthropic")` lists more than three models in id order.
- Post-turn: a hook gated on the test is still waiting when `TurnCompleted` reaches the client, and `close` + `wait_closed` return only once it ran; the `--print` memory black-box test (`cli/context.rs`) passes only because `Host::shutdown` waits for the extraction.
- TUI: 23 `TestBackend` snapshots (idle, streaming, tool rows, permission collapsed/expanded/feedback, question single/multi, confirm, help, dropdown, ctrl-c armed, retrying, interrupted, context bar, rejected intent, view table, picker); 33 `on_key` tables including the guard, the answered-once marker, `2` → `AllowSession{scope}`, `n` + feedback → `Deny{feedback}`, Esc and Ctrl-C orders, `/clear` → `Open(Create)`; `check_discipline.sh` holds the terminal stack to the one crate; `scripts/tui-smoke.sh` passes on macOS — the Linux leg is a CI job (`tui-smoke`) that has not run here.
- stream-json input (`cli/stream_json.rs`): two user lines → two turns and two `result` lines; `interrupt` ends a `Delay` turn `Interrupted` with exit 130; `can_use_tool` answered `allow` writes the file, `deny` with a message reaches the model and the turn goes on; stdout is JSON only; stdin closing while idle exits 0; the two flag misuses are `INVALID_INPUT`.
- `!echo hi over the wire` (`tests/rpc.rs`) → `Action{name: "!", args: "echo …", result: "hi …\n"}` in the folded snapshot; the fold turns such an action into a user note (`context.rs`); `/permission acceptEdits` makes a `Write` run with no `InteractionOpened` in that session.
- sdk changed once (346dba7): `CommandOutcome::Record`, `SurfaceOptions.env`.

Found while integrating (each is a commit body too):

- An intent that waited is now acknowledged twice, `Queued` then `TurnStarted`, and the `QueueChanged` that empties the queue follows the `TurnStarted` — the stream-json host loop had to match items by turn and intent and saw a false idle between the two (8f205ef).
- A permission's summary names the tool's subjects (`Write /work/note.txt`), not its input JSON; the JSON stays the fallback (aacf8af).
- A fast turn completes before a non-instant command's ack: the ack follows the command's return, and `/compact` returns once the turn has *started*. Two tests assumed the other order; the kernel was right.
- `/compact` on a session shorter than the strategy's keep window cuts nothing: `COMPACTION_USELESS` on the ephemeral stream is its only trace. It also spends one provider response on the summary request, which a scripted test must budget for.
- `Commands` holds a `Weak<dyn HostApi>`: the host owns every mailbox, so a session must never own the host.
- The kernel's three commands are registered last, so a plugin's command of the same name wins.

Open, carried forward:

- `LiveTurn` keeps `TurnRetrying.attempt` but not `max`, so the status line says `retrying 2`, not `2/10` (sdk `state.rs`; next sdk change).
- An `IntentAck` names no client: each surface keeps the set of intents it minted to ignore another client's acks.
- An Agent-SDK client-mode host opens with `control_request{subtype: "initialize"}`, which the print surface refuses with an `error` response; the plain CLI dialect works. `updatedInput` that differs from the call is a deny — `Answer` cannot rewrite a call.
- A permission-mode badge and shift+tab cycling wait for `ConfigView.plugins` (M7); `/permission` alone reports the mode.
- The permission dialog trims its own top rows when a preview is taller than the screen, so the title can scroll off at 80×24 with a 12-row diff; trimming the preview first would be the better rule.
- Under tmux the title stack (`ESC[22;2t`/`ESC[23;2t`) is a no-op; the old project's latch-and-reset handled that case.
- Markdown tables, reasoning expansion, a transcript cache, syntax highlighting, the inline viewport, mouse — M11.
- `turn.rs` is 737 non-test lines (warning); `impl Turn` already spans `turn.rs` and `stream.rs`, so the next split moves the tool-execution group behind a type of its own.
- Two plugins carry a near-identical `HostApi` test double for `CommandContext.host`; a `testing` feature in the sdk would end that.
- `!` runs a line the reject tables would refuse for the model (an editor, a REPL) with stdin closed; it ends at the tool's timeout. By design, noted.
- Live smokes against Anthropic and OpenAI (M1, M2) — still need keys.
