# M77 — A tail is measured in rows

## Goal

User-reported, 2026-09-06 (a tester on Windows, relayed by the user):
the streaming thought "看得晃眼睛" — the block under `✻ Thinking…`
changes height on every delta and everything above it moves with it.
The user's intent stands as written on 2026-09-02 (§10, *a thought is
stable*): a thought streams **two rows**, height fixed, content scrolling
inside; the tester's reference (workbuddy) does the same and "the content
above the thinking region is not affected by it".

The cause is one word in one helper. `transcript::output::tail` keeps the
last `keep` **logical lines** (`str::lines`) and `transcript::under` wraps
them to the measure afterwards. A reasoning paragraph is one logical line,
so "two lines" is anywhere from two rows to twenty, and the count changes
as the paragraph grows and drops when a `\n` moves it out of the tail.
The transcript follows its foot (`Scroll::Tail` holds `total − rows`), so
every change in the block's height reflows everything above it. A
running tool's tail (`TAIL_ROWS`) has the same helper and the same bug
with long output lines. Not Windows-specific: a narrower console only
makes each paragraph more rows.

## Bricks

1. **`output::tail(arriving, keep, width)`** — the last `keep` rows *as
   they will be drawn*: take the last `keep` logical lines (each wraps to
   at least one row, so `keep` of them always hold `keep` rows), wrap them
   to `width` with `wrap::wrap_all(&plain(..), width)`, keep the last
   `keep` rows. Pure; tests in `output.rs`: one long paragraph → exactly
   `keep` rows and they are its end; short text → all of it; empty →
   none; a CJK paragraph counts cells, not chars; a `\n\n` boundary does
   not shrink the count.
2. **The two callers pass the width a `⎿` body wraps at**:
   `rows.result_width()` (the measure less the connector — the precedent
   is `folded(output, fold, rows.result_width())`). `streaming` for a
   thought, `result` for a running tool's progress. `under` wraps again;
   on rows that already fit that is the identity, and a test says so.
3. **`transcript.rs` tests**: a running thought of one 300-character
   paragraph draws `1 + THOUGHT_ROWS` rows at width 60 and the last row
   ends where the paragraph ends; a running tool's progress of long lines
   draws `1 + TAIL_ROWS`; **stability** — over every prefix of a text
   that grows word by word through two paragraph breaks, the drawn height
   never exceeds `1 + THOUGHT_ROWS` and never decreases.
4. **`screens/thinking.rs`**: one new scene, a thought being had of a
   single long paragraph, snapshots at 80×24 and 120×40 — the visual
   proof that the block is three rows at both widths. The existing
   `reasoning_streaming` snapshots stay byte-identical (their fixture's
   last two lines already fit one row each).
5. **Docs**: `docs/design/tui.md` §4 thinking row and §6 `thinking` /
   `tool running` say *rows of the transcript, as wrapped*; a dated §10
   entry in the voice of the ones before it.
6. **Hands-on, in a harness-owned tmux only** (`tmux -L m77`, never the
   user's terminal): a `BINGO_FAKE_SCRIPT` whose response carries one
   `reasoning` step of a 1500-character paragraph with two `\n\n`, the
   real binary at 80 columns, `capture-pane` every 100 ms while it
   streams; the number of rows between `✻ Thinking…` and the blank row
   under it is `THOUGHT_ROWS` in every capture after the first two, and
   the row above the block is the same row in every capture.

## Files

`crates/bingo-surface-tui/src/transcript/output.rs`,
`crates/bingo-surface-tui/src/transcript.rs`,
`crates/bingo-surface-tui/src/screens/thinking.rs` and two new
snapshots, `docs/design/tui.md`.

## Exit criteria

- [ ] `tail` unit tests (brick 1) and the drawn tests (brick 3) pass,
      and the stability test fails on the code before the change (run it
      first, paste the red).
- [ ] New scene snapshots at both sizes show `✻ Thinking…` over exactly
      two `⎿` rows; every existing snapshot byte-identical.
- [ ] `cargo fmt --all -- --check`, `cargo check --workspace
      --all-targets --locked`, `cargo clippy --workspace --all-targets
      --locked -- -D warnings`, `cargo test -p bingo-surface-tui`,
      `scripts/check_discipline.sh`, `scripts/budget.sh` pass in the
      worktree; the full `cargo test --workspace --locked` and
      `scripts/tui-smoke.sh` run at the merge.
- [ ] The hands-on drive (brick 6) captured: paste three captures
      spaced through the stream, showing the row above the block and the
      block itself unchanged in height.

## Non-goals

- The cut of a *finished* block (`kept`/`cut`: a result's five, a peeked
  thought's first two, `… +N lines`) stays by logical lines. It does not
  move under the reader, and every cut in the surface counts lines.
- `THOUGHT_ROWS` stays two; the fold, the close to `✻ Thought for 2s`,
  and the peek-from-the-top are as decided on 2026-09-02.
- No padding of the block to its full height before the text fills it:
  the one-to-three-row growth happens inside the first deltas.
- Nothing Windows-specific; no process, path, signal or clock is touched,
  so no cross-target check is owed.

## Risks

- Wrapping the whole thought per delta would be O(text) per frame while
  it streams; wrapping only the last `keep` logical lines keeps it O(keep
  lines). A test with a 20 000-character prefix guards the shape, not the
  time.
- `under` re-wrapping the already-wrapped rows must be the identity:
  `wrap` drops a leading whitespace token on a continuation row, and
  `tail`'s rows may start with the indent a paragraph carried. The drawn
  tests compare against `wrap` of the same text, not against a literal,
  so a divergence shows as a row that differs rather than a count.
