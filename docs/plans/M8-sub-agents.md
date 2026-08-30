# M8 — Sub-agents: five verbs on two kernel primitives

## Goal

The model spawns a sub-agent and it is a session: same journal, same reducer, same `draw`. `SpawnAgent` opens a child under the calling tool item, `SendMessage` and `FollowupTask` post into its queue, `WaitAgent` blocks until it is idle, `ListAgents` reads the session tree. A background child that finishes wakes its parent with a peer message. `@reviewer fix it` in the composer reaches the child. A permission prompt raised by a child appears in the root's TUI or `--print` host and is answered through the root's handle. `.bingo/agents/<name>.md` names a persona, model and tool set. Kernel: peer delivery, redirect, tree attachment (ADR-0010); nothing in `bingo-sdk` or `bingo-core` says agent.

## Bricks, in build order (owner)

1. **ADR-0010 + sdk** (kernel) — `OpenOptions { children: bool }` as the third parameter of `HostApi::open`; `Delivery::{Wake, Hold}`; `ToolHost::deliver(to, intent, input, delivery) -> Result<(), KernelError>` replacing `submit`; `ItemBody::ToolCall.child_session` removed. One change; the workers build on it.
2. **Actor** (kernel) — `Msg::Deliver`; `deliver()` records prose with the sender's origin, no command parse, no `on_submit`; idle + `Wake` → `start_turn(held prose ++ this, TurnOrigin::Peer)`; idle + `Hold` → `enqueue`; busy → `enqueue`; `submit_prose` on an idle session also takes held prose first. `Redirect{session}` in `submit_prose` → `tool_host.deliver(session, fresh intent, input, Wake)` then `Applied{"redirected"}` or `Rejected{SESSION_NOT_FOUND}`. `SessionToolHost::deliver` → `host.live(to)?.mailbox.deliver`. The fold prefixes a `User` item with `origin.principal` as `[from <principal>]`.
3. **Host** (kernel) — `descendants(root)`; `host/tree.rs`: a forwarder task over the root's stream, every live descendant's `events_since(0)`, and the gateway (subscribed before the descendants are listed, so a creation cannot fall between); a child's `Lagged` re-subscribes from its last forwarded `seq` and is not forwarded; the forwarder keeps `interaction → mailbox` from the `InteractionOpened/Resolved/Cancelled` frames it passes; `TreePort` routes `answer` by that map and everything else to the root. `open` builds it when `options.children`. `delete` closes and deletes descendants first.
4. **RPC** (kernel) — `OpenParams.options` (serde default), `RemoteKernel::open` passes it, `schema/rpc.json` regenerated; one wire test: open with children, spawn a child through a scripted tool, the notifications carry the child's `session`.
5. **Print** (kernel) — stream-json opens with `children: true` and folds one `SessionState` per session; a child frame's `parent_tool_use_id` is its summary's `parent.item`; `session_id` stays the root; text mode opens without children. Black-box in `tests/cli/stream_json.rs`.
6. **`bingo-agents`** (worker A) — definitions: `<config_dir>/agents/*.md`, then `.bingo/agents/*.md` from cwd upward (nearest wins), frontmatter `name?`, `description`, `model?`, `provider?`, `tools?: [names]` (serde-saphyr, the skills crate's shape), body = system prompt appended as `system_extra` after a fixed sub-agent note (you are a sub-agent; your final text is the result; `SendMessage(to: "parent")` between turns; no `AskUserQuestion`). Tools, all `read_only: true, trusted: true` (a child's own calls go through the gate): `SpawnAgent{prompt, agent?, name?, background?: true, model?, provider?, tools?}` → `spawn_session(SessionSpec{cwd: cx.cwd, key: agent/<parent>/<name>, parent: {cx.session, cx.item}, title: name, tools: definition's or every registered tool minus SpawnAgent})`, then `deliver(child, prompt, Wake)`; background → `{name, session}` at once, and a watcher (`host.open(ById(child))` from the `HostHandle` kept at `start`) delivers `[<name> finished]\n<final text>` to the parent with `Wake` when the turn it started completes; foreground → wait, return the final assistant text. `SendMessage{to, text}` → `Hold`; `FollowupTask{to, text}` → `Wake`; `WaitAgent{name, timeout_s?}` → idle snapshot or the next `TurnCompleted`, `Interrupt::Cancel`; `ListAgents{}` → name, session, busy, last turn. `to`/`name` resolve among `sessions{parent: cx.session}` by `title`, plus `parent`. Name collisions get `-2`, `-3`. A `Hook` on `Submit`: `@<name> rest` naming a child (or a sibling) → strip the prefix, `Redirect`; otherwise `Continue`. `/agents` (instant) → `View::Table`. Unit tests on sdk doubles; black-box scenarios in `crates/bingo/tests/rpc.rs` and `tests/cli/agents.rs`.
7. **TUI** (worker B) — open the root with `children: true`; `states: BTreeMap<SessionId, SessionState>` fed by `frame.session` (a new child starts from its head `SessionUpdated`); `view: SessionId` chooses which state `draw` gets — no second render path; the dialog focuses the first open interaction anywhere in the tree with the child's title in its frame; a `ToolCall` row whose id is a known child's `parent.item` grows a `↳ <title> · running|idle` line; the switcher (`ctrl+g`, `/agents` stays the kernel's table) lists root and children with busy and attention marks, enter switches the view, `esc` returns; typing in a child view submits through a handle fetched once with `open(ById)` (`Reply::Handle`), the root keeps the tree stream; title and bell read attention across the tree; `/resume` re-attaches as before. `TestBackend` snapshots: two-session fold, dialog from a child, the `↳` row, the switcher.
8. **bin** (kernel) — `AgentsPlugin` after `McpPlugin`; budget number recorded.

## Files

`docs/adr/0010-sub-sessions.md`, `crates/bingo-sdk/src/{host,tool,event}.rs`, `crates/bingo-core/src/{session,host,context}.rs`, `session/mailbox.rs`, `host/{tool_host,tree}.rs`, `crates/bingo-surface-rpc/src/{methods,server,client}.rs`, `schema/rpc.json`, `crates/bingo-surface-print/src/{lib,hosted,stream_json}.rs`, `crates/bingo-agents/**`, `crates/bingo-surface-tui/src/{run,ui,view,input,dialog,transcript}.rs`, `crates/bingo/src/main.rs`, `crates/bingo/tests/{rpc.rs,cli/agents.rs,cli/stream_json.rs}`, doubles in `bingo-tool-bash`, `bingo-permissions`, `bingo-skills`, `bingo-mcp`.

## Dependencies

None new. `bingo-agents` uses `serde-saphyr` (workspace) for frontmatter. `budget.toml` rises by the one workspace crate; the number goes in Verified.

## Exit criteria

- [x] `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `scripts/check_discipline.sh`, `scripts/budget.sh`, `cargo deny check`, `scripts/tui-smoke.sh`
- [x] Actor: `deliver(Wake)` to an idle session opens a `Peer` turn whose user item carries the sender's origin; `deliver(Hold)` queues it and the next client submit runs it first, in order; `deliver` to a busy session is absorbed at the barrier; a `Redirect` hook lands the rewritten text in the target and the source acks `Applied{redirected}`; an unknown target is `Rejected{SESSION_NOT_FOUND}`; the fold shows `[from reviewer]`
- [x] Host: a `children: true` attachment yields a child's frames from `seq` 1, for a child created after the attachment too, each with its own `session`; answering a child's permission through the root handle resolves it and the child's tool runs; a child stream that lags is healed without a `Lagged` reaching the client; `delete(root)` removes the child from `sessions`
- [x] RPC: `session/open` with `options.children` streams the child's `event` notifications; schema unchanged but for `OpenParams` and `ToolCall`
- [x] Print: `--output-format stream-json` lines from the child carry `parent_tool_use_id` = the `SpawnAgent` call id and the root's `session_id`; text `--print` writes only the root's text
- [x] agents: layer order and override; the sub-agent note in the child's system prompt; foreground `SpawnAgent` returns the child's final text; background completion wakes the parent with a `Peer` turn whose user item says who; `SendMessage` is `Queued`, `FollowupTask` starts a turn; `WaitAgent` on an idle child returns at once; `@name` redirects and `@nobody` does not; `/agents` table; `ListAgents` names the child and its state; the kernel-noun grep stays clean
- [x] TUI: two-session fold snapshot; a child's permission dialog names the child and `y` resolves it; `↳ reviewer · running`; switcher lists and switches; a line typed in a child view reaches the child
- [x] Black-box: `--print` with a script whose first step calls `SpawnAgent` finishes with the child's text in the result; the RPC scenario sees a child frame whose `parent.item` is the root's tool item
- [x] sdk changed once (ADR-0010 lists what it touched)

## Non-goals

Teams, rooms, tasks (M9). A definition as the primary persona (`mode`). Per-agent permission mode or thinking level (needs `SessionSpec` growth). Killing or deleting an agent from the model. Depth beyond 1. A roster `Extension`. Agent-to-agent `@` across trees. An agents plugin over the wire.

## Risks touched

R5 — `bingo-agents` stays under 2K lines and the plugin owns every noun; `check_discipline.sh`'s kernel-noun grep gains `agent`. R1 — one sdk change. R2 — the TUI holds a map of the reducer's states, never a state of its own; a child view is a key into that map. R6 — the five tools are read-only by declaration and a child's calls are gated in the child; a prompt that reaches the root is the child's own interaction, never a copy.

## Verified (2026-08-29, commit 81078d6)

```
$ cargo fmt --all -- --check                                        exit 0
$ cargo check --workspace --all-targets --locked                    exit 0
$ cargo clippy --workspace --all-targets --locked -- -D warnings    exit 0
$ cargo test --workspace --locked                                   exit 0
  core 145 · tui 145 · hooks-shell 98 · permissions 96+6 · print 82 · provider-openai 80+15
  tool-web 77 · agents 70 · tool-fs 69 · skills 68 · context 66 · tool-bash 60
  provider-anthropic 56+12 · bin (cli 44 + rpc 13) 57 · mcp 46+14 · store-jsonl 34
  provider-fake 19 · sdk 19 · rpc 16+20 = 1370 passed, 0 failed
$ scripts/check_discipline.sh                                       exit 0 (four size warnings, below)
$ scripts/budget.sh                                                 dependencies 265 (max 265, was 264); relink isolation 0
$ cargo deny check                                                  advisories ok, bans ok, licenses ok, sources ok
$ scripts/tui-smoke.sh                                              exit 0
$ tmux drive of the real binary: `SpawnAgent` runs, the root shows `↳ reviewer · done` under the
  call and `1 agent · 1 needs you`, ctrl+g lists root and reviewer, enter switches to the child's
  own transcript, ctrl+g back, `@reviewer hello there` reaches the child, and the child's view
  shows its answer
```

Exit criteria, item by item:

- Actor (`session/tests/peers.rs`): `a_wake_delivery_to_an_idle_session_opens_a_peer_turn_that_says_who_spoke` — `TurnOrigin::Peer`, the item's origin, and `[from reviewer]` in the request the provider saw; `a_held_delivery_waits_in_the_queue_and_the_next_submit_carries_it_first` — `Queued`, then one turn carrying both inputs in order; `a_delivery_to_a_busy_session_is_queued_whatever_its_kind`; `a_peer_may_not_deliver_an_action`; `a_redirect_hook_sends_the_line_elsewhere_and_acks_where_it_went` — the target records the stripped text, the source records nothing and acks `Applied{redirected}`; `a_redirect_to_a_session_that_is_gone_is_rejected`. The fold's prefix: `a_user_item_from_a_named_principal_says_who_spoke` (`context.rs`).
- Host (`host/tests/tree.rs`): `a_child_opened_after_the_attachment_streams_from_its_head_and_is_answered_through_the_root` — the child's head arrives on the root's stream and `AllowOnce` through the root handle resolves the child's permission and runs its tool; `a_child_that_already_exists_is_followed_from_its_head_too` (from `seq` 1, contiguous); `a_lagging_child_is_healed_and_the_client_never_sees_a_marker` — 768 held deliveries past a 256-frame channel, every durable `seq` arrives and no `Lagged` reaches the client; `deleting_the_root_deletes_its_children_first`.
- RPC: `a_tree_attachment_routes_a_childs_frames_to_the_root_stream` (wire tests) — the `event` notification carries `root`, and `RemoteKernel` routes the child's frame to the root's stream. `schema/rpc.json` regenerated for `OpenParams.options`, `EventParams` and `ToolCall`.
- Print: `a_sub_sessions_lines_carry_the_call_that_spawned_it` (`cli/stream_json.rs`) — the child's line carries the root's `session_id` and the `SpawnAgent` call id, there is exactly one `result` line and it is the root's; `a_sub_sessions_turn_ending_does_not_end_the_run` and `a_sub_sessions_lines_name_the_root_and_the_call_that_spawned_it` (unit).
- agents (70 tests): layers and override, frontmatter, the note in `system_extra`, name suffixing under a live-key collision, the tool set a child is offered, foreground text, `a_background_spawn_names_the_child_at_once`, `a_finished_background_agent_wakes_its_parent_without_signing_the_text`, `SendMessage` holds / `FollowupTask` wakes, `WaitAgent` on an idle child, `ListAgents`, `@name` stripped and redirected / an unknown name left alone, every tool read-only and trusted. Black-box: `agents::a_foreground_agent_answers_the_root_and_stdout_stays_the_root_s`, `agents::the_child_s_reply_is_the_tool_call_s_result`, `agents::a_child_has_no_spawn_agent_to_call`, `agents::a_project_definition_names_the_agent_it_starts` (`cli/agents.rs`); `a_foreground_agent_is_a_child_session_on_the_root_s_attachment` and `a_background_agent_wakes_the_root_and_says_who_it_is` (`tests/rpc.rs`).
- TUI (145 tests): `tree.rs` (join on the head frame, view fallback, an unknown session never shown, the tally and root-first dialog, a child found by its parent item, `done`); `run.rs` (a child's frames folded into its own state, a child's permission answered through the root handle, a line typed in a child's view submitted on the child's handle, a child closing without ending the run); `input.rs` (ctrl+g lists and switches, toggles and `esc`, opens on the child in view, a child's prompt answered from the root view, `/clear` from a child view); four snapshots (the `↳` row, the band, a child's transcript, the switcher).

Found while integrating (each is a commit body too):

- The wire needed more than `options`: `RemoteKernel` files a frame under `frame.session`, and a client only claims the root's route, so a child's frames landed in an unclaimed one. An `event` notification under a tree attachment now carries `root`, and the client routes by it.
- `--print`'s single-prompt loop ended the run on the first `TurnCompleted` it saw, which under a tree attachment is the child's. Both print loops now react to the root's frames only, plus any interaction wherever it was opened.
- A child is offered neither `SpawnAgent` (the depth limit refuses it) nor `AskUserQuestion` (the note says it does not have it, so the tool set says so too).
- `agent` joined the kernel-noun grep in `check_discipline.sh`; it is a plugin noun like `room` and `team`.

Reviewed (2026-08-30, on `2c7acb1`). Three defects the exit criteria never named, each reproduced on the binary before it was fixed, and one line of the ADR the code had outgrown:

- `--print` in text and json mode hung forever when a foreground sub-agent asked a permission: only stream-json attached to the tree, so nobody saw the child's prompt. Every mode now opens with `children: true`; what of the tree a mode reports is the renderer's decision (`Renderer::reports`), and text and json still report the root alone. Black-box `agents::a_childs_permission_is_refused_off_a_tty_in_every_output_format` runs under a 30 s guard (`run_within`) and was seen to fail at that guard with the fix reverted; unit `a_text_run_refuses_a_sub_sessions_prompt_and_keeps_its_prose_off_stdout`, `a_json_run_reports_the_root_alone_while_answering_the_tree`, `text_and_json_report_the_root_alone_and_the_envelope_the_tree`.
- A child whose turn failed or was interrupted was reported as `finished without saying anything`, `is_error` unset, foreground and background alike: `watch::next_reply` read `TurnCompleted` and never its status. It now yields a `Reply { status, text }`; `watch::output` is an error result for anything but `Completed`, and the text says `failed: …` or `was interrupted: …`, then whatever the child had said. Black-box `agents::a_child_that_failed_is_an_error_result_for_the_root`; unit `a_turn_that_did_not_complete_is_never_read_as_an_answer`, `a_background_agent_that_failed_tells_its_parent_so`, `an_idle_child_whose_last_turn_failed_remembers_that`, `a_foreground_spawn_reports_a_child_that_failed_as_an_error`, `an_idle_agent_whose_last_turn_failed_says_so`.
- `parent` was accepted as an agent's name, and such a child could never be written to: `resolve` reads the word as the address it already is. `names::check` refuses it (`a_name_that_would_break_the_key_or_the_address_is_refused`).
- ADR-0010 §3 said the root's lag is reported as before; `host/tree.rs` heals every session's, the root's included. The ADR now says what the code does.

Gates after the fixes: fmt, check and clippy clean; 1380 passed, 0 failed; discipline ok with the same four size warnings; budget ok (265 of 265); `cargo deny check` ok.

Open, carried forward:

- After a resume, `ListAgents` lists a persisted child as `idle` (`host.sessions` merges the store) and `SendMessage` to it fails with `no session …` (`deliver` reaches live sessions only) — reproduced on the binary. Neither `HostApi` nor `SessionSummary` says whether a session is live; the fix is a kernel fact (`SessionSummary.live`, or a filter), owed to M9's plan.
- A child's default tool set is the host's whole catalogue, while `SpawnAgent` says "every tool this session has": a parent narrowed by `SessionSpec.tools` gets a wider child. The gate still stands in the child; the description, or an exclusion form of `tools`, should be made true.
- `FollowupTask` returns before the child's actor has seen the delivery, so a `WaitAgent` right after it can attach to a still-idle child and return the previous reply. Attach, deliver, then await the intent's ack, as `SpawnAgent` does.
- `SendMessage`, `FollowupTask` and `WaitAgent` have no black-box scenario on the binary; nor do `@name` and `/agents`.
- `crates/bingo-core/src/session.rs` is 941 non-test lines (fails at 1000), and `crates/bingo/tests/rpc.rs` 913. The actor wants splitting, but its inherent impl already uses its two-file allowance (`session.rs`, `session/interactions.rs`), so the cohesion rule and the size rule now pull against each other: the next kernel change owes an ADR line and a third file.
- A background child's report reaches an idle parent as a `Peer` turn and a busy one as `Queue`, and no script can order two sessions against one provider cursor. `TurnOrigin` says which door a turn came through, not who wrote its input — that is `origin.principal` on the item — so the RPC scenario asserts the item, its principal and the turn that carried it, and never the origin.
- `ToolHost` cannot read one session's summary or open one, so `bingo-agents` keeps the `HostHandle` from `start` in a `OnceLock`, and `names::own` lists every session, store included, to find the caller's — once per `SendMessage`. Wants `SessionFilter.id` or a `HostApi::summary`.
- A child's "finished" is inferred (idle with a last turn), not a fact on the wire; the TUI's `done` and the watcher's report both read it that way.
- Switching to a child in the TUI opens a second attachment for its handle and never closes it; a `HostApi::handle(session)` would remove that.
- The watcher reports once, for the turn the spawn started. A follow-up's result reaches the parent only through `WaitAgent` or the child's own `SendMessage(to: "parent")`.
- Everything M7 carried is still open (plugin notices, `HookContext.permission_mode`, `CommandSpec.description`, an sdk `testing` feature, the live provider smokes).
