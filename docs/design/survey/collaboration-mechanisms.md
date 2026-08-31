# Collaboration mechanisms of the old bingo (survey, 2026-08-31)

> Source: two subagent deep-dives (task system; rooms/teams/messaging), condensed here with
> citations preserved. Paths are relative to the old repo (`../bingo/src/…`). Facts verified on
> the date in the title. Complements `collab-protocol-health.md` (2026-08-28), which sized the
> layer (~27K lines, 18% of the crate) and judged it premature — "a product to build on a
> stable base". M0–M15 built that base; this survey is the mechanism-level input for building
> the product.

## What this changes for the rewrite

Where we already match or beat the original:

- **One verb, one address grammar** — `SendMessage(to: "@agent" | "#room")`, same grammar in
  the user's composer. The original's single best decision; we already have it.
- **A real session tree.** The original has a flat global registry with depth *gates* (cap 3,
  rooms depth-1-only, no parent pointer) — its own verdict: "the worst of both". Our tree with
  parent links is the coherent version; keep scoped visibility and cascading stop on it.
- **`WaitAgent`.** The original has no join/gather at all — fan-in is "N messages, each with a
  300s watchdog". Its report calls this the top missing primitive. We shipped it in M11.
- **Async by default.** The original's `Agent` tool defaults `background: true`. Consistent
  with ADR-0018's policy.

What to adopt when we design the collaboration milestone:

- **Separate "what reaches you" from "what you owe."** Delivery unconditional and free;
  obligation explicit, tracked, chased, displayed (`Mention`, `Ack`). Every design in the old
  codebase that holds up follows this line; both of its documented production failures
  (D124, D129) came from conflating the two.
- **The ack ledger's honesty**: `Queued / Delivered{run} / Answered{run} / Dropped` — read
  into a prompt is not answered. One ledger, one chaser (the original grew three).
- **Wake duality**: idle receiver → atomic claim + drain + new turn; running receiver →
  inject at the next tool-round boundary (input-token cost, zero extra calls); main → digest
  debounce (2s quiet / 15s deadline, urgent bypasses).
- **Room creation for named agents** — the original gives depth-1 named agents the create
  tool ("a team that can only be grouped from the top is not a team that can organize
  itself"), seats *only the caller*, everyone else named explicitly, user included.
- **Rooms as explicit rosters**, not "the parent's children": membership names members; the
  `@` marks obligation, not routing. Our current sibling fan-out under `room.parent` should be
  re-examined against this when rooms grow agent-facing creation.
- **Blueprint vs construction site** for teams: persistent roster in a committed file
  referencing agent definitions; runtime instances and rooms are the ephemeral half.
  Idempotent start (`refresh` = new prompt, same history); spawn ≠ wake (members idle at
  zero tokens); bypass-immune confirmation on roster changes, including the write-the-file
  back door.
- **Tasks**: one file per task, tmp+rename, loud parse errors; owner bound to an agent *run*
  whose terminal state propagates (`Failed`/`Abandoned`, never silently `Completed`); write
  attribution; lifecycle hooks with veto-and-rollback. Panel: auto-open on create, self-close
  on all-done with a one-line receipt; manual open never self-closes; blocked dims the whole
  row over any status colour; owner shown only while the owner is live on the roster.

What to refuse:

- A second representation anywhere: the original shipped a dead protocol task model
  (4 states, prefixed ids, `task/changed` never emitted) beside the live one (3 states,
  integer ids) — the exact debt our "one fact, one representation" rule forbids.
- Vocabulary splits (`channel` in the domain, `room` in every string — five shapes for one
  noun) and noun collisions ("task" = todo, background work, and prose assignment).
- Prompt-as-mechanism: ~2K tokens of etiquette per subagent, rewritten six times, for rules
  a post-time check could enforce in a line.
- Stored-but-never-rendered fields (`activeForm`), hidden half-features (`metadata._internal`
  with no producer, owner/blocks in the schema but stripped from the prompt), self-nag
  reminder injections with concealment orders.

---

## 1. Tasks

### Data model and ownership

- `Task` (`tasks.rs:33-51`): `id, subject, description, active_form?, status, owner?,
  blocks[], blocked_by[], metadata{}`. No timestamp, no author, no session id; `owner` is a
  free-form *name* string.
- `TaskStatus` = `Pending | InProgress | Completed` (`tasks.rs:11-19`). No Failed/Cancelled;
  "deleted" is file removal at the tool layer; "blocked" is derived from `blocked_by`.
- Ids: decimal strings, `max+1` under a process mutex (`tasks.rs:170-188`), per list; path
  guard rejects non-`[A-Za-z0-9_-]` ids (path traversal, tested).
- Persistence: one JSON file per task, `data_dir/tasks/<list_id>/<id>.json`; write =
  temp + rename, temp suffix chosen so directory scans never see it (`tasks.rs:154-167`);
  parse failures hard-error — "tasks silently vanished" is the recorded lesson
  (`tasks.rs:146-152`). Strict async `list()` for the model, lenient sync `list_ui()` for the
  renderer (`tasks.rs:319-348`).
- `list_id` = transcript stem → tasks are per *session*, rebound on `/clear`, moved on
  `/rename`; `BINGO_TASK_LIST_ID` pins.
- The store hangs off `Session`, and sub-agents share the parent's store by `Arc` clone
  (`tool/agent.rs:1255`): any depth can update or delete any task, with no record of who.
- A dead second model exists in the protocol layer: `TaskResource` with a fourth state
  `Cancelled`, prefixed `TaskId`, `task/changed`/`task/removed` notifications, a revision
  scope — none of it ever populated or emitted (`app/controller.rs:1659,1751,836`). `/tasks`
  works in the TUI (reads disk directly) and returns empty in every other frontend.

### Tools

Four, at every depth (`tools.rs:54-57`): `TaskCreate {subject, description, activeForm?,
metadata?}` (always Pending, owner cannot be set at creation), `TaskUpdate {taskId, …,
status: pending|in_progress|completed|deleted, addBlocks?, addBlockedBy?, owner?}`,
`TaskGet`, `TaskList` (returns only unresolved blockers). A coercion layer
(`tool/task.rs:17-78,225-250`) accepts `{task:{…}}` wrappers and `title`/`content`/`task_id`/
`state` aliases — worth keeping, but log what was coerced (the original discards that list).
`TaskCreate` is not concurrency-safe (read-max-then-write id allocation). Plan mode exempts
tasks by *name prefix* `Task` (`permission.rs:392-398`).

### Lifecycle and notification

- Only models write tasks; no slash command or engine path mutates them.
- Task state and background-work state are disjoint systems: `watch.rs` has
  `Running|Idle|Done|Failed|Cancelled` and a notification queue; the task store never
  observes it. A crashed agent leaves its task `in_progress` forever.
- Three real signals: `TaskCreated`/`TaskCompleted` hooks with exit-2 veto — a vetoed create
  is rolled back, a vetoed completion refused (`tool/task.rs:181-193,414-435`); a
  watch-channel "expand" pulse that auto-opens the TUI panel on create only; and a self-nag
  reminder injected every 10 quiet turns with an instruction to hide itself
  (`query.rs:170-200`) — a smell standing in for tasks being load-bearing.

### Display

- Task tool calls are hidden from the transcript; the panel is the display
  (`tui/chat.rs:91-103`).
- Panel above the composer: pulse glyph iff something is in progress; `todo · d/t tasks`;
  `… N done` fold (3 shown); active rows (5 shown) with blocked-dims-everything >
  in-progress-accent > pending-neutral; ` (@name)` only if the owner is a live non-stopped
  roster instance (`tui/chat_tail.rs:1954-1968`); ` › blocked by #3` filtered to unresolved.
- Two booleans, `visible × auto`: auto-opened panels self-close when everything is done and
  leave `✓ N/N tasks done · ctrl+t to view`; manually opened ones never self-close
  (`tui/chat_tail.rs:1854-1867`).
- Freshness by directory re-read on a tick, skipped while hidden, diffed before repaint —
  a mitigation for the change stream that was never emitted.
- `activeForm` is documented as the spinner verb and read by nothing.

---

## 2. Messaging between agents

- One speech tool, `SendMessage {to, message, summary?, ack_timeout? (default 300s),
  urgent?}` (`tool/agent.rs:1544-1575`); `#x` = room, else agent (`@` optional) — the same
  grammar the user's composer parses (`app/submit.rs:110-135`).
- Identity is runtime-stamped: `sender_of()` reads `session.instance`; the model cannot
  state its own name (`tool/address.rs:44-50`).
- Reach is addressing, not tool distribution: any named agent may message any named agent
  (D137 — hub routing "is the shape a human org uses for authority, not for talking"); the
  only refusals are self-address, no-name senders, and non-member room posts. `urgent` is
  subagent→main only and rings the bell.
- Three lanes: direct (inbox + ack minted, "sending is answering" settles the reverse debt);
  to main (pre-formatted `main_mail`, no ack — main answers the *user*); room fan-out (§3).
- Ack ledger `Queued|Delivered{run}|Answered{run}|Dropped{reason}` (`agents.rs:263-290`) —
  "two of these look like success and are not". Watchdog sleeps the timeout, chases with
  `FollowUp` up to 3 rounds; the watch row is registered on first miss, so a row existing
  means chasing happened.
- Wake: idle → `flush_pending` atomically flips Idle→Running and drains (no double-start);
  running → every deposit pulses a `watch` generation, drained at the top of the next tool
  round and injected as a user message (`query.rs:930-940`); stopped → broadcasts skip,
  a direct message revives. Main's inbox is debounced 2s quiet / 15s deadline into one
  digest (`app/mail.rs`).
- No gather/join exists; `background: false` blocks the caller's whole turn. The report's
  top structural gap.

---

## 3. Rooms

- A room *is* the domain's "channel" (`channels.rs:1-31` admits the split). Engine holds four
  primitives — member list, serial/free commit check, wake-on-delivery, sender stamping +
  budget — "everything else is prompting".
- Membership is an explicit roster; `main`/`user`/`all` reserved; the user is an ordinary
  member (D95), auto-joined by speaking (controller-side, so every client joins by one door).
- Creation: agent tool `Channel {action: create|invite|kick|list, channel, members, mode}` —
  available to main *and depth-1 named agents* ("a team that can only be grouped from the
  top is not a team that can organize itself", `tools.rs:85-99`); create seats **only the
  caller** — everyone else, user included, must be named. Members must be main, user, or
  depth-exactly-1 (`tool/channel.rs:405-420`). Also created from team blueprints; user side
  has `/join`, `/leave`. Whole feature behind `experimental.agentChannels`.
- Fan-out (`channels.rs:1221-1360`): membership check → frozen check → serial staleness
  bounce (missed increments returned *as a tool result*, counting as read — optimistic
  locking with the model as resolver; default advice: drop) → per-agent budget (50) → total
  cap (500, freeze + warn main) → append → settle-then-open mentions (answering-and-asking
  in one post doesn't settle its own question) → deliver to every member except sender,
  main, user → unknown `@`s bounced to the sender same turn.
- **The `@` is an obligation marker, not a filter**: every message reaches every member;
  the `@` decides who owes an answer. `Mention {seq, from, to, answered?}` is a first-class
  debt, closed by the named member's next post ("speaking is the answer"); `@all` is one
  debt against the room; mention watchdog 300s × 3. Mentions are re-derived from the log on
  replay — one authority.
- Persistence: append-only JSONL sidecar (`app/roomlog.rs`), replay is a fold; membership
  lines are sequenced but never delivered and never make anyone stale.
- Verdict flags: serial-by-default probably a misfeature at scale (busy rooms bounce
  constantly, burning tokens to discard work); the channel/room vocabulary split metastasized
  into five shapes for one noun; ~2K tokens of room etiquette per member enforces rules a
  post-time check could.

---

## 4. Teams

- "The team is the blueprint (`.bingo/team.json`, committed), the room is the construction
  site (runtime instances + channels)" (`team.rs:1-22`). `TeamMember {name, agent, avatar?,
  model?, provider?}` references `.bingo/agents/<name>.md` — one source of truth per persona;
  model pinned in the file because "which member is on which model is a property of the
  formation".
- Rooms resolver: no `channels` → one room named after the team holding everyone; declared
  `channels` → exactly those ("a room nobody asked for is one nobody reads").
- Org chart: recursive `TeamRef {path}` across directories, pre-order load so a subtree is a
  contiguous slice; scope rule "a room reaches its own subtree, never a parent or sibling";
  names unique tree-wide (what makes bare-name addressing work); depth cap 8; validate-all-
  up-front — a bad reference anywhere spawns nothing.
- Start = spawn idempotently (`team.rs:1116-1255`): existing instance → `refresh` (new
  prompt, same history — D69); fresh → insert as Crew then immediately idle (**spawn ≠
  wake** — standing by costs zero tokens); per-member failures, not fatal; renames tracked so
  a renamed member still joins its rooms.
- Memory as a pointer (D51): per project-hash+branch+team+member directory, referenced by a
  ~40-token system block saying where the past is, with "do not read speculatively".
  Append-only decisions log written by `/team assign` and every hire.
- `Team` tool is main-only with bypass-immune confirmation on start/stop/save, and the
  `Write .bingo/team.json` back door asks the same question in every permission mode
  (`permission.rs:309-315`).
- Hires vs crew: model-spawned instances are `Hire`, leased for 2 sweeps and released with a
  notification; crew is never swept. The lease-by-sweep-count is flagged as a GC heuristic
  with a load-bearing off-by-one — prefer explicit release on acked completion.

---

## 5. Display information architecture (for reference, not porting)

- Roster: flat rows under the composer (`@main` first, agents, then `#rooms`), entered with
  `↓`; per row a state dot, then **debt before state** ("the debt is what the user is looking
  for and the state is what explains it"); badge `•` activity vs `•N` mention.
- Background dialog sections Agents / Shells / Rooms; the room chip marks rooms you are
  *not* in (a tick on every joined room "would be a column of ticks saying nothing").
- Room pages are projections of the log ("a room is a log, not a turn loop"); main relays
  room traffic to the user under a four-setting policy block (verbatim when named or
  deciding; one line for state changes; silence for progress and FYI).
- `/team status` is the only tree-shaped view: indented nodes, per-member state or
  `○ offline`.
