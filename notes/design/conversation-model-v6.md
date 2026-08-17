# Conversation model v6 — user, main, rooms, and the @ (D117–D123)

Ruled by the user 2026-08-16, implemented 2026-08-17. **This file supersedes
every earlier conversation-model document** — v2, v3, v4, v5 and the
interaction blueprint are deleted with this batch; their history lives in
`notes/research.md` (D76–D116) and in git. Where this file and any older
record disagree, this one wins.

## The verdict on v5

The view layer's inbox turn held: the flow is a whitelist, containers carry
badges, the user pulls. Two things did not survive contact with the user:

1. **The zoom was still a second place.** An agent behind an alt-screen modal
   with its own renderer is not "exactly like main" — and the user's ruling
   is that it must be: *main is just a slightly specialized agent*.
2. **The social layer still woke on every post.** v5 proudly left delivery
   untouched ("perception is not presentation", law 5) — which meant one
   "Hi" into a crew room was still N model calls, and main was relayed every
   room line unconditionally. **Law 5 is explicitly reversed** for the room
   feed: the user ruled main a member under the same rules as everyone else.

## The model

Three parties, one grammar:

- **user** — the human, a first-class identity in every conversation
  (`[DM from user]`, `@user`, posts as `user` in rooms).
- **main** — the agent the terminal starts on. Its privileges are the host's
  (slash commands, `!`, the queue, rewind, the digest); as a *conversation*
  it is one page among pages, and as a *room member* it is gated like any
  other.
- **rooms** — the only group primitive (D95 stands). Members are any subset
  of the team; the user joins by posting or `/join`.

## The laws

### Delivery is not waking (D117)

Every room line still lands in every member's inbox, in total order, under
the same budgets and serial checks. *Waking* is earned:

- `@name` (or `@all`) wakes the named member **now** — idle members start a
  run on the spot, running members absorb the mention (and every queued room
  line before it, in order) at their next tool boundary.
- Unnamed lines wait: a member wakes to them in bulk (`ROOM_UNREAD_WAKE` = 5
  unread) or on age (`ROOM_UNREAD_MAX_AGE` = 120s, enforced by one
  per-session 15s sweep).
- **No polling** (the user's ruling, verbatim): the age clock is born with
  the first unread line and dies with the drain — an empty inbox never
  wakes, and a quiet room costs no model calls.
- The gate is one predicate (`inbox_wakes`) consulted by all three doors:
  the flush, the finish-continuation, and the mid-turn take.
- Misfired mentions come back to the sender: a token that resolves to
  nobody, or names a stopped member (D105a's one revival door stands), is
  named in the tool result.

### Main is a member (D118)

The unconditional room→main relay retired. Unnamed room lines wait in a
per-room **pen** and enter `main_mail` on a mention of main (`@main`/`@all`
— released in room order, ahead of the naming line), in bulk at 5, or at
the 120s age pump (frame clock + main's own turn boundary). The 2s/15s
digest debounce shapes delivery of what the gate has released. DM mail was
never penned; the frozen-budget `⚠` was never a relay and never waits.

### The @ decides what you owe (D119, superseding D112's who-spoke rule)

- A line that names you needs you now: act on it or answer it, in the room.
- `@all` is owed one *covered* answer (a colleague already answering covers
  you) — the anti-chorus clause survives on the one broadcast form left.
- A line that names nobody is FYI, whoever wrote it. Waking on a batch you
  have nothing to add to ends in silence; the one exception is a question
  the batch shows still unanswered — the user's especially.
- Senders are bound too: `@` what needs someone now, leave FYI unnamed,
  `@all` is a fire alarm. `@user` only when the human must look.
- Main carries its own half (`MAIN_CHANNEL_NOTE`): answer a room in the
  room, and **keep the user posted on what their rooms are doing** — D123
  reversed D119's narration ban on the user's ruling ("main 应该向我转述
  房间内的情况"). Main is the user's eyes on the team; the flood guard
  D119 feared is the pen itself (a batch reaches main at most once per
  mention/5-lines/120s release), so the surviving discipline is form, not
  silence: a briefing in main's own words, compressed, attributed, with
  the verbatim record staying on the room's page.

### A page is main's page (D120)

Entering any conversation turns the screen to it — same `UiMessage` →
`Block` → `Doc` pipeline, same collapse machinery, same avatar gutter, same
write-once flushing into the terminal's own scrollback. Switching is a page
turn (`term::page_break`, D98's primitive revived): the page you leave banks
into scrollback, the next starts at the top, coming home reprints a recent
tail (the resize machinery, D27's accepted duplicate). Typing on a page is
prose to its subject as the user — `/` and `!` are messages, a stopped
instance resumes, the domain's pending record is the echo. Esc stops the
page's run first and comes home second; main's own turn is out of Esc's
reach while a page is up (`Ctrl+C` overrides). `shift+tab` cycles the viewed
agent's mode. **The room page is speech only**: what members sent to the
room, nothing else — membership changes stay in the log.

### The roster (D121)

Conversations line up under the composer — the user's own CC screenshot:
`● main` first, then agents, then member rooms, three-row window, badges in
constant view (unread dot, mention `•N` accent, waiting-on-you). The one
door is `↓` at the end of prompt history (CC's fallthrough — no chord, no
new key); `Enter` opens, `k` stops, `↑`/`Esc`/typing return to the draft.

### Mentions of the user

`@user` has **no delivery semantics** — the user cannot be woken, only
called: the room's badge takes the accent count and rings once per mention
turn-on (re-armed by reading; silent for the room being looked at). D116's
`⚑` flow line retired on the user's ruling — the badge in constant view is
the message. `SendMessage`'s address space still has no `to: "user"`.

## Byte contracts (unchanged, verbatim)

`[DM from user]` · `[message from @X]` · `[#ch msg #N] from: text` ·
`<messages>` envelope (+ pre-D98 `<channel-messages>` compat) ·
`[follow-up instruction]` vs `[follow-up N/3]` · unmarked main Direct ·
`STEER_MARKER` · summary rides `main_arrivals` only · permission reason
`{instance} · ` prefix. One parser (`buffer::line_source`), one walk
(`perspective::walk`). v6 added **no** model-facing markers.

## Global rules for every batch (migrated from the interaction blueprint)

> - Append the decision record to `notes/research.md` as `### Dnn. <title>`
>   (English, problem → decision → consequences). Check `git log` and
>   research.md for the latest number before claiming one.
> - If the batch touches user-visible feedback states, update
>   `notes/design/feedback-states.md` and its changelog; if it touches
>   config keys / slash commands / capabilities, sync
>   `src/skills/bundled/guide.md` (AGENTS.md's same-batch rule).
> - Design language is Claude Code's: chrome copy in English, CC wording
>   verbatim where a CC equivalent exists.
> - Never `git add -A`; stage files explicitly. Commit trailers (both):
>   `Co-authored-by: bingo <id+bingo@users.noreply.github.com>`
>   `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`
> - Gates before commit: `cargo fmt --all -- --check` ·
>   `cargo check --locked --all-targets` ·
>   `cargo clippy --locked --all-targets -- -D warnings` ·
>   `cargo test --locked --all-targets` · `scripts/check_discipline.sh`.
> - Do not "fix" pre-existing unrelated CI debt inside a batch.

## Batches

| Batch | Landed | Scope |
|---|---|---|
| prep | eb1410b | address grammar out of agent.rs; `names` into channels |
| D117 | be5774b | the wake gate: mentions, bulk, age, sweeper |
| D118 | a8edd12 | main's pen: the unconditional relay retires |
| D119 | 7ee9947 | the @ doctrine: CHANNEL_NOTE rewrite, MAIN_CHANNEL_NOTE |
| D120 | 52c7c15 + 1fe00ba | page_break; pages replace the zoom |
| D121 | 7af1356 | the roster replaces the tree, the pills and the `⚑` |
| D122 | 452e3df | this file; v2–v5 + blueprint deleted; docs synced |
| D123 | this batch | the briefing duty: D119's narration ban reversed |

## Not built, deliberately

- A mention does **not** revive a stopped member — D105a's one door (a
  direct message) stands; the sender is told instead.
- `@all` has no rate limiter: the 50/500 room budgets and the fire-alarm
  prompt rule govern it.
- member↔member DMs still do not exist — coordination is the room, with
  the `@`.
- Members' serial `seen` semantics are unchanged; a member woken late and
  posting stale is still bounced with the increments (drop by default,
  D112's surviving half).
- The `•N` badge grammar is bingo's own, beyond CC (whose pills carry no
  counts): kept deliberately — rooms exist here and CC has none, so words
  at *you* need a number CC never had to print.
- History-derived pages show folded calls without per-call durations or
  outputs (the record never had them — D99's limit, unchanged); the live
  tail renders flat rows (the registry publishes strings).
