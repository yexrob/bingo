# M56 — The picture where the eye is

## Goal

Two asks from the user after seeing M51 draw (2026-09-04): a picture
always sits at the left edge of its block, which reads as dumped
rather than placed — it should sit where the words it belongs to sit;
and a click on a picture should open it in the system's viewer
(Preview on macOS), the way a click on a URL row opens the browser.

## Bricks

1. **The picture's column.** Today a picture's cells start at the
   block's own indent (`Hangs::Said`/`Returns` in `transcript/
   pictured.rs`). Give `Hangs` the column its picture stands in: a
   picture in an answer's words stands at the words' indent (the
   `⏺` speaks-indent, as now) **plus** the indent of the markdown
   construct it was written in — a list item's marker width, a
   quote's bar — read off the chip line's own leading spaces
   (`markdown::Linked` gains `indent: usize`, measured by the
   renderer; pure, tested for a plain paragraph, a bullet, a nested
   bullet, a quote). A picture under `⎿` keeps the returns indent.
   A picture wider than the room left of the right margin is fitted
   to that room, as now, and never pushed past it.
2. **A picture is a thing to click.** `Painted` already keeps where
   each block's lines landed (`Painted::{line_at,row_of}`) and each
   block's `pictures`; add the cells each picture occupies (row range,
   column range) to `Block`/`Painted` — derived at draw, the way the
   strip's thumbnails are remembered — and let `pointer.rs` answer a
   left click inside one with `Effect::OpenPicture(Source)`. The
   strip's thumbnails answer the same way.
3. **Opening it.** `Effect::OpenPicture` resolves the source to a
   path — a journal picture or a draft has bytes only: write them once
   to `data_dir/pictures/<id>.png` (through `bingo_pictures::to_png`)
   and open that; a linked picture with a local path opens the path
   itself; a linked URL opens the URL — through the one browser/file
   opener (`bingo-loopback::browser::open` after M54 lands; if M54
   has not merged, wait for it rather than adding a second opener).
   `BINGO_NO_BROWSER` keeps it shut in tests. A notice names what was
   opened; failure to open is a `Warn` notice with the path.
4. **The hint.** The status line's `? for shortcuts` sheet gains one
   line: `click a picture · open it`. Design §5's image row gets a
   dated line for both.

## Files

`bingo-surface-tui/src/{markdown.rs,transcript/pictured.rs,painted.rs,
pointer.rs,effect.rs,run.rs (one arm; at 886 non-test lines),
composer/strip.rs,welcome.rs or help}`, `docs/design/tui.md` §5.

## Exit criteria

- [x] A picture in a bulleted answer stands at the bullet's text
  column; in a plain paragraph at the words' column; under `⎿` where
  it is today (snapshots).
- [x] A click inside a drawn picture opens it (test: the effect is
  produced with the right source; the opener is exercised with
  `BINGO_NO_BROWSER`, asserting the path it would have opened).
  *Through an injected opener, not the variable — see Verified.*
- [x] Every AGENTS.md gate; budget unchanged; tui-smoke.
- [ ] Hands-on (main session with the user): click a Read picture and
  a markdown picture; each opens in Preview.

## Non-goals

Centring pictures (a transcript is left-ranged type). Dragging,
zooming or a full-size sheet inside the terminal. Copying the picture
to the clipboard.

## Risks

- Mouse reporting inside tmux: a click reaches bingo only when the
  mouse is on (`set -g mouse on`); otherwise nothing happens, which is
  tmux's, not ours — say so in the hint's design note.
- Writing journal pictures to `data_dir` leaves files behind; bound it
  by id (one file per picture, overwritten) and note the directory in
  the design doc.

## Verified

*2026-09-04, worktree `.claude/worktrees/m56` on `m56-picture-click`,
base 98b4218f (M54 merged).*

### What landed

The four bricks, in four commits.

1. **The picture's column** (`61201cbc`). `markdown::Linked` gained
   `indent: usize`, measured by the writer where the chip is written —
   the display width of that line's own decoration. `Hangs::Said` carries
   it, so `room()` is the measure less the `⏺` indent *and* the column,
   and the cells are moved right by it before the block is marked
   (`at_column`). A picture under `⎿` is untouched. 1 pure test over the
   markdown (paragraph, bullet, bullet with words, ordered marker, nested
   item, quote, quote + bullet, two items) and 3 over the drawn rows
   (paragraph, bullet — including the width it is fitted to and that
   nothing crosses the margin — nested and quote).
2. **Where the cells are** (`13c99204`). Not a remembered rectangle:
   `kitty::pictured` inverts `kitty::colour`, and
   `graphics::placed::cells(lines, area)` reads every picture's rectangle
   back out of the lines a region drew. `Painted::cells` is filled by the
   transcript and extended by the strip, and `Painted::begin` clears it
   with the frame. 7 pure tests + 1 in `kitty`.
3. **The click and the opening** (`a6b667c4`). `Painted::picture_at`,
   `Effect::OpenPicture(Source)`, one arm in `run.rs` (929 non-test
   lines) and a new `viewer.rs`. 5 click tests, 5 in `viewer`, 5 in
   `run`.
4. **The hint** (`f1ff0a3a`). One row in `keys::BINDINGS`, and design §5's
   image row has a dated line for both halves and the tmux caveat.

27 new tests; `bingo-surface-tui` 819 → 846, workspace 3763.

### What the plan got wrong, and one change of brief

- **A click opens a file, never an address** (user's word, mid-milestone,
  changing brick 3). The plan had a linked URL opened *as* a URL. It is
  not: the `Linked` memo is already holding the bytes it fetched, so an
  address goes the way a journal picture and a draft go — written once to
  `<data_dir>/pictures/<id>.png` and that file opened. `Effect::
  OpenPicture` has exactly two outcomes, the path an answer's words named
  or the one file this surface wrote, and the viewer is never sent to
  fetch bytes that are in hand.
- **`BINGO_NO_BROWSER` cannot be set from inside a test.** `std::env::
  set_var` is `unsafe` and this workspace forbids `unsafe`, so no
  in-process test can turn the opt-out on, and calling the real opener
  would open Preview on the machine running `cargo test`. The opener is
  therefore a seam — `viewer::Opener`, `Arc<dyn Fn(&str) -> bool>`,
  exactly `ShowPageTool::opened_by`'s shape (ADR-0042 §4) — which
  `Run::opener` holds and a test replaces with a recorder. The criterion's
  substance holds: the tests assert the exact word the system would have
  been handed, for all four kinds. `browser::open`'s own honouring of
  `BINGO_NO_BROWSER` is `bingo-loopback`'s test, and the CLI tests set the
  variable for child processes as before.
- **The cells are not remembered, they are read back.** The plan said to
  add the row and column range "to `Block`/`Painted`". Storing a rectangle
  beside cells that already carry the picture's number in their colour
  would be a second representation of where the picture is, and would
  drift the first time a region drew a line the block did not expect.
  `placed::cells` derives it from the drawn lines instead. Two drawings of
  one picture stay two rectangles: unioning them would make a click on the
  words between them open the picture.
- **A list item with words *and* a picture had its chip at column 0.**
  Pre-existing (M51): `TagEnd::Item` popped the item's indent off the
  margin before the rows were flushed, so the chip fell out of the list.
  The picture would then have stood under a chip that was in the wrong
  place, so it is fixed here — `Writer::end_item` flushes, then pops. The
  one existing expectation that moves is
  `a_bulleted_picture_stands_on_its_bullet`'s second half; the
  byte-identity snapshot (a document with no picture) is untouched, which
  is what says no other answer moved a row.
- **A layer over the frame takes the click with it.** Not in the plan.
  `render_transcript` runs whatever is drawn over it, so the cells under
  an open sheet are still in `Painted::cells`; opening a picture launches
  an application, so `pointer::picture` returns `None` while
  `ui.layer.showing()`. The pre-existing looseness — a click on a sheet
  cycling the fold of the block beneath it — is left as it was.
- **The `?` sheet is one row shorter than its table at 80×30.** The row
  the hint adds pushes `/resume` off the foot of the sheet in
  `help_80x30`. The sheet has always clipped (it is a `Paragraph`, not a
  scroller, and a real run has more than three commands); this fixture
  simply sat on the edge. Recorded rather than worked around.

### What is not verified

- **No real terminal drew or opened anything.** As in M46, M48, M49 and
  M51: every terminal in these tests is one this repository wrote, and
  every opener is one this test wrote. What is proven is the word the
  system would be handed and the file that is written for it. The
  hands-on criterion is the main session's.
- **`Painted::cells` is not proven against a *scrolled* transcript.** The
  lookup reads the window that was drawn, so it moves with the scroll by
  construction, but no test scrolls a picture part-way off and clicks the
  half that is left.
- **Nothing bounds `<data_dir>/pictures/`.** One file per picture id,
  overwritten, so opening the same picture a hundred times leaves one
  file — but a session that opens a hundred different pictures leaves a
  hundred, and nothing sweeps them. Bounded by id as the plan asked, not
  by count or by age.
- **The Windows cross-check for the TUI cannot run here**, for the reason
  ADR-0041's note records (`reqwest` → `rustls` → `aws-lc-sys`, whose
  build script wants `windows.h`). Output below. This milestone adds no
  `cfg`, no signal, no clock and no process to the TUI; the one
  platform-shaped thing it reaches is the opener, which is
  `bingo-loopback`'s and already has its Windows arm — and that crate
  cross-checks clean here. The paths it writes are `PathBuf::join` and
  `create_dir_all`, which have no platform of their own. CI's `windows`
  job is the backstop.
- **`view.rs` is 948 → 952 non-test lines**, `run.rs` 894 → 929. Warned,
  not failing; both should be split before the next change lands in them.

### Gates, all from the worktree, `-j 2`

```
$ cargo fmt --all -- --check                                  # silent, exit 0
$ cargo check --workspace --all-targets --locked              # Finished
$ cargo clippy --workspace --all-targets --locked -- -D warnings   # Finished
$ cargo test --workspace --locked | tee target/m56-test.log
    exit 0; 81 result lines, 0 with a failure; 3763 passed, 0 failed
$ scripts/check_discipline.sh
    dependency direction ok / kernel names no tool / cohesion ok / discipline ok
    (pre-existing warns only; run.rs 929, view.rs 952)
$ scripts/budget.sh
    dependencies (unique, normal): 332 (max  332)
    warm cargo check -p bingo-core: 1s (max  20s)
    relink isolation: touching the TUI recompiled 0 crates for core (must be 0)
    budget ok                       (`target/debug` over its soft limit, as before)
$ cargo deny check              # advisories ok, bans ok, licenses ok, sources ok
$ TMUX_TMPDIR=$(mktemp -d) scripts/tui-smoke.sh                # tui-smoke ok
$ cargo test -p bingo --locked --test pty            # 9 passed, 0 failed
$ cargo check -p bingo-loopback --all-targets --locked \
      --target x86_64-pc-windows-msvc                          # Finished
$ cargo check -p bingo-sdk   … --target x86_64-pc-windows-msvc # Finished
$ cargo check -p bingo-core  … --target x86_64-pc-windows-msvc # Finished
$ cargo check -p bingo-surface-tui … --target x86_64-pc-windows-msvc
      # FAILS in aws-lc-sys' build script (ADR-0041's note), as it did before
```

No known flake was hit. No crate joined the tree: `bingo-loopback` was
already in the workspace, so the budget is 332 before and after.

### Hands-on (main session with the user)

*To be filled after the merge: click a `Read` picture and a markdown
picture on the user's own terminal, and see each open in Preview —
including anything wrong.*
