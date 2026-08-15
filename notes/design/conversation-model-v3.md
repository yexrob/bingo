# Conversation model v3 — the console, the pair, and the record (D98–D102)

Agreed with the user 2026-08-16 after a three-round design discussion in
session. This file is the repo-side authority for the implementing agents.
Base: dev@26bf0f0 (D94–D97 landed). Where this file and
notes/design/conversation-model-v2.md disagree, this one wins; v2 remains the
record of what was built.

## Global rules (binding for every batch)

Identical to the D76–D97 programs (notes/design/interaction-blueprint.md,
"Global rules"): the four gates (`cargo fmt --all -- --check`; `cargo check
--locked --all-targets`; `cargo clippy --locked --all-targets -- -D warnings`;
`cargo test --locked --all-targets`) plus `scripts/check_discipline.sh` before
every commit; English UI copy/comments/docs; a `### D9x.`/`### D10x.` record in
notes/research.md per batch; feedback-states.md + guide.md + READMEs synced
when user-visible behavior changes; Conventional Commits with both trailers
(`Co-authored-by: bingo <id+bingo@users.noreply.github.com>` and
`Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`); explicit staging
(never `git add -A`); no pushes, no merges; no recursive deletes of
directories the batch did not create; pid-tagged temp dirs in tests; no new
dependencies without dispatch-level approval.

## Why v3

Two findings drove it, both verified in code:

1. **The hub is still a bus — in prose form.** D94 removed the lifecycle
   *lines*, but every agent run's terminal state still fires `submit_auto()`
   (chat.rs, the WatchEvent arm), trigger-blind: main wakes, digests and
   narrates into the flow even when the run that finished was the user's own
   DM exchange. Nine kinds of voice can write into the hub flow; the user
   asked for two.
2. **The DM view renders an agent's whole context flat.** `dm_posts`
   interleaves the task prompt, the hub's instructions, room relays and
   reminders with the user's own conversation. The grouped projection that
   answers "who did it talk to, what did it do" exists (D96) but is read-only
   and four keys deep — the right machine behind the wrong door.

## The model

Two kinds of surface, told apart by one visible feature:

> **A surface with a composer is a conversation and can always be spoken
> into. A surface without one is a record.** "Read-only" is a property of
> records, never a mode of a conversation; the observer vocabulary retires
> with it.

**Participants**: the user, main, agents. Main keeps its specialness — it is
the dispatch console, runs the host turn loop, cannot be stopped or removed —
but the specialness belongs to *main the participant* and to the host
machinery, not to a separately named surface.

**hub retires.** Its three historical meanings resolve separately: the bus is
dead (delivery matrix below); the pair view is `@main`, the same view type as
any DM; "home" survives as a *property* — @main is the terminal's floor,
always first in the bar, never closable, the segment every excursion returns
to. The renames: bar label `hub` → `@main`; rule `── hub ──` → `── @main ──`;
`HUB_NAME` → `MAIN_NAME` (its value has been `"main"` all along —
channels.rs:37); `BufferId::Hub` narrows its meaning to "the home
conversation" (the variant stays; its mechanics are genuinely different).

### Conversations (composer, in the flow)

- **`@main`** — the user↔main dialogue plus host furniture (route receipts,
  slash info, dialogs). Exactly two speakers. A dispatch renders as main's own
  process row — the async-command presentation: a watch row with live state
  and a one-line result — and nothing else of the agent's life renders here
  (the matrix below). The host machinery (permission asks, rewind, `!` bash,
  slash, steer) lives in this conversation and only this one.
- **`@agent`** — the pure pair: what the user said, what the agent answered,
  and the agent's process *for those turns*, rendered by the same renderer
  @main uses (collapsed activity groups replace today's flat dim Process
  lines). The hub↔agent traffic, room relays, intake and reminders leave this
  view entirely — they live on the observation page.
- **`#room`** — unchanged from D95: speech plus dim membership lines.

**Avatars everywhere**: all conversations wear the D97 gutter, @main included
— main gets a portrait of its own; terminals without image support keep the
existing initial-on-colour chip fallback. One more consumer of the machinery,
no new mechanism.

### Records (no composer, visibly a panel or pager)

- **The team directory** (ctrl+t) — unchanged in substance; main joins the
  roster (it is a participant like the rest, and its row is the door to its
  observation page).
- **The observation page** — D96's dossier, unchanged in substance (lanes by
  counterpart · rooms · intake · timeline, protagonist rule, snapshot), two
  changes at its edges: every agent has one **including main** (main's page
  shows its agent lanes, its notification intake, its process — the
  "main's-eye view" the user asked for in round one), and the door moves
  near: one key from the open conversation (tab), the directory row, and the
  ctrl+b detail it already has.

## Delivery matrix v3

| Event | Goes to |
|---|---|
| agent spawn/done/report (main's dispatches) | main's context (task notification — contract unchanged); on screen only the dispatch row's state and whatever main then says |
| agent failed / crashed | the one direct exception: an alert line in @main + D79 attention — bad news must not depend on main's narration discipline |
| agent deliberate notice | `SendMessage(to: main)` — the one directed-message tool, now assembled for subagents with the target restricted to main: into main's inbox, wakes main (`submit_auto`), the trigger invisible on screen — exactly an async command finishing. An `urgent: true` flag rings D79 mechanically on arrival (the bell is the harness's; the words are still main's). `notify_user` retires entirely, its rate limiter and 🔔 relay rendering with it |
| user↔agent DM turns | the DM only. **No main-side event at all** — no wake, no notification, no trace (D63 alignment). The agent may *choose* to tell main via `SendMessage` when the exchange changes something main is coordinating |
| main's perception of agent state | pull, not push: the status/check tools it already has, plus its own dispatch notifications |
| room posts (`SendMessage(to: "#room")` — `Post` retires) | the room; main-as-member digestion is debounced/batched, never one wake per post |
| main's digest turns | speak (prose in @main) or stay silent — the silence contract |

## The silence contract (the model-layer cut)

A digest turn (no user input) ends either in prose — which renders in @main
as main speaking — or in a silent acknowledgement marker that renders as
nothing: the dispatch row already says done. Prompt contract plus one render
rule. The risk is model discipline; the floor under it is the dispatch row
(always visible), the @main unread badge, and the existing chase machinery.

## Accounting fixes folded in

- **DM unread counts Said posts, not history length.** perspective.rs already
  states the right measure ("process rows are work, not messages"); the bar
  never used it. Mention is reserved for words actually addressed to the
  user, not every history change — today every DM change is a mention, which
  is badge blindness by construction.
- **@main gets a real unread.** Main speaking while the user is in another
  conversation must badge the bar; today only relays do (`note_relay`).
- **Replay budget on switch drops 30 → 8.** The record keeps the whole; the
  flow keeps scrollback.
- **Room mention detection** stops requiring the literal `@user` token.

## Resolved forks

- **One speech tool.** `SendMessage` is the single way any participant
  speaks to any conversation, with one semantics: deliver and wake. Its `to`
  grammar is the conversation namespace the UI already uses — a bare
  instance name (or `@name`) for an agent, `#name` for a room — so the tool
  layer and the interface layer share one address language, the same way the
  user's own composer already routes by conversation. Addressing narrows by
  caller: main → any agent, and any room it is in; subagent → main, and any
  room it is in (hub-and-spoke preserved by addressing, not by a second
  tool). `Post` retires; fan-out, main's debounced digestion, per-agent room
  rate limits and membership checks are the *room's delivery policy* and
  stay in the channels layer untouched. Wakes coalesce through the inbox:
  messages landing while the recipient is mid-turn are drained at the next
  turn boundary, so no separate rate limiter. `urgent: true` (agent→main
  only; meaningless for rooms) rings D79 on arrival — the bell is
  harness-guaranteed, the words are main's. `notify_main` is not built;
  `notify_user` retires entirely (D94's tool, its rate limiter, the 🔔 relay
  rendering). `deliver` being name-keyed already anticipates agent↔agent
  addressing if it is ever opened — an addressing-rule change, no
  projection change. The final verb table: `Agent` dispatches, `SendMessage`
  speaks, `AgentControl`/`Channel` manage.
- user-DM runs: no automatic main-side event; agent-initiated `SendMessage`
  is the door; main's perception is pull-based.
- avatars: kept everywhere and extended to @main; text chips where images
  are unsupported.
- hub: retired as a concept; home survives as a property of @main.
- read-only: a property of records (directory, observation page, transcript);
  no conversation is ever read-only. Rooms keep D95 membership semantics
  (join to speak) — out of scope for this program.

## Batches

- **D98 — the quiet console.** Delivery rerouting: no agent lines render in
  @main (the 🔔 relay rendering retires); failed/crash alert line + D79;
  `SendMessage` becomes the one speech tool (assembled for subagents;
  `to` = agents by caller's addressing rules, or `#room` for membership
  rooms; `urgent` flag rings D79 on arrival); `Post` and `notify_user`
  retire; user-DM runs stop waking main; room-mail digestion debounced.
- **D99 — the pure pair.** The DM view renders the user lane only, through
  the @main renderer (collapsed activity groups); the avatar gutter comes to
  @main with a portrait for main; unread/mention accounting fixes; replay
  budget cut.
- **D100 — the record's doors.** Observation page for every agent including
  main; entry from the open conversation (tab), the directory row, and
  ctrl+b detail; main joins the directory roster.
- **D101 — the rename.** hub → @main everywhere (bar, rules, copy, docs);
  `HUB_NAME` → `MAIN_NAME`; `BufferId::Hub` semantics narrowed;
  feedback-states.md and guide.md sync.
- **D102 — the silence contract.** Prompt contract + acknowledgement marker
  + render rule for digest turns.

Detailed specifications are carried by each batch's dispatch prompt, as in
the D76–D97 programs.
