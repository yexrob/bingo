# ADR-0021 — OpenRoom: agents may open rooms

Status: accepted · 2026-08-31 · Plan: M18

## Context

Rooms ship as `command:room` + `hook:rooms` — no tool. An agent can post into
a room it can name (`SendMessage` to `#room`) but cannot open one: creation
is the parent session's privilege via `/room`. The old bingo gave its
depth-1 named agents the create tool, with the reason in its source: "a team
that can only be grouped from the top is not a team that can organize
itself" (survey: `docs/design/survey/collaboration-mechanisms.md`). Its flat
registry needed a cohort gate; our tree is real, so *placement* is the whole
question — a room fans out to the children of the session it hangs under
(`post.rs`), so where it hangs decides who hears it.

## Decision

One new tool in `bingo-rooms`: **`OpenRoom { name, members?, shared? }`**,
registered for every session; the manifest gains `tool:OpenRoom`.

1. **Default placement: under the caller.** The audience is the caller's own
   children — an orchestrating agent convening its own workers. This is the
   unprivileged case.
2. **`shared: true`: under the caller's parent.** The audience is the caller
   and its siblings — a worker convening its peers. A root session asking
   for `shared` is refused with the reason (its plain rooms already reach
   everyone it has).
3. **One door.** The tool calls the same `seat::seat` the `/room` command
   calls, against the chosen parent: same name validation, same idempotent
   reset of a standing room, same journal extension, same fan-out and
   `/room` listing afterwards. No second mechanism, no new state.
4. **Members are titles**, resolved among the room's siblings at post time,
   exactly as today; naming an absent member is allowed and skipped at
   delivery (`post.rs` already does).
5. **Traits fail closed.** Not read-only, not concurrency-safe, interrupt
   Block (the defaults); the permission card shows the room name, the
   members, and where it will hang — `shared` is an act on the parent's tree
   and the card says so.

## Consequences

- The parent-creates workflow stands unchanged; `/room` stays the person's
  door and lists agent-opened rooms like any other.
- No cohort/depth gate: the tree's structure is the visibility rule. A deep
  agent's `shared` room reaches only its own siblings, never the whole tree.
- Room discovery for agents (a rooms listing tool) is deliberately not
  added; `/agents`-style listing already names siblings, and a room an agent
  should join is named in its brief. Revisit with the collaboration
  milestone if briefs prove insufficient.

Refs: ADR-0011 §1 (a room is a Log session), the collaboration survey.
Non-goals: invite/kick verbs, membership ACLs, cross-tree rooms.
