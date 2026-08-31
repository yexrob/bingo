# ADR-0023 — The board: a room's task list

Status: accepted · 2026-09-01 · Plan: M19

## Context

Tasks are a session's own list (ADR-0011 §2): the doer's private ledger.
Coordination wants the other thing — a shared, claimable board. The old
bingo had the halves and never joined them: its survey calls the claimable
work queue "the best latent idea in the repo" (a board scales better than
N×N messaging), and its holes are documented — a shared store any depth
could mutate with no attribution, and tasks stranded `in_progress` forever
when their owner crashed.

## Decision

1. **The board IS the room's task list.** No new noun, no new store, no new
   schema: `bingo-tasks`'s journal read/write already address any session,
   and a room is a session. The four tools and `/tasks` gain an optional
   `in: "#name"`; resolution walks the tree the way a post's address does —
   a room that is the caller's child or sibling — so structural
   reachability is the gate, the same one `SendMessage` to `#room` has.
   An unreachable name is a worded error result.
2. **Claiming is runtime-stamped.** `TaskUpdate { id, claim: true }` sets
   `owner` to the *caller's own title*, resolved from the session the call
   came from — the model never states who it is, so a claim cannot be
   forged or fat-fingered onto a teammate. Passing `owner` explicitly stays
   for the orchestrator assigning work.
3. **Staleness is rendered, never written.** No machinery flips a crashed
   owner's tasks: the parent hears every background end (the deliver door)
   and edits the board deliberately. What the mechanism does instead is
   tell the truth at read time — `TaskList` and `/tasks` mark an owner with
   no live session of that title as `owner: reviewer (gone)`. Display
   asserts liveness; storage is never silently mutated. (The old tree's
   lesson worn on the display side: its owner chip refused to render a dead
   agent's name at all.)
4. **The personal list is untouched.** Without `in`, every tool means the
   caller's own session, byte-identical to today; the context contributor
   keeps reciting only the personal list.

## Consequences

- Boards survive `--continue` and land in every surface for free: the list
  is a journal extension, and the whole View lane already draws it.
- Write attribution beyond claiming is deliberately deferred: the list is
  a whole-value republish and `Extension` events carry no principal. The
  journal's successive snapshots are diffable if it is ever wanted; a
  change-log field would be a second representation grown speculatively.
- Two plugins now walk the tree to resolve a `#name`. The duplication is
  accepted this round and recorded for the sdk sweep (a shared resolver,
  or `SessionFilter { id }`).
- Concurrency: the tools stay `concurrency_safe: false`; two writers to
  one board serialize at the caller as they do for a private list. A busy
  board's last-write-wins on the whole list is a known limit, accepted at
  this scale and stated in the tool prompt.

Refs: ADR-0011 §2, ADR-0021 (rooms an agent can open), the surveys.
