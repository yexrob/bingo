# Conversation model v2 — implementation program (D94–D97)

Approved by the user 2026-08-15 after the first real-device smoke round.
Design source: the "bingo 会话模型 v2" design page (session artifact); this file
is the repo-side authority for the implementing agents. Base: dev@8464999
(D76–D93 landed). Branch: `feat/conv-v2`.

## Global rules (binding for every batch)

Identical to the D76–D92 program (notes/design/interaction-blueprint.md,
"Global rules"): the four gates (`cargo fmt --all -- --check`; `cargo check
--locked --all-targets`; `cargo clippy --locked --all-targets -- -D warnings`;
`cargo test --locked --all-targets`) plus `scripts/check_discipline.sh` before
every commit; English UI copy/comments/docs; a `### D9x.` record in
notes/research.md per batch; feedback-states.md + guide.md + READMEs synced
when user-visible behavior changes; Conventional Commits with both trailers
(`Co-authored-by: bingo <id+bingo@users.noreply.github.com>` and
`Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`); explicit staging
(never `git add -A`); no pushes, no merges; no recursive deletes of
directories the batch did not create; pid-tagged temp dirs in tests; no new
dependencies without dispatch-level approval.

## The model

Five entities:

1. **hub** — the user's assistant (the main agent). A 1v1 conversation, not a
   message bus. Besides the user↔main dialogue it carries ONLY `notify_user`
   relay lines.
2. **team** — the organization, NOT a conversation: a read-only directory
   (roster) view listing members with presence, each member's rooms, the room
   list with memberships, and a recent member-lifecycle feed (spawn/done).
   Entry: the ctrl+t cycle (tasks → teammates). Not in the conversation bar.
3. **#room** — the only group-chat primitive. Members are an arbitrary subset
   of the team; a room need not include the user (agents may form rooms among
   themselves). Rooms show dialogue plus dim room-membership system lines
   only — never reminders, thinking, or tool calls.
4. **@agent DM** — the pair conversation user↔agent. Full process density.
   Agent↔agent DMs exist at the domain level but never render inside the
   user's pair view.
5. **Perspective page** — a read-only, per-agent communications dossier:
   a grouped index (DMs by counterpart · rooms · intake · merged timeline),
   Enter opens one thread read-only, Esc walks back. Entry: ctrl+b detail →
   tab.

**Protagonist rule** (display density): every 1v1 view has one executing
protagonist whose process detail (thinking / tool calls / diffs) is shown —
hub's protagonist is main; the user↔X DM's is X; every thread on X's
perspective page has X as protagonist regardless of counterpart. Rooms are
multi-party: speech only, no process.

**Privacy stance**: presentation layer respects pair boundaries (the user's
@X view never mixes in X's other DMs); the audit layer is all-seeing (the
perspective page + on-disk transcripts). Rooms the user is not in are visible
in the directory, can be opened read-only (observer framing), and joining to
speak is a membership event every member sees — no silent lurking.

**Delivery matrix** (UI routing only — the MODEL-side contracts, task
notifications to the main agent, D64 markers, D63 privacy, are untouched):

| Message | Goes to |
|---|---|
| agent spawn/done/ack lifecycle | team directory feed (+ bar presence); NOT the hub flow |
| room membership change | that room's dim system line |
| agent working process | its DM / perspective threads |
| agent reports & results | its DM (+ unread); user-worthy items via notify_user |
| agent → user deliberate notice | `notify_user` tool → hub relay line + D79 attention |
| user → agent | DM or @-routing (unchanged) |
| agent ↔ agent DM | both perspective pages; never hub, never the user's pair views |

**Resolved forks**: room access = directory-visible + read-only observe +
join-to-speak (with system line). Perspective entry = ctrl+b detail tab.
`notify_user` levels = `info` (hub line + unread only) and `urgent` (also
fires the D79 notifier). Avatars return as a row-builder gutter in DM / room /
perspective views (kitty image, chip fallback); the hub stays avatar-free.
The conversation bar moves to the bottom row (below the composer) and lists
only conversations the user is in: hub · their #rooms · @DMs. Content images
(pasted / tool-produced / agent-sent) register in a session-level newest-first
registry and can be opened in the system viewer (fullscreen: click; inline:
`/images` picker; transcript view: `o`); avatars are chrome, never registered.

## Batches

- **D94 — delivery rerouting + notify_user.** Stop lifecycle/report flooding
  in the hub flow (bar presence + unread + the bounded team log carry the
  signal until D95 renders the directory); add the `notify_user(text, level)`
  tool for subagents with per-agent rate limiting (1/min, excess coalesced
  into a "N more in @name" line); hub relay line rendering + D79 hookup for
  `urgent`. Model-side contracts unchanged.
- **D95 — rooms as first-class + the team directory.** Arbitrary-subset
  membership incl. agent-created user-less rooms; join/leave system lines;
  read-only observer mode from the directory; the ctrl+t directory view
  (roster · rooms · recent feed); the #team board buffer retires.
- **D96 — the perspective page.** Two-level read-only projection over agent
  transcripts + deliver records: grouped index with counts and last-activity,
  thread views with the agent as protagonist (full process rows), merged
  timeline; ctrl+b detail tab entry.
- **D97 — presentation batch.** Avatar gutter in DM/room/perspective; bar to
  the bottom row (in-conversations only); the image registry + open flows
  (click / `/images` / `o`), detached system-viewer spawn, bounded temp files.

Detailed specifications are carried by each batch's dispatch prompt, as in the
D76–D92 program.
