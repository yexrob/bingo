# M32 — The picker says what a session is

## Goal

A `/resume` row today reads `1. untitled · <timestamp> · <id>` — a
number, not a conversation. After this milestone a row says what the
session is about (its first ask, as its title) and how much happened
in it (a message count), and the raw id leaves the card. The two
facts land where ADR-0005 §5 already keeps derived facts — the
summary — and the journal does not grow a frame per message.

## Bricks, in build order

1. **sdk: the count and its one rule.**
   `SessionSummary.messages: Option<u64>` (`serde` default `None`;
   `None` means "never counted" — an old summary file must show
   nothing rather than a lying `0`). One pure rule, written once
   beside `ItemBody`, says which items a person reads as messages:
   their own prose and the model's answers; a tool call, a receipt,
   a notice are not messages. The fold (`SessionState::apply`)
   maintains `summary.messages` as such items land, so every client
   and the actor hold the same fresh count. Both schemas regenerate
   (`schema/rpc.json`, `schema/plugin.json`); an added optional
   field bumps no `PROTOCOL`.
2. **core: an untitled session takes its first ask as its name.**
   One mint rule: a session whose `title` is `None` and whose fold
   holds a user line takes that line's first sentence — truncated at
   a char boundary, ~48 chars — as its title, published on the
   `SessionUpdated` heads the actor already sends (open) and once
   when the first prose lands live. A session anyone named — an
   agent's seat, a `#room` — is never renamed; the mint fires at
   most once.
3. **store: fresh at rest, no new frames.** `summary::write` on
   `SessionUpdated` stays; appending a message-shaped item also
   updates the count in `summary.json` (a small read-modify-write
   beside the append — the file, not the journal, absorbs
   freshness). `rebuild` recounts from the whole journal with the
   same sdk rule, so a deleted or torn summary comes back true.
4. **TUI: the row.** `N. <title or untitled> · <n> msgs · <relative
   time>` — count shown only when `Some`, time as a person reads it
   (`2h ago`; a small helper if none exists), the raw id gone
   (`/resume <id>` still takes one for hands). Snapshot pins the
   row; the stored rows of the switcher inherit the better titles
   for free.
5. **Black-box.** A run with two exchanges, then `--continue`: the
   picker (or `summary.json` directly) carries the first ask as the
   title and the count; a summary file written before this milestone
   shows no count and no lie; the child/room titles are untouched.

## Files

`crates/bingo-sdk/src/{event,state,item?}.rs`,
`crates/bingo-core/src/session.rs` (the two publish sites),
`crates/bingo-store-jsonl/src/{lib,summary}.rs`,
`crates/bingo-surface-tui/src/view.rs` (+ a time helper),
`schema/{rpc,plugin}.json`, tests beside each. No new dependencies;
budget unchanged.

## Exit criteria

- [ ] a resumed picker row reads title · count · relative time, and
      the title is the session's first ask, truncated safely
- [ ] the count is maintained by the one fold rule, survives a
      summary rebuild, and an old summary shows no count rather
      than `0`
- [ ] a named session (agent seat, room) is never renamed; the mint
      fires once and only where `title` was `None`
- [ ] no journal grows a frame per message; both schemas
      regenerated; every gate green (fmt, check, clippy, test,
      discipline, budget unchanged, deny)

## Non-goals

Model-written summaries as titles; a `/title` command; backfilling
old summary files wholesale (an old session earns its fields the
next time its journal is written or its summary rebuilt — say so in
Carried, do not sweep the store); message counts in the switcher
rows; any new `SessionUpdated` cadence beyond the one first-prose
mint.

## Risks

R-bloat — the journal must not pay for freshness: no per-item
`SessionUpdated`; the store's file absorbs it. R-rule — the message
rule lives in ONE sdk fn used by the fold, the store's append and
the rebuild; a second counting site is the ADR-0011 debt. R-mint —
the mint must lose to every explicit name, past and future; pin
with a test where a title arrives after the first prose. R-truncate
— CJK first lines must cut at a char boundary, pinned. R-stale —
`Option` honesty: `None` renders as nothing, `Some(0)` as `0`.
