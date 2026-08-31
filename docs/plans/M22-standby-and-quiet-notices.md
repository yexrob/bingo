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

- [x] kernel order pinned: held briefing + later wake = one turn, briefing first
- [x] standby spawn: no watcher, seated receipt, foreground refusal
- [x] the relay black-box runs on one kickoff post; an unwoken standby
      member has zero turns
- [x] tool words teach the room pattern and the member side
- [x] subsystem notices render as marked lines; person input renders as
      today; unknown surfaces stay person-loud; `TestBackend` proves all three
- [x] every gate green (fmt, check, clippy, test, discipline, budget
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

## Verified (2026-09-01)

- Worker K merged `d677c5d`: the closed quiet set written down once in
  `transcript.rs` with the lean-loud rule in its comment; the notice
  reuses the tool row's own bricks (same `⏺`, same status colour); `cut`
  promises no key a notice cannot answer; a room's journal reads as a
  chat of marked lines — a taste call accepted on review. Worker L
  merged `0dda5a7`: brick 1 found the kernel matching ADR-0027 §2
  exactly (a Hold to a never-run session waits; a later Wake opens one
  turn absorbing briefing-then-trigger as one four-part model message —
  pinned); the `delivery()` pure brick with the worded foreground
  refusal; no watcher; the relay's teeth proven by reverting `standby`.
- Integrated gates on `0dda5a7`, quiet machine (1-min load 5.7): fmt /
  check / clippy / discipline / budget (302/302) / deny all exit 0. The
  workspace suite's single red was the relay test under full-parallel
  load; 3/3 green solo and the cli suite 121/121 green quiet — recorded
  beside the wall-clock family as load-sensitive, not a race (every
  response past the last deterministic point says the same word).
- The PTY smoke is red on this machine at one scene that predates all of
  this: "a focused block opens whole in the pager" — the sheet sits ~9
  rows short with the transcript still visible above it and `G`'s offset
  applied against a bigger window. Bisect: red at `f631082` (M19,
  before today's work) and at K's branch head; green once at `b910d5d`;
  K ran the full smoke green twice on its branch. Filed as its own
  defect with a dedicated worker; every gate named above is green.

## Carried

- The pager sheet defect, until its worker lands.
- A notice longer than five lines folds with no key to open it — the
  pager does not reach `User` items; a small follow-up if wanted.
- `screens.rs` at 969 lines sits 31 under the fail; the next screen test
  forces the split K sketched (the colour-landing section out).
- The relay test budgets 5 s of tail; under full-parallel load that
  margin flaked once — widen only if it ever flakes quiet.
