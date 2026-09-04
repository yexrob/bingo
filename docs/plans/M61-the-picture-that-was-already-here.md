# M61 — The picture that was already here

## Goal

Three reports from the user, 2026-09-04, on pictures the transcript
draws (M51/M56):

1. **A remote picture is fetched every time.** A URL the model wrote
   is fetched on every session that shows it, and again on resume.
   Wanted: a cache on disk, **two weeks** by default.
2. **`ctrl+g` still flickers the pictures once.** The reconciler no
   longer deletes a picture a frame did not place (`6dffe3a8`), so
   the bytes that remain on that path are the frame's own: the list is
   drawn over the transcript and closing it rewrites the placeholder
   cells under it. Whether anything is still sent, and whether the
   rewrite itself is what a person sees, is to be found out with
   bytes, not assumed.
3. **Loading a picture stalls the TUI.** The first frame that draws a
   picture decodes it on the render path (`transcript/pictured.rs::
   drawn` → `Decoded::png`), and the send that follows scales it
   (`Decoded::thumbnail`) on the same thread. A large screenshot is
   hundreds of milliseconds during which no key is read.

## Bricks

1. **A cache under the data dir.** In `bingo-pictures` (the crate that
   fetches), `load` of a URL looks first in `<data_dir>/pictures/
   cache/<sha256 of the url>` beside a sidecar or a filename that
   carries when it was fetched (one representation: the file's own
   mtime is the fact, nothing beside it). A hit younger than the TTL
   is read, not fetched; a miss or a stale entry is fetched and written
   atomically (write to a temp name in the same dir, rename). The TTL
   is a `Duration` argument with the default `14 * 24h` written once
   as a named constant; `bingo_core::settings` gains `pictures.
   cache_days` (integer, default 14, `0` = never cache) read where the
   surface builds its loader — one setting, one place. A cache that
   cannot be written is a cache that is not used: the picture still
   shows. Pure brick: `cache::path(url, dir)`, `cache::fresh(mtime,
   now, ttl)`; tests with a tempdir and a clock argument, never a
   sleep. Sweep: a stale entry is removed when it is passed over;
   nothing walks the directory at start.
2. **Decode off the frame.** `Decoded` answers a frame at once with
   what it has — pixels, "would not decode", or *not yet* — and never
   decodes on the caller's thread. A *not yet* draws the chip the
   words already have (`[image: …]`) for that frame and hands the
   decode to `tokio::task::spawn_blocking` through the run's existing
   reply channel (`Reply::…`, as `Reply::Linked` does); the reply
   wakes a frame, which now finds the pixels. The same for the scaled
   thumbnail the send needs: `Stored::catch_up` asks `pixels` and a
   *not yet* is a picture not sent this frame and not claimed held
   (the reconciler already handles `None`); the frame after the reply
   sends it. One in-flight decode per `(id, within)`; a request for a
   picture already decoding is not a second decode. Every frame stays
   under the tick: assert it in a test with a picture large enough
   that a synchronous decode would not (use `bingo_pictures::testing`
   and a decode counter, not wall clock).
3. **The `ctrl+g` path, in bytes.** A pty scene (or a `run.rs` test
   with the `Recorder`, which sees every `place`): a transcript with
   one picture; open the switcher; close it. Assert **no** graphics
   bytes go out on either frame. If that already holds, the flicker is
   the cell rewrite: see whether the list can be drawn over the
   transcript without rewriting the picture rows it does not cover
   (ratatui diffs cells, so a row the list does not touch is not
   rewritten — check what the list's `over()` actually clears), and
   whether the rows it does cover can be given back in one
   synchronized frame (M52 wrote every frame inside mode 2026 outside
   a multiplexer; under tmux 3.6b there is no such thing, and then the
   answer is "tmux, and 3.7b fixes it" — say so in the Verified
   section with the bytes).

## Files

`bingo-pictures/src/{load.rs,cache.rs}`, `bingo-core/src/settings.rs`
(+ its fixture), `bingo-surface-tui/src/graphics/{decoded.rs,
stored.rs,linked.rs}`, `transcript/pictured.rs`, `run.rs` (the reply
and the wake), `docs/design/tui.md` (a dated line), ADR-0041 (a line
for the cache's place on disk), `docs/plans/M51-*.md` pointer.

## Exit criteria

- [x] A URL shown twice is fetched once (test with a local server or
      a counting `fetch` seam); a stale entry is fetched again and
      rewritten; `cache_days = 0` never writes.
- [x] No frame decodes or scales on the render thread (counter test);
      a picture appears within a frame or two of its decode reply.
      *Structural rather than a counter — see Verified.*
- [x] The switcher open/close scene writes no graphics bytes.
- [x] All gates; `cargo check -p bingo-core --all-targets --target
      x86_64-pc-windows-msvc`. *`bingo-pictures` cannot be
      cross-checked here (ADR-0041's note) — see Verified.*
- [ ] Hands-on in the user's terminal: appended by the parent.

## Non-goals

Caching local files; a cache size cap (the TTL bounds it in time, a
cap is a second rule — note it as a risk); prefetching.

## Risks

A decode reply for a picture that has since scrolled off, or whose
item was rewound away, must be dropped and not drawn. `spawn_blocking`
on a run whose runtime is single-threaded in tests: the tests already
use `#[tokio::test]` where they need it — check the `Run` test harness
before choosing the seam. The cache directory is shared between
concurrent bingo processes: the atomic rename is what makes two
writers safe; a reader of a half-written file must never happen.

## Verified

*2026-09-04, worktree `.claude/worktrees/m61` on `m61-picture-cache`,
base `72644bd9` (M60 merged; `6dffe3a8` and `a65c6c9c` both on it).*

### What landed

Eight commits, three bricks.

1. **A cache under the data dir** (`dc3d7f8b`, `d003d8a7`, `340fcc0d`,
   `4a3e14b4`). `bingo-pictures/src/cache.rs`: `Cache::under(data_dir,
   days) -> Option<Cache>` — `None` for `0`, and then nothing is written
   and nothing is read. The layout is
   `<data_dir>/pictures/cache/<32 hex characters>`, one directory below
   the file a click hands a viewer (M56), and `viewer::DIR` is now
   `bingo_pictures::cache::DIR` so the name is spelled once. `DAYS = 14`
   is the constant. `load(source, cache)` reads a hit younger than the
   life, removes a stale entry as it passes over it, and writes a miss
   through `<name>.<pid>.<n>.tmp` and a rename. Pure bricks: `fresh(
   mtime, now, ttl)` with the clock handed in, and the entry's name.
   `bingo-core`'s `KERNEL_KEYS` gains `pictures` and
   `settings::picture_cache_days(layers)` is its one reading; the bin
   reads it beside the demo switch's own and puts it in the surface's
   options, where `run.rs` builds the cache.
2. **Decode off the frame** (`3d8b4f51`, `dcabd9e8`, `2fe24631`).
   `Decoded` answers two questions now. `size(id, image)` is the frame's,
   and it is a header read (`bingo_pictures::size`) rather than a decode.
   `pixels(id, image, within)` is the wire's, and it answers `Ready`,
   `NotYet` or `Never` and never decodes on the caller's thread: a
   `NotYet` is left on `Decoded::owed`, `run/showing.rs::fit` hands each
   to `spawn_blocking`, and `Reply::Fitted` folds it back. Asking is
   taking, so thirty frames a second cost one fitting.
   `Stored::catch_up` takes the same three answers, and a `NotYet`
   neither sends nor forgets. The picture path left `run.rs` for
   `run/showing.rs` (957 → 862 non-test lines).
3. **The `ctrl+g` path, in bytes** (`3d017480`). Below.

### What the plan got wrong

- **The size must not wait, and it need not.** The plan had `Decoded`
  answer *not yet* to everything, the frame draw the chip, and the reply
  wake a frame that finds the pixels. That would have been a worse
  flicker than the one being fixed: how many cells a picture takes is
  where every row under it goes, so a transcript of ten pictures would
  have reflowed ten times as they landed. It is also unnecessary — a
  picture's size is in its own header, so *measuring is not decoding*.
  `bingo_pictures::size` reads the first 64 KiB of the base64 (48 KiB of
  picture, a whole group of four so the prefix decodes on its own), takes
  a PNG's `IHDR` here and asks every other format's decoder for its
  header alone, and falls back to the whole payload for the rare picture
  with more than 48 KiB in front of its own size. The frame is answered
  now; only the pixels are late. The blast radius of the change is
  correspondingly small: every snapshot and `TestBackend` test in the
  crate passed untouched, because the cells are still drawn on the frame
  that asks. Only the three `run.rs` tests that watch the *wire* moved.
- **`scaled` is gone, and nothing makes a whole picture as PNG any
  more.** `to_png` + `scaled` meant a JPEG was decoded, encoded as PNG,
  decoded again and resized — the whole-picture PNG existed only as a way
  station. `fitted(image, within)` decodes once and writes out at the
  size the cells hold; a PNG already inside its box is still the very
  bytes that came in. `to_png` stays for `viewer`, which wants the whole.
- **No revision bump was needed.** M51 had to teach `blocks::Revision`
  about `Linked::answers()` because a late picture changed the block's
  *lines*. A late fitting does not: the cells were drawn on the first
  frame, and what arrives later is only what goes on the wire, which
  `hand_pictures` does after every draw. Nothing redraws a block.
- **The setting is `pictures.cacheDays`, and `cache_days` reads too.**
  Every other settings key is camelCase (`maxTokens`, `braveApiKey`), and
  a lone snake-case key would be the odd one; but the ask was written
  `cache_days`, and a spelling that silently does nothing is worse than
  either. Both are read, `cacheDays` is the one messages name, and any
  *other* key under `pictures` is a startup failure that names the layer
  — the unknown-key notice sees top-level keys only, so nothing else
  would catch `cacheDaze`.
- **The bin reads the setting; the surface is handed it.** The plan's
  Files did not list `crates/bingo/src/main.rs`, but there is no other
  way: a plugin may not import the kernel (ADR-0001), and `ConfigView`
  publishes only the kernel's `thinking` and the policy's own words, so
  a surface cannot ask for a settings key. It rides in
  `SurfaceOptions::args` — "surface-specific options, from the command
  line or config" — read where the run builds its loader. `run` grew past
  sixty lines doing it, so which surface does the work is now its own
  function.
- **The entry's name is FNV-1a over 128 bits, not sha256.** `sha2` is in
  the lock only under `wezterm`, a dev-dependency of the pty suite, so
  depending on it directly would be 333 against a cap of 332; `sha1` is
  in the normal tree but is not sha256 either. The hash is written once
  in `cache.rs`, with the reason beside it. It has one job — telling two
  addresses apart — and it is not a signature.
- **One change of brief, mid-milestone** (the user, after seeing brick 2
  described): a paste must place its `[image N]` at once and never wait
  on the thumbnail. It falls out of the same seam rather than needing a
  second one — `composer/strip.rs::thumbnail` measures where it used to
  decode, so the token and the strip's slot are both on the frame the
  paste lands on and only the pixels are late.
  `a_carried_picture_is_sent_small_and_kept_when_its_token_goes` now
  asserts all three beats: the paste frame writes no picture bytes and
  has `[image 1]` in the line, the frame after the reply sends the
  8×3 thumbnail (80×60 pixels, not the picture's 400×300), and the frame
  after the submit sends nothing and takes nothing away.

- **`run/showing.rs` holds functions over the run, not more of its
  methods.** `Run`'s inherent `impl` was already in three files
  (`run.rs`, `run/submit.rs`, `select.rs`) and `check_discipline.sh` §5
  caps it at three. The move was needed either way: `run.rs` was at 957
  non-test lines against a hard 1000, and this milestone adds to it.

### The `ctrl+g` scene, in bytes

`run::tests::the_switcher_opened_over_a_picture_writes_no_graphics`: a
transcript with one `Read` picture on a terminal that draws them, the
switcher opened with `ctrl+g` over it and closed with `esc`, through
`Run::terminal_event` and `Run::paint` with the `Recorder` — which sees
every `place`.

```
frame 1   draws 10×10 placeholder cells      places: 0   (the fitting is owed)
          — the fitting lands —
frame 2   the picture goes out               places: 1   c=10,r=10
ctrl+g    the list, 4 rows, over the foot    places: 1   nothing went out
          100 placeholder cells → 80
esc       the list goes                      places: 1   nothing went out
          80 placeholder cells → 100
```

So **no graphics bytes go out on either frame**, and every cell comes
back. The flicker that is left on that path is the cell rewrite, and it
is bounded: `view::over` renders `Clear` over exactly
`min(lines.len(), region.height)` rows at the bottom of the band above
the composer — four rows here, two of them the picture's last two — and
ratatui diffs cells, so the eight rows of picture above the list are not
written at all. The two that are covered are repainted when the list
goes, which is what a person sees; the terminal was holding the picture
the whole time (`6dffe3a8`), so it repaints from its own copy and no
byte crosses the wire. `a65c6c9c` closed the other half on the user's
box — the dim pass was rewriting a placeholder's colour, which *is* the
picture's number, and that made the whole picture vanish rather than the
covered rows alone.

**What remains is the multiplexer's.** Those rewritten rows go out inside
one synchronized update only where mode 2026 is honoured; under tmux this
run writes its frames bare on purpose (`terminal::synchronizes`), because
tmux acts on DECSET 2026 only from 3.7 and 3.7/3.7a act on it wrongly
(3.7b: "Fix so that the end of a synchronized update again triggers a
redraw"). So under tmux 3.6b the two covered rows are painted as two
writes rather than one. That is tmux's, and 3.7b is the fix — there is
nothing this surface can add, and the bytes above say it is not sending
anything else.

### Gates, all from the worktree, `-j 2`

```
$ cargo fmt --all -- --check                                 # silent, exit 0
$ cargo check --workspace --all-targets --locked              # Finished
$ cargo clippy --workspace --all-targets --locked -- -D warnings
                                                              # Finished
$ cargo test --workspace --locked --no-fail-fast
    exit 0; 81 result lines, 0 with a failure; 3847 passed, 0 failed
$ scripts/check_discipline.sh
    dependency direction ok / kernel names no tool / cohesion ok / discipline ok
    (pre-existing warns only; run.rs 957 → 862, main.rs 761 → 786)
$ scripts/budget.sh
    dependencies (unique, normal): 332 (max  332)
    warm cargo check -p bingo-core: 0s (max  20s)
    relink isolation: touching the TUI recompiled 0 crates for core (must be 0)
    budget ok
$ cargo deny check          # advisories ok, bans ok, licenses ok, sources ok
$ cargo test -p bingo --locked --test pty            # 11 passed, 0 failed
$ cargo check -p bingo-core --all-targets --locked \
      --target x86_64-pc-windows-msvc                         # Finished
$ cargo check -p bingo-pictures --all-targets --locked \
      --target x86_64-pc-windows-msvc
                    # FAILS in aws-lc-sys' build script (ADR-0041's note)
```

No crate joined the tree: 332 before and after. `scripts/tui-smoke.sh`
was not run — other workers hold the tmux sockets. The known
`bingo_plugin_rpc::connection` flake was not hit.

### What is not verified

- **No real terminal drew any of this.** As in M46, M48, M49, M51 and
  M56, every terminal in these tests is one this repository wrote. What
  is proven is the bytes and the cells. The hands-on line is the main
  session's — and it is the one that matters for the `ctrl+g` report,
  since what is left there is a repaint, not a byte.
- **`bingo-pictures` cannot be cross-checked for Windows here**, for
  ADR-0041's recorded reason: `reqwest` → `rustls` → `aws-lc-sys`, whose
  build script compiles C against `windows.h`. `bingo-core` checks clean
  and carries this milestone's settings change. The cache is the one
  path-and-clock code added, and it is `std::fs` and `SystemTime`
  throughout — `metadata`/`modified`/`read`/`remove_file`/
  `create_dir_all`/`write`/`rename`, `File::set_modified` in a test,
  `std::process::id`. One Windows difference is worth naming and is not
  exercised: `std::fs::rename` there replaces an existing file, but can
  fail with a sharing violation while another process has the entry open
  — which this code swallows by design, so the picture still shows and
  the next run rewrites the entry. CI's `windows` job is the backstop.
- **"No frame decodes" is structural, not counted.** The plan asked for
  a decode counter. What is asserted instead is stronger and needs no
  clock: the *only* call in the TUI that reaches a decoder for a
  picture's pixels is `graphics::decoded::Fit::fitted`, which nothing but
  `run/showing.rs::fit` calls and which it calls inside
  `spawn_blocking`; `Decoded::pixels` can only ever return `NotYet` until
  a `Fitted` is folded back
  (`a_rectangle_is_not_yet_until_the_run_has_fitted_it`), and the three
  `run.rs` wire tests assert `places` is empty on the frame that draws.
  No milliseconds were measured: a wall clock in a test pins the machine.
- **The base64 decode that `size` does is not free**, only cheap: 48 KiB
  of a payload on the frame that first measures a picture, once per
  picture. Not measured.
- **Two processes writing one entry** is safe by the rename, not by a
  test — a two-process test would pin scheduling.
- **The cache has no size cap** (a non-goal), and nothing walks the
  directory: an entry for an address never asked for again is never
  passed over, so it stays until something asks. A fortnight of
  screenshots at `Image::MAX_BYTES` is a bound in principle and not one
  a person would like; the cure is a sweep at start, which is the second
  rule this milestone refused to add.
- **The setting is read once, at start.** Changing `pictures.cacheDays`
  takes a restart. Nothing says so to a person.
- **`--print` and the channels keep no cache.** They pass `None`. A
  `--print` run reads a URL once and ends; a channel would benefit, and
  has not been given one.
