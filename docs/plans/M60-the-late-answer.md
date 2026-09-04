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

- [x] The harness's late scene: composer empty, no notice, picture
      sent through the envelope after the answer lands.
- [x] The passthrough-off scene: notice at once, nothing wrapped
      written, no probe wait.
- [x] Every existing pty scene and the M49 unit tests unchanged.
- [x] The layout jump: found and fixed, or found and explained in the
      Verified section with the harness bytes that show it.
- [x] All gates; `cargo check -p bingo-surface-tui --all-targets
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

## Verified

### What crossterm actually emits, measured

Not guessed: crossterm 0.29's own `event/sys/unix/parse.rs` and the
byte-at-a-time `Parser::advance` of `event/source/unix/tty.rs` were copied into
a scratch crate and the harness's `Answers` bytes played through them
(2026-09-04). What comes out, for a reply arriving in one read:

| the reply the terminal sends | the events crossterm makes of it |
| --- | --- |
| `ESC _ Gi=31;OK ESC \` | `alt+_`, `G`(shift), `i`, `=`, `3`, `1`, `;`, `O`(shift), `K`(shift), `alt+\` |
| `ESC P >|ghostty 1.3.1 ESC \` | `alt+shift+P`, `>`, `|`, `g h o s t t y`, ` `, `1 . 3 . 1`, `alt+\` |
| `ESC P >|tmux 3.6b ESC \` | `alt+shift+P`, `>`, `|`, `t m u x`, ` `, `3 . 6 b`, `alt+\` |
| `ESC ] 11;rgb:… ESC \` | `alt+]`, `1 1 ; r g b : …`, `alt+\` |
| `CSI 6;20;10t` | **nothing at all** |
| `CSI ?62;22c` | `InternalEvent::PrimaryDeviceAttributes`, which `EventFilter` keeps out of `EventStream`: **nothing at all** |

Every event is `KeyEventKind::Press` with an empty `KeyEventState`; a capital
carries `SHIFT`. Two consequences the plan did not have:

- **The cell can never come late.** `\x1b[6;h;wt` reaches `parse_csi`, whose
  `b'0'..=b'9'` arm hands a `t`-terminated sequence to
  `parse_csi_modifier_key_code`, which knows only `A B C D F H P Q R S` as final
  bytes and returns `Err` — so the buffer is cleared and *no* event is emitted.
  A late answer can therefore carry the kitty `OK` and the names, and never the
  cell. Since `Graphics::from` refuses to guess a cell (M46 risk 2), a run whose
  cell missed the window keeps the chip however much else arrives. That is why
  **brick 3's longer window is the cure and brick 1 is the net**, and it is
  recorded rather than papered over — the two ways out (an unwrapped `CSI 16 t`
  under tmux, whose reply tmux answers itself; or `TIOCGWINSZ`'s pixel fields)
  are both a change to what the probe asks or a second way of measuring one
  fact, and neither is verifiable here without a real tmux.
- **A read boundary right before a terminator's `ESC` turns one `alt+\` into
  `esc` and `\`** — and `esc` ends a turn. The ear takes that pair while it is
  mid-reply (`a_terminator_split_by_a_read_boundary_is_still_a_terminator`). The
  mirror case — a boundary right *after* an opening `ESC` — is deliberately not
  handled: it would mean holding a bare `esc` back on the chance that a reply
  follows, and an `esc` a person pressed has to fire at once.

### What landed

1. **The ear** — `crates/bingo-surface-tui/src/late.rs`, a new module, pure.
   `Late::hear(Event) -> Heard` with `Heard::{Keys(Vec<Event>), More,
   Answer(Vec<u8>)}`. The only state is the events held back, so between replies
   it is empty and the shape in hand is read off `held[0]` rather than stored
   beside it. Three openers (`alt+_`, `alt+shift+P`, `alt+]`), each pinned to the
   prefix of the answer that was actually asked for (`G`, `>|`, a digit), a body
   of plain characters, and a terminator of `alt+\`, split-`esc` + `\`, or — for
   an OSC — the `BEL` crossterm reads as `ctrl+g` (a binding of its own, so it
   *has* to be eaten). It gives up on the first event that does not fit and hands
   back everything held, in order; `MOST` (128) bounds a reply that never ends.
   The bytes are read back out of the events (`spelled`), which is one fact, not
   a buffer kept beside them. 12 tests, spelled as terminal replies and played
   through the same rule crossterm follows.
   **The theme probe's OSC replies are eaten too**, beyond the plan's four: the
   fault is one fault — a probe of `Tui::enter` answered after its clock — and
   the *layout jump* below needs those replies to reproduce, so leaving them out
   would have left half the reported bug standing. They are eaten and then
   merged like any other reply, where `probe::parse` finds nothing in them and
   the merge is a no-op.
2. **The late settle** — `graphics::late(reply)`, `Heard`, `Probe::and`,
   `Ui::withdraw`. `graphics`'s `OnceLock<Settled>` became
   `RwLock<Option<Heard>>` holding *the answer* rather than the decision:
   `Heard { probe, transport, passthrough }` is the one fact, and `Settled`
   (graphics + notice) is derived on read, so neither half can go stale against
   the other or against the answer. `None` means the run never asked
   (`BINGO_GRAPHICS=off`, or a multiplexer this cannot reach through), which
   makes "never asked" unmistakable for "asked and heard nothing" and stops a
   late reply from turning pictures on in a run that wanted none.
   **What forbids a second settle is that merging is monotone**, not a flag:
   `Probe::and` only ever adds (`kitty |=`, `cell.or`, names deduplicated), so a
   second late reply that says nothing new changes nothing — `no second one` is
   unrepresentable rather than guarded. `run.rs` gained `Run::heard` and
   `Run::answered_late`, and `Run` a `ear: late::Late` field beside `stored`
   (both are terminal state, ADR-0002); `drive` grew no line, because the loop
   already had somewhere for the loop's state to live.
3. **The passthrough is asked** — `tmux::{Passthrough, allows, passthrough}` and
   `theme::PROBE_THROUGH`. `tmux display-message -p '#{allow-passthrough}'`
   runs once, before the probe, with stdout piped and stderr dropped, and its
   wait is bounded by `theme::PROBE` (a tmux client against a wedged server would
   otherwise hold the whole start-up; a local one answers in milliseconds).
   `off` → no envelope, no window, `PASSTHROUGH_OFF` at once; `on`/`all` →
   `PROBE_THROUGH`; anything else, including a tmux that cannot be run, is
   `Unknown` and probes exactly as before.
   **`PROBE_THROUGH` is `3 * PROBE` = 1200 ms**, and it is written as three times
   the same constant rather than as a second number. Why three: a bare terminal's
   answer crosses one boundary, while under tmux the question crosses to the
   server, waits there for the flush that hands it to the client's terminal, and
   the answer comes back through the client and the server into this pane —
   three legs, each waiting on tmux's own event loop rather than the terminal's.
   The box this was reported from took longer than one leg's worth. It is
   arithmetic from the reported failure, not a measurement of tmux's flush
   cadence: what a real tmux costs was not timed here, and whatever still
   overruns it is brick 1's.
   The notice split in two, so the two cases read differently:
   `PASSTHROUGH_OFF` = "tmux: pictures need \`set -g allow-passthrough on\`" and
   `PASSTHROUGH_UNHEARD` = "tmux: pictures need bingo started in the focused
   pane". `unheard` takes the whole `Heard` and reads the setting; a run where
   the outer terminal *did* answer says nothing either way.
4. **The scenes** — `crates/bingo/tests/pty.rs`. `Answers` gained
   `ThroughTmuxLate` and `TmuxPassthroughOff`, a `late()` half written after
   `LATE` (`bingo_surface_tui::PROBE_THROUGH + 400 ms`, driven from the surface's
   own constant, which is why that constant is now `pub`) from a thread of its
   own, and a `passthrough()` that puts a one-line stub `tmux` at the head of the
   child's `PATH` — so what the surface is told is the scene's answer and not
   whatever tmux the machine happens to carry. A test waits on the write itself
   (`wait_late`), never on a clock.

### The layout jump: found, and it is those same bytes

Reproduced in the harness before the fix (a throwaway pty scene, `TMUX` set, the
answers written 1200 ms after the query, screens diffed):

- With the graphics reply alone — `ESC P >|tmux 3.6b ESC \` in time, then
  `ESC _ Gi=31;OK ESC \  CSI 6;20;10t  ESC P >|ghostty 1.3.1 ESC \  CSI ?62;22c`
  late — the composer reads `> Gi=31;OK>|ghostty 1.3.1`, which is the user's
  report byte for byte, and **at 80×24 and at 140×40 the layout does not move**:
  every region keeps its rows and only the box's own row and the status line
  differ.
- Add the *background* probe's late replies in front of them —
  `ESC ] 11;rgb:1e1e/1e1e/2e2e ESC \  ESC ] 10;rgb:cdd6/f4f4/f5f5 ESC \` — and at
  80×24 the composer reads
  `> 11;rgb:1e1e/1e1e/2e2e10;rgb:cdd6/f4f4/f5f5Gi=31;OK>|ghostty 1.3.1-main+0af`
  / `  8e6d`: **two rows.** `Demand.composer` goes from 1 to 2, the box takes
  four rows instead of three, and the transcript loses one — rows 11 through 21
  all move up by one. That is the jump: the welcome box and everything above the
  input box shifting by a row, once, at start.

So the jump is the composer growing a row when the swallowed text outgrows the
box's width, and it needs no separate fix: the text never reaches the composer
now. Its two ingredients — a long enough reply, or a narrow enough pane — explain
why the user saw it with a 25-column composer line while the 80-column harness
did not.

### What is not verified

- **No real tmux and no real terminal behind one**, exactly as M49 recorded.
  Every terminal, every multiplexer and now the `tmux` binary itself are things
  this repository wrote. In particular: that
  `display-message -p '#{allow-passthrough}'` prints `on`/`off`/`all` on tmux
  3.6b is taken from the plan and from tmux's option vocabulary, not observed —
  and if it printed something else the answer reads as `Unknown` and the probe
  behaves exactly as it did before this milestone, which is the fail-safe.
  **This is the parent's hands-on.**
- **`PROBE_THROUGH`'s value is arithmetic** (see above), and the cost of the
  window when nothing ever answers — a pane that was not focused at start, with
  the passthrough on — is now 1200 ms of start-up instead of 400. That is the
  price of the fix, paid only by a broken configuration.
- **The notice is not asserted on a pty screen**, following M49's reasoning: a
  notice lives 4.4 s from the moment `opening::notices` raises it, which is
  *before* the first frame, and a slow box can expire it before it is ever drawn
  — asserting it on a screen would pin this machine. It is asserted at both ends
  instead (`unheard` over five answers, `opening::notices` putting it on a `Ui`,
  `Ui::withdraw` taking it back), and the off-scene asserts the permanent facts:
  not one byte of the question was written, wrapped or bare. (That the notice's
  clock starts before there is a screen to draw it on is a real wrinkle this
  milestone did not touch.)
- **The all-four-late case is a unit test, not a pty scene.** With every reply
  late, the cell is destroyed, so the honest end state is: composer clean, notice
  withdrawn, and still no pictures. The pty scene therefore has tmux's name, the
  kitty `OK` and the cell arrive inside the window and the outer name and DA1
  after it — the split is arbitrary, as any tmux flush boundary is — so that the
  criterion's "picture sent after the answer lands" is a real assertion rather
  than a decoration. The other shape is
  `a_late_answer_with_no_cell_still_takes_the_wrong_word_back`.
- **The M49 unit tests kept every assertion but not every call.** `unheard` and
  `Settled::of` now take the whole `Heard` (brick 3 gave them a third fact to
  read), `Graphics::from` takes `&Probe` (so `Settled` derives without cloning),
  and `PASSTHROUGH` split into two constants, so the tests that named it name the
  new one. Nothing was weakened or deleted; the existing pty scenes are unchanged
  except that the tmux ones now find a stub `tmux` saying `on`, which is the
  answer they were silently assuming.
- **The Windows cross-check cannot run here**, for ADR-0041's reason —
  `reqwest → rustls → aws-lc-sys`, whose build script compiles C against
  `windows.h`, and there is no Windows SDK on this machine (output below). So the
  platform arms were checked the only other way available: the gates in
  `graphics/mod.rs` and `graphics/tmux.rs` were temporarily inverted
  (`unix` ↔ `windows`) and the crate compiled on this box, which built the
  `not(unix)` arms of `exchange` and `passthrough` with no error and no warning
  of their own. `Passthrough` carries `#[cfg_attr(not(unix), allow(dead_code))]`
  because a platform that asks a terminal nothing only ever builds `Unknown`.
  `late.rs`, `Probe::and`, `Ui::withdraw` and `run.rs` are platform-free.
- **`tui-smoke.sh` was not run**: it takes tmux sockets other workers on this
  machine are using. The pty suite is the terminal-byte gate here.

### Gates, all from the worktree, `-j 2`

```
$ cargo fmt --all -- --check                                    # clean
$ cargo check --workspace --all-targets --locked                # Finished
$ cargo clippy --workspace --all-targets --locked -- -D warnings   # Finished
$ cargo test --workspace --locked --no-fail-fast
                             # 3791 passed, 0 failed across 81 targets
$ scripts/check_discipline.sh                                   # discipline ok
$ scripts/budget.sh    # dependencies (unique, normal): 332 (max 332); budget ok
$ cargo deny check                 # advisories ok, bans ok, licenses ok, sources ok
$ cargo test -p bingo --locked --test pty             # 11 passed, 0 failed
$ cargo check -p bingo-surface-tui --all-targets --locked \
      --target x86_64-pc-windows-msvc
        # FAILS in aws-lc-sys' build script: jitterentropy-base-windows.h:49
        # fatal error: 'windows.h' file not found  (ADR-0041's note, as M49)
```

No crate joined the tree: the budget is 332 before and after. The known
`bingo_plugin_rpc::connection` flake was not hit. Measured against the branch
point: the surface's own suite is 855 → 872 (11 in `late.rs`, 3 in
`graphics/mod.rs`, 1 each in `probe.rs`, `tmux.rs` and `ui.rs`) and the pty
suite 9 → 11, so the workspace is 3772 → 3791.

### Hands-on, in the user's tmux under Ghostty (the parent, with the user)

*To be filled after the merge: at start the composer is empty and the layout does
not jump; with `allow-passthrough on` a `Read` of a picture draws it; with
`allow-passthrough off` in a fresh session the notice names the setting and the
chip stands; and a run started in a background pane says the other notice. Worth
watching for: whether `display-message -p '#{allow-passthrough}'` prints what
this expects (`tmux display-message -p '#{allow-passthrough}'` by hand answers
it), and whether 1200 ms is enough on that box — if the pictures still need the
late path, the cell is what is missing and the plan's own diagnosis has its
answer in "What is not verified" above.*
