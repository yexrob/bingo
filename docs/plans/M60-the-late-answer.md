# M60 — The late answer

## Goal

Reported 2026-09-04, tmux 3.6b under Ghostty 1.3 with
`allow-passthrough on`: bingo starts, the layout jumps once, the
composer reads `> Gi=31;OK>|ghostty 1.3`, and the status line says
`tmux: pictures need \`set -g allow-passthrough on\` and the focused
pane at start` — although the passthrough is on and the answer that
proves it is sitting in the input box.

Diagnosis (from `graphics/mod.rs::listen`, `probe.rs::query`): under
tmux the probe's four questions travel in one envelope; tmux flushes
the envelope to the outer terminal on its own schedule and the outer
terminal's answers come back through tmux. On this box they arrive
*after* `theme::PROBE` (400 ms) has run out. The read ends on the
clock with only tmux's own XTVERSION in hand, `unheard` raises the
passthrough notice, raw mode is handed to crossterm — and the answers
land in the key stream. crossterm reads `ESC _` as alt+`_`, the APC
body `Gi=31;OK` as typed characters, `ESC \` as alt+`\`, `ESC P` as
alt+`P`, `>|ghostty 1.3` as typed characters; the `CSI 6;h;w t` and
DA1 replies it drops. That is the composer's text, byte for byte. The
one-off layout jump is most likely the same bytes (an alt-chord or
the composer growing a line) — confirm in the harness, do not assume.

Two faults, one root: the probe treats "not answered by the clock"
as "answered no", and nothing downstream knows an answer's shape.

## Bricks

1. **A late answer is still an answer.** The key stream recognises
   the probe's four replies in whatever shape crossterm delivers them
   (APC `ESC _ … ESC \`, DCS `ESC P … ESC \`, `CSI 6;h;w t`, DA1 —
   find out what crossterm's parser actually emits for each by
   playing them into the harness terminal, and write the recogniser
   against that, not against the bytes) and eats them: no character
   of a reply ever reaches the composer, no alt-chord of one ever
   fires a binding. A pure brick, `probe::Late` (or a better name),
   fed events, returning what it swallowed and whether a whole answer
   has landed; unit-tested on the exact event sequences crossterm
   produces for each of the harness's `Answers`.
2. **A late `OK` settles graphics on.** When the eaten answer is one
   `Settled::of` would have taken at start, it is taken now: graphics
   turn on, the notice is withdrawn, the next frame draws pictures
   (`Stored::catch_up` sends what the chips stood in for). The
   `OnceLock` becomes whatever lets one late settle happen and no
   second one; the surface still holds no session state (ADR-0002) —
   this is terminal state, and it lives where `Graphics` lives now.
3. **Passthrough is asked, not guessed.** Under tmux, before the
   probe, `tmux display-message -p '#{allow-passthrough}'` (one short
   process; a failure to run it means "unknown" and the probe goes
   ahead as today). `off` → no envelope is sent, no wait is spent,
   the notice is raised at once with the setting named. `on`/`all` →
   the probe waits a longer window (`theme::PROBE_THROUGH`; pick a
   value from what the harness and tmux's flush cadence justify, and
   say why in the constant's comment — the late path of brick 1
   covers whatever still arrives after it). The notice is reworded so
   the two cases read differently: the passthrough is off, or the
   pane was not the focused one when bingo started.
4. **Harness scenes** (`crates/bingo/tests/pty.rs`): `Answers::
   ThroughTmuxLate` — tmux's name at once, the outer terminal's four
   replies after the probe window (drive the delay from the same
   constant, plus margin) → composer empty, no notice, a Read of a
   picture is transmitted in the envelope. `TmuxAlone` with a stub
   `tmux` on `PATH` answering `off` → the notice, no envelope written,
   and the start-up costs no probe wait (assert the time bound
   loosely: the machine is not the machine). The existing scenes keep
   passing unchanged.

## Files

`bingo-surface-tui/src/graphics/{mod.rs,probe.rs,tmux.rs}`, the
event path in `run.rs`/`input.rs` where crossterm's events enter
(the eater sits before any binding), `theme.rs` (the second window),
`crates/bingo/tests/pty.rs`, `docs/design/tui.md` (a dated line under
the pictures section), M49's plan gets a one-line pointer here.

## Exit criteria

- [ ] The harness's late scene: composer empty, no notice, picture
      sent through the envelope after the answer lands.
- [ ] The passthrough-off scene: notice at once, nothing wrapped
      written, no probe wait.
- [ ] Every existing pty scene and the M49 unit tests unchanged.
- [ ] The layout jump: found and fixed, or found and explained in the
      Verified section with the harness bytes that show it.
- [ ] All gates; `cargo check -p bingo-surface-tui --all-targets
      --target x86_64-pc-windows-msvc` (the tmux call and the eater
      are unix-gated together with the probe).
- [ ] Hands-on in the user's tmux/Ghostty: appended by the parent.

## Non-goals

Owning the tty read instead of crossterm; a probe that never ends; a
second probe at a later time; a change to what the probe asks.

## Risks

crossterm may split a reply's characters across events in a way that
depends on read timing — the recogniser must tolerate a reply arriving
in any number of events, and must give up (and pass the events on)
the moment a sequence stops looking like a reply, so a person who
types `G` is not swallowed. Bound what is held back: at most one
reply's length, and never a keystroke that follows a completed reply.
