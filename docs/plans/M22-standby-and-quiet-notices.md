# M22 — Standby members and quiet notices (ADR-0027)

## Goal

Collaboration stops being hub-shaped: a member can be seated silent
(`standby: true`, briefing held, zero tokens until something wakes it),
and one kickoff post runs a relay end to end. And the machinery stops
shouting in the transcript: a subsystem's delivered notice — a finished
job, an agent's reply, a room post — renders as a marked line the way a
tool call does, not as a person-sized message block.

## Bricks, in build order

**Worker L — standby (`bingo-agents`):**

1. **The order, verified first** — a kernel-level test pinning that a
   `Hold` delivered to a session that has never run waits, and that a
   later `Wake` opens one turn absorbing both, briefing first (the
   ADR-0025 barrier-order precedent). If reality differs, the ADR moves,
   not the fiction.
2. **The arm** — `standby: Option<bool>` on `SpawnArgs`: mint as today,
   deliver the prompt `Hold`, leave no watcher, return the seated
   receipt; with `background: false` a worded refusal (ADR-0027 §5).
3. **The words** — `SpawnAgent` description gains the room pattern
   (ADR-0027 §4); the NOTE gains the member side; both pinned by the
   existing description tests.
4. **Black-box** (`tests/cli/peers.rs`) — the relay: a room with three
   standby members, one kickoff post, the count relayed to the target,
   the parent's transcript free of per-member dispatch; a standby member
   nothing wakes has zero turns; `standby` foreground refused worded.

**Worker K — quiet notices (`bingo-surface-tui`):**

5. **The rule** — a `User` item whose `origin.surface` is a subsystem's
   (`bash`, `agent`, `room`, `schedule`) renders as a marked notice line
   in the tool-call style — dot, one summary line, the body indented
   under it — never as a person's message block. Anything else, channel
   conversations included, keeps the person block: fail to the loud side.
6. **The look** — first line = who/what and the outcome (the notice's
   own first line); the rest readable but subordinate; theme tokens, no
   new colours; `TestBackend` coverage for both kinds and for the
   fail-loud default; the PTY smoke stays green.

## Files

L: `crates/bingo-agents/src/{spawn,note}.rs`,
`crates/bingo-core/src/turn/tests.rs` (or wherever the deliver-order
test honestly lives), `crates/bingo/tests/cli/peers.rs`.
K: `crates/bingo-surface-tui/src/` (the block/view lane), its tests.
No shared files between L and K; no new dependencies; budget unchanged.

## Exit criteria

- [ ] kernel order pinned: held briefing + later wake = one turn, briefing first
- [ ] standby spawn: no watcher, seated receipt, foreground refusal
- [ ] the relay black-box runs on one kickoff post; an unwoken standby
      member has zero turns
- [ ] tool words teach the room pattern and the member side
- [ ] subsystem notices render as marked lines; person input renders as
      today; unknown surfaces stay person-loud; `TestBackend` proves all three
- [ ] every gate green (fmt, check, clippy, test, discipline, budget
      unchanged, deny)

## Non-goals

Changing foreground or watched-background spawns; any watcher for
standby members; auto-reporting a standby member's results to the parent;
rendering changes to tool calls or person messages; kernel changes
beyond none (the order test only reads).

## Risks

R-order — brick 1 may falsify the Hold-then-Wake absorption order; then
ADR-0027 §2 is amended to what the kernel does, before the arm is built.
R-idle-forever — a standby member whose room never speaks is a silent
leak of nothing: it holds no process, no tokens; accepted and said in the
receipt. R-notice-detection — surface strings are the contract here;
the set is closed and named in one place so a new subsystem must choose
loud or quiet deliberately.
