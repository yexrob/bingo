# M9 — Collaboration: teams, rooms, tasks on three kernel facts

## Goal

A project's `.bingo/team.json` names resident agents; opening a session in that project seats them as children of it, at zero tokens, and `@reviewer`, `SendMessage`, the switcher and `/agents` already know them. A room is a session nobody answers: `/room design reviewer scout` opens `#design` under the person's session, a post into it reaches every member's queue — an idle member wakes, a busy one absorbs it at the barrier — and the room's transcript is drawn by the same reducer and `draw` as any session. A task list is a session's own state: four tools write it, `ctrl+t` shows it, `--continue` reads it back. Kernel: `Driver::Log`, `HostApi::extend` + `SessionState.extensions`, the host in every tool's and hook's hand (ADR-0011); nothing in `bingo-sdk` or `bingo-core` says room, team, task or agent.

## Bricks, in build order (owner)

1. **ADR-0011 + sdk** (kernel) — `Driver::{Model, Log}` on `SessionSpec` and `SessionSummary`; `ParentLink.item: Option<ItemId>`; `HostApi::deliver(to, intent, input, delivery)` and `HostApi::extend(session, plugin, kind, payload)`, both async; `ToolHost` keeps `ask`, `progress`, `record`; `ToolContext { host: HostHandle, call: Arc<dyn ToolHost>, .. }`; `HookContext.host: HostHandle`; `SessionState.extensions: BTreeMap<String, BTreeMap<String, Value>>` fed by `Event::Extension`. One change; the workers build on it.
2. **Actor** (kernel) — `session/inputs.rs` takes submit, commands' completion, prose, redirect, deliver, queue and interrupt out of `session.rs` (the cohesion rule's third file). A `Log` session (`config.model.is_none()`) records prose and deliveries as `User` items with `turn: None` and acks `Applied{item}`; `/model`, `/think`, `/compact` are refused; an interrupt is refused as on any idle session. `Msg::Extend` publishes a durable `Event::Extension`. `redirect` and the executor reach the host through `TurnConfig.host: HostHandle`; `hook_context()` carries it and `provider`/`model` as `None` for a `Log` session.
3. **Host** (kernel) — `create` and `resume` skip `choose_model` for `Driver::Log` (`turn_config(.., Option<ModelChoice>, ..)`, the turn keeps its own `model` once it starts); `reconfigure` refuses a `Log` session; `mailbox_of(id)` is live-or-reopen and serves `deliver` and `extend`; `SessionToolHost` shrinks with the trait. The fold prefixes `[from <principal> in <conversation>]`.
4. **Print + RPC** (kernel) — `parent_tool_use_id` is `null` for a child without a call; the snapshot carries `driver` and `extensions`; `schema/rpc.json` regenerated.
5. **`bingo-agents`** (worker A) — on the sdk: `LateHost` deleted, `SpawnAgent` opens the child with `open(Create{spec}, ..)` and watches that attachment, the hook uses `cx.host`. `names::resolve`: `#name` is a session titled `#name` among the caller's children or its parent's children. `SendMessage` to a `Log` target receipts `posted to #name` (no "next turn"). `team.rs`: `.bingo/team.json` from cwd upward — `{ "roles": [{ "name", "agent"?, "system"?, "model"?, "provider"?, "tools"? }], "norms"?: "<path>" }`; a `Session` hook on a root session (no parent) seats every role: the persisted child of that title is reopened, else one is created (key `agent/<root>/<name>`, `system_extra` = the sub-agent note + a norms block from the file + the role's or its definition's system, `parent.item: None`); no provider is called. `/team` → `View::Table` of roles declared and their state. The note gains: a message marked `in #room` came from a room; answer there with `SendMessage(to: "#room")` or stay quiet. Tests on the `Fleet` double; the crate stays under 2K non-test lines.
6. **`bingo-rooms`** (worker B, new crate, plugin id `bingo.rooms`) — `/room` lists the rooms under this session; `/room <name> [member…]` creates `#name` under the calling session (`Driver::Log`, `title: "#name"`, `key: rooms/<parent>/<name>`, `parent.item: None`) or resets its members, and `extend(room, "bingo.rooms", "members", {"members": [..]})`. A root's `Session` hook reads `team.json`'s `"rooms": [{ "name", "members" }]` the same way. An `Event` hook: `SessionUpdated` with `driver: Log` and a `rooms/` key registers the room and its parent; an `Extension{bingo.rooms, members}` frame updates the plugin's fold of it; an `ItemCompleted{User}` in a room fans the text out to every member but the author — members by title among `sessions{parent: room.parent}` — with `Origin { surface: "room", principal: the author's principal else "parent", conversation: "#name" }` and `Delivery::Wake`; `deliver` reopens a member that is not live. No serial mode, no `@` ledger, no watchdog, no budget.
7. **`bingo-tasks`** (worker C, new crate, plugin id `bingo.tasks`) — `TaskCreate{subject, description?, activeForm?, owner?, blockedBy?, blocks?, metadata?}`, `TaskUpdate{id, subject?, description?, activeForm?, status?, owner?, addBlockedBy?, addBlocks?, metadata?}`, `TaskGet{id}`, `TaskList{}` — Claude Code's shapes, statuses `pending | in_progress | completed`, ids `1, 2, …` per session. The list is the session's extension `bingo.tasks/tasks`, read from `host.open(ById(cx.session), ..).snapshot.extensions` and written back whole with `host.extend`. A `System` contributor (late order) lists open tasks when there are any. `/tasks` → `View::Table`. Tools `read_only: true, trusted: true, concurrency_safe: false`.
8. **TUI** (worker D) — a user item whose origin names a principal draws `<principal>: ` before its text, so a room reads as a chat; a `Log` session shows no spinner and no status in the switcher, and its composer says `post to #design`; `ctrl+t` toggles a panel over the viewed session's `extensions`, rendered generically (an array of flat objects is a table, an object a key/value list, else text; plugin and kind as headings); `TestBackend` snapshots for the chat, the panel and a room row.
9. **bin** (kernel) — `RoomsPlugin`, `TasksPlugin` after `AgentsPlugin`; budget number recorded.
10. **Black-box** (kernel) — `--print` in a directory with `team.json`: a `ListAgents` step names the roles and the provider script was not consumed by them; RPC: `/room design reviewer`, a background `reviewer`, a submit into `#design`, the reviewer's stream carries a `User` item with `origin.conversation == "#design"` and a turn opened on it; `--print` twice with `--continue`: `TaskCreate` then `TaskList` reads the journal; a room member that asks a permission reaches the root's TUI and `--print` as in M8.

## Files

`docs/adr/0011-log-sessions-and-plugin-state.md`, `crates/bingo-sdk/src/{host,tool,hook,event,state}.rs`, `crates/bingo-core/src/{session,host,context,executor,turn}.rs`, `session/{inputs,mailbox}.rs`, `host/{tool_host,resume}.rs`, `turn/config.rs`, `scripts/check_discipline.sh`, `crates/bingo-surface-rpc/src/methods.rs`, `schema/rpc.json`, `crates/bingo-surface-print/src/stream_json.rs`, `crates/bingo-agents/src/{names,spawn,message,hook,note,team,command}.rs`, `crates/bingo-rooms/**`, `crates/bingo-tasks/**`, `crates/bingo-surface-tui/src/{transcript,view,input,keys,run,ui}.rs`, `crates/bingo/src/main.rs`, `crates/bingo/tests/{rpc.rs,cli/agents.rs,cli/rooms.rs,cli/tasks.rs}`, doubles in every crate that implements `HostApi`, `ToolHost` or builds a `ToolContext`/`HookContext`.

## Dependencies

None new. Two workspace crates: `budget.toml` 265 → 267.

## Exit criteria

- [x] `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `scripts/check_discipline.sh`, `scripts/budget.sh`, `cargo deny check`, `scripts/tui-smoke.sh`
- [x] Actor: a `Log` session records a submit and a `Hold` and a `Wake` delivery as `User` items with no turn and acks `Applied{item}`; `/model` on it is refused; an interrupt is `NOT_READY`, as on any idle session; `Msg::Extend` publishes a durable `Extension` the reducer keeps as the latest payload per kind, in a snapshot and after a replay
- [x] Host: `deliver` to a session that is persisted but not live reopens it and the item lands; `extend` likewise; `create` with `Driver::Log` calls no provider; the fold reads `[from reviewer in #design]`
- [x] agents: roles from `team.json` are children of the root after it opens, with the norms in their system prompt, and no provider response was consumed; the same roles come back on `--continue`; `#design` resolves from the root and from a sibling; `SendMessage` to a room receipts a post; `/team` names declared and running roles; `LateHost` is gone
- [x] rooms: a post in a room reaches every member but the author with the room in its origin; an idle member opens a `Peer` turn on it and a busy one absorbs it at the barrier; membership survives a restart; `/room` lists
- [x] tasks: the four tools round-trip; `TaskList` after `--continue` reads what `TaskCreate` wrote; the contributor injects open tasks and nothing when there are none; an `Event` hook sees `Extension{bingo.tasks}`
- [x] TUI: a room transcript shows who spoke; `ctrl+t` shows tasks as a table; a `Log` session has no spinner and no status
- [x] Every surface, for the new session kind: a room member that asks a person reaches the root's TUI and a `--print` run; a room member that fails is reported as failed (M8's rule)
- [x] `check_discipline.sh`'s noun grep stays clean; sdk changed once (ADR-0011 lists what it touched)

## Non-goals

Serial rooms, `@` debt, ack chasing, avatars, org trees, hires. Team memory across root sessions (a role's transcript persists with its root; extraction into a per-project memory is its own ADR). `TaskCreated`/`TaskCompleted` shell hook events. A per-plugin TUI widget. `#room` as a direct send from the composer (switch to the room and type). Members outside the room's parent's tree. `Driver::Log` over ACP. A `SessionFilter.id` or `HostApi::summary` (carried from M8).

## Risks touched

R5 — three plugins own every noun; `bingo-agents` gains `team.rs` and must stay under 2K non-test lines, else definitions move to a crate of their own. R1 — one sdk change, and it deletes more than it adds. R2 — the rooms plugin's member map is a fold of the frames it observes, never a second fact; the TUI panel renders the reducer's `extensions`. Cost — `bingo-tasks` snapshots the session per call; measured in Verified, carried if it shows.

## Verified (2026-08-30, commit 6f58eb6)

```
$ cargo fmt --all -- --check                                        exit 0
$ cargo check --workspace --all-targets --locked                    exit 0
$ cargo clippy --workspace --all-targets --locked -- -D warnings    exit 0
$ cargo test --workspace --locked                                   exit 0 — 1521 passed, 0 failed
  new: bingo-tasks 45 · bingo-rooms 39 · bingo-agents 96 (was 74) · tui 162 (was 145) · core 157 (was 145)
  bin: cli 49 (tasks, teams, a childs permission in every format) · rooms 2 · rpc 13
$ scripts/check_discipline.sh                                       exit 0 (size warnings: core/turn.rs 775, tests/rpc.rs 793, tui/test_support.rs 741)
$ scripts/budget.sh                                                 dependencies 267 (max 267, was 265: two workspace crates); relink isolation 0
$ cargo deny check                                                  advisories ok, bans ok, licenses ok, sources ok
$ scripts/tui-smoke.sh                                              tui-smoke ok
$ tmux drive of the real binary (live-m9.sh): a cwd with team.json seats `reviewer` before the first
  turn; `/team` shows `reviewer - ses_… idle`; `/room` shows `#design reviewer`; ctrl+g lists
  `cwd · done`, `reviewer · idle`, `#design`; the room view says `post to #design`; a post there
  wakes the reviewer, whose transcript reads `❯ parent: hello team, thoughts?` then its reply;
  ctrl+t on the room shows `bingo.rooms · members`, on the root the tasks table `1  write the plan  pending`
```

Exit criteria, item by item:

- Actor (`session/tests/log.rs`): `a_submit_is_recorded_at_once_and_opens_no_turn`, `a_delivery_of_either_kind_is_recorded_the_same_way`, `nothing_compacts_and_nothing_is_there_to_interrupt`, `an_extension_is_durable_folded_and_the_latest_payload_is_the_state`, `a_resumed_session_restates_its_extensions_for_the_observers`; `a_start_hook_finishes_before_the_first_turn_opens` (`session/tests.rs`).
- Host (`host/tests.rs`): `a_log_session_needs_no_provider_and_answers_nothing`, `a_delivery_reaches_a_stored_session_that_is_not_live`, `latest_prefers_a_root_over_the_newer_child_under_it`, `a_resumed_session_keeps_its_system_prompt_and_tool_set`, `a_start_hook_may_read_the_session_tree_and_the_first_turn_waits_for_it`; the fold: `a_user_item_from_a_room_says_where_it_came_from` (`context.rs`). Wire: `a_delivery_and_an_extension_reach_the_kernel_verbatim`.
- agents: `team/seat.rs` (a root seats every role as a child of its own, norms before the system prompt, a definition fills what a role does not declare, a persisted role is reopened not seated twice, a child seats nobody, no file seats nobody), `team/file.rs` (both norms forms, unknown keys, nearest file wins), `names::a_room_resolves_from_the_session_that_holds_it_and_from_a_member`, `message::a_message_to_a_room_is_a_post`, `team/command` table and no-file text. Black-box `cli/agents.rs`: `a_project_s_roles_are_seated_before_the_root_s_first_turn` (a two-response script, neither consumed by a role), `the_same_roles_come_back_on_continue`.
- rooms: `post::everyone_but_the_author_hears_it_and_is_told_where_from`, `a_name_nobody_has_is_skipped_and_the_rest_still_hear_it`, `a_room_is_never_a_delivery_target`, `hook::a_post_nobody_signed_came_from_the_session_the_room_hangs_under`, `a_reopened_room_is_the_same_room_and_keeps_its_members`, `a_project_s_rooms_are_seated_when_a_person_s_session_opens`, `roster::…`, `command::…`. Black-box `tests/rooms.rs`: `a_post_in_a_room_wakes_its_member_and_is_not_fanned_back` (a `Peer` turn whose input is the post, `origin.conversation == "#design"`, the room's journal holds one post), `a_room_member_that_asks_a_person_is_answered_through_the_root`. Membership across a restart: the kernel restates extensions at a segment's head (`a_resumed_session_restates_its_extensions_for_the_observers`).
- tasks: `journal::the_four_tools_share_one_list_and_nothing_else`, `contributor::the_open_tasks_reach_the_prompt` / `a_session_with_no_tasks_adds_nothing_to_the_prompt`, `command::…`; black-box `cli/tasks.rs::the_journal_carries_the_list_from_one_run_to_the_next` (`--continue` reads back what `TaskCreate` wrote; an `Extension{bingo.tasks}` frame is on stdout — what an `Event` hook sees).
- TUI: snapshots `a_room_transcript_reads_as_a_chat`, `ctrl_t_shows_what_the_plugins_wrote_into_the_session`, `a_room_sits_in_the_switcher_with_no_status`, `the_composer_of_a_room_offers_to_post_to_it`; `panel::view_of` unit tests; the live drive above.
- Every surface: a room member is a child, so M8's `a_childs_permission_is_refused_off_a_tty_in_every_output_format` and the TUI's child-dialog snapshots cover print and the terminal; RPC has its own scenario above; `a_child_that_failed_is_an_error_result_for_the_root` covers a member that fails.
- Noun grep clean; sdk touched for the contract once (`59f118b`) and twice more while integrating (`7994ebf` `ContextQuery.host`; `e829b87` `SessionSummary.{system_extra, tools}`), each said in ADR-0011.

Found while integrating (each a commit body too):

- A reopened session announced itself but not its extensions, so the rooms hook's fold of membership came back empty from a restart while the snapshot had everything: the head of a segment restates every extension (`b47fcd2`).
- `bingo-context`'s memory extractor asks the model at the end of every turn that ran a tool, on every session, and took a scripted response for it — every multi-session scenario since M7 lost one response to a call nobody scripted, and M8's live drive needed a "spare" answer for it. A side question now says so (`provider_options.bingo.purpose`) and the fake provider answers it from the script's `side` list or with nothing (`a10f67c`). Worker B's RPC scenario had passed with the member's turn failing on an exhausted script; it no longer does.
- `--continue` (`Latest`) took the most recently created session in the directory, which in a project with a team is a role; it prefers a root (`e829b87`).
- A resumed session lost its `system_extra` and `tools` (a gap since M8): the summary now carries both (`e829b87`).
- A start hook ran beside the session, racing the first turn; awaiting it deadlocked, because seating lists the tree and the host reads every live actor's summary — the one starting included. The actor now answers its summary while its start hooks run and holds everything else (`e829b87`).
- A seated role was offered `AskUserQuestion` against its own note; `ListAgents` and `/agents` listed rooms as idle agents (`e78bc05`).
- Teams needed no crate: a team is resident agents, a module of `bingo-agents` (ADR-0011).

Open, carried forward:

- Memory extraction runs at the end of every tool-using turn of every session, sub-agents and room members included: one side question per such turn. Whether a child's turn should feed the project memory — and at what cost — is a product decision the memory plugin owes an ADR line.
- A room's parent is never a delivery target (members are the room's siblings), so a person's own session is never woken by its room; a member reaches the person through `SendMessage(to: "parent")` as before.
- `names::own` / `hook::is_root` list every session, store included, to find one summary — once per `SendMessage`, once per session start (M8's carried `SessionFilter.id` / `HostApi::summary`). `HookContext` carries no `Env`, so the seat hook keeps the one from registration.
- `bingo-tasks` opens a snapshot per tool call and per prompt round; unmeasured beyond the black-box run's 0.35 s.
- A role's memory is its journal under its root; across new roots it starts over. Extraction into a per-project memory is the M14 experience library's neighbourhood, and its own ADR.
- `TaskCreated`/`TaskCompleted` shell hook events; a per-plugin TUI widget; `#room` as a direct send from the composer.
- `crates/bingo-core/src/turn.rs` 775, `crates/bingo/tests/rpc.rs` 793 and the TUI's `test_support.rs` 741 non-test lines are over the 700 warning; `session.rs` is back at 714 after the split.
- Everything M7 and M8 carried that this milestone did not close (plugin notices, `HookContext.permission_mode`, `CommandSpec.description`, the live provider smokes, `FollowupTask` before `WaitAgent`, a child's default tool set being the host's).
