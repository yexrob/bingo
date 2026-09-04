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

- [ ] A URL shown twice is fetched once (test with a local server or
      a counting `fetch` seam); a stale entry is fetched again and
      rewritten; `cache_days = 0` never writes.
- [ ] No frame decodes or scales on the render thread (counter test);
      a picture appears within a frame or two of its decode reply.
- [ ] The switcher open/close scene writes no graphics bytes.
- [ ] All gates; `cargo check -p bingo-pictures -p bingo-core
      --all-targets --target x86_64-pc-windows-msvc`.
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
