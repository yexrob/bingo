# ADR-0022 — Mentions: what a room post owes

Status: accepted · 2026-09-01 · Plan: M19

## Context

A room delivers every post to every member (`[from x in #room]`), and nothing
marks obligation: a member that reads a question and stays silent is
invisible. That exact failure is documented in the old bingo's decision log
(D124) and its strongest surviving idea is the cure — separate *what reaches
you* from *what you owe*, and make the owing a tracked, chased, displayed
object (survey: `docs/design/survey/collaboration-mechanisms.md`). The old
tree also shows the debt of doing it wrong: three overlapping watchdogs, and
a mention store beside the log it was derivable from.

## Decision

1. **`@name` in a room post opens a debt against that member; the member's
   next post to the room closes it.** Speaking is the answer — substance is
   deliberately not judged. Parsing is word-boundary and case-insensitive
   against the room's members; a name that is not a member opens nothing.
   `@all` is one debt against the room, closed by any other member's post,
   and never chased member-by-member — the sigil did not pick a member, so
   neither does the chase. (2026-09-03: the roster is asked before the
   sigil — a room that seats a member called `all` spends the word on the
   member, because a real name is never shadowed.)
2. **Derived, never stored.** `mentions(posts) -> Vec<Mention>` is a pure
   fold over the room's own journal — the posts are already there, and so
   are the closures. No second store, nothing to keep in sync, and replay
   after a restart re-derives the same debts from the one authority.
3. **One chaser.** The rooms hook — which already watches every journal —
   arms one bounded timer per open mention: after 300 s unanswered, the
   named member is nudged (`deliver(member, …, Wake)`: "you were asked in
   #design and have not answered", with the post's head), at most three
   times. A nudge is a delivery, not a post: the room stays quiet, and a
   nudge opens no debt of its own. Timers die with the process; the next
   process's hook re-derives the fold and chases anything overdue **once**,
   the schedule plugin's overdue rule.
4. **Displayed where a person looks.** `/room` gains an `owed` column (who
   owes, oldest age). While any mention is open, the hook publishes a
   `View::Table` signal `owed` on the room's parent — the rail card
   appears; `Null` when the last debt closes — the card goes (ADR-0013,
   the jobs-signal pattern). The card names the room and who owes
   (2026-09-02: the clock left it — a signal republished only when a debt
   opens or closes cannot keep an age true), and the debts it is drawn from
   ride beside it in the same payload, so a surface that wants an age says
   one at draw time from `debts[].at`.

## Consequences

- No kernel or sdk change: the fold, the timers and the nudges live in
  `bingo-rooms`; the vocabulary is prose-level, so an IM channel bridged to
  a room inherits `@` semantics without knowing this ADR exists.
- The debt is only as durable as the journal — which is exactly as durable
  as the posts themselves. One fact, one representation.
- Deliberately out: obligations on direct `SendMessage` (rooms first;
  machinery follows evidence), read receipts, substance judgement, and any
  escalation past three nudges — after the third, the debt stays visible in
  `owed` and that is the escalation.

Refs: ADR-0011 §1, ADR-0013 §2, the collaboration surveys.
