# ADR-0027 — Spawn ≠ wake: the standby member

Status: accepted · 2026-09-01 · Plan: M22

## Context

A live test showed the collaboration running hub-shaped: three agents
briefed for a relay in a room each spent their first turn answering the
spawn prompt — the only address a spawn reply has is the parent — and then
sat waiting to be dispatched. The parent had to kick every step. The room
machinery (Wake fan-out, the serial check, mention debts) was never the
problem; the shape of arrival was: **a spawn today is also a wake**, so a
briefing cannot be given without demanding an answer to it.

The old tree had the missing idea and named it: *spawn ≠ wake — members
idle at zero tokens* (survey §4). Its team start seats crew and leaves
them idle; the briefing lives with the role; work arrives as messages.

## Decision

1. **`standby: true` on `SpawnAgent`** (background only): the child is
   minted exactly as today — same key, same title, same definition — but
   the prompt is delivered `Delivery::Hold`. It waits at the head of the
   child's queue; no turn opens; the member idles at zero tokens.
2. **The first wake reads the briefing first.** Whatever later wakes the
   member — a room post's fan-out, a direct message, a nudge — opens the
   turn that absorbs the held briefing ahead of its trigger. The order is
   the kernel's queue order and is pinned by a test, the way the barrier
   order was for ADR-0025.
3. **No watcher on a standby spawn.** A teammate is not a one-shot task:
   nothing wakes the parent when a standby member's turns end. Results
   travel the room or a DM, and the mention debt (ADR-0022) is the
   obligation channel. The receipt says what is true: seated, reads its
   briefing when something wakes it.
4. **The pattern moves into the tools' own words.** `SpawnAgent` teaches
   the room shape — open the room with its members, standby-spawn each
   role, post the kickoff *into the room* so every member wakes at once —
   instead of leaving the model its hub habit. The sub-agent NOTE teaches
   the member side: a room post wakes you; when it falls to you, act and
   post back; when it does not, end your turn without posting.
5. **`standby: true` with `background: false` is a worded refusal** —
   waiting for an agent that will not speak until spoken to is a deadlock
   asked for by name.

## Consequences

- Hub dispatch becomes a choice instead of a default: one kickoff post
  runs a whole relay, and the parent's transcript stays clean of
  per-member choreography — pinned end to end by a black-box scenario.
- A standby member nothing ever wakes costs nothing: no turn, no tokens,
  and it dies with the process like any child. `ListAgents` shows it idle
  like anyone else.
- The briefing is journaled when absorbed, not when held, so a `--continue`
  before the first wake re-derives a queue the kernel already owns; no new
  state is written anywhere.
- The foreground spawn, the background-with-watcher spawn and their tests
  are byte-identical to today; `standby` is a third arm, not a rewrite.
