# ADR-0032 — Hooks cross the bridge

Status: accepted · 2026-09-01 · Plan: M29

## Context

ADR-0030 kept the authority plane in-process: implementations and
data cross, verdicts do not. The user redrew that line for hooks on
seeing what a hook is — not the machinery but a handler registered at
the kernel's points — and the verdict worry dissolves on two facts.
First, `HookOutcome` has no `Allow`: a hook can `Continue`, `Deny`,
`Ask`, `Block` or `Redirect`, so an external hook can only ever
tighten what happens, never widen it. Second, hooks-shell already
hands this exact surface to arbitrary user-configured shell commands
on Claude Code's contract; a plugin process is the same trust —
user-installed code — on a better channel: long-lived instead of a
fork per event, a typed schema instead of a stdin convention. Opening
the bridge here repays a debt rather than granting anything new.

## Decision

1. **The sdk `Hook` trait crosses in ADR-0030's shape**: one bridge
   proxy, zero new traits. A plugin declares its hooks at handshake —
   id and matcher (points, tool-name regex) — so the kernel's cheap
   skip stands; nothing crosses per event that the matcher rules out.
2. **Decision points** (`on_submit`, `before_tool`, `after_tool`,
   `on_stop`) cross as requests: the event goes over, the response
   carries the outcome and, where the trait mutates (`on_submit`'s
   input, `before_tool`'s call), the possibly-rewritten value, which
   the host applies. Bridge hooks compose with in-process hooks in
   registration order, exactly as two in-process hooks do.
3. **Observation points** (`on_turn`, `on_compact`, `on_session`,
   `on_event`) cross as notifications: nothing is awaited, so
   watching costs the turn nothing.
4. **Tighten-only is the type's own law.** `HookOutcome` has no
   `Allow` and gains none for the wire. Policy stays in-process —
   `Deny` and `Ask` give an external process every tightening it
   could want, and an external loosening is exactly what this design
   exists to refuse.
5. **Timeouts keep hooks-shell's precedent**: a hook that errors or
   misses its deadline never gets to decide — the host continues with
   a notice naming it. The constants live with ADR-0030's.

## Consequences

- The checkpoint shape completes: an external plugin snapshots on its
  own `before_tool`, with no hooks-shell composition seam.
- A slow external hook on a decision point taxes every matched event;
  the matcher and the deadline are the protections, and the words say
  so.
- hooks-shell remains the right home for a person's one-liners; a
  plugin that outgrows it moves to the bridge without changing what
  it may do.
- ADR-0030's context sentence ("the authority plane stays
  in-process") narrows to policies; its non-goal is amended in place.

Refs: ADR-0015 §4, ADR-0030.
Non-goals: Policy over the wire; an `Allow` outcome; new hook points;
any change to in-process hooks or hooks-shell.
