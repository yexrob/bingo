# M78 — A thought holds where it ended

## Goal

User, 2026-09-06, right after v0.5.2 shipped M77: "默认思考完不要闭合吧
就保持住 不然还是会在闭合的时候布局变化" — when the thinking is over,
do not close the block by default; hold it, or the layout still moves at
the close. M77 fixed the height *while* a thought streams; the close still
takes two rows away and everything above the block drops by two.

So a finished thought **holds**: the heading becomes `✻ Thought for 2s`
and the two rows under it stay exactly the rows that were there when the
last delta landed — the same `tail`, at the same width, no `… +N lines`
row (that would add a row and move the layout the other way). The click
still hides it: a thought that is over is the one kind with a shut, and
the shut is reached by going round, not met on arrival.

## Bricks

1. **`fold.rs`**: every kind starts at `Peek` — `start` goes, `fold_of`
   answers `Peek` for what the map does not hold. The one fact the module
   keeps about thoughts moves from the start to the ring: `shuts(item)`
   (today's `thinking_is_over`) says which kind has a shut, and `cycled`
   goes `Peek → Open → Shut → Peek` for it and `Peek → Open → Peek` for
   everything else (`Open` comes back to `Shut` where the kind has one,
   to `Peek` where it does not). `deeper` unchanged. Module docs rewritten
   to say this; tests: the start table (all `Peek`), the two rings, the
   agent's-own-call exception, `deeper`.
2. **`transcript.rs`**: `thought_for` draws the same rows under its
   heading as `still_thinking` — `Peek` is `tail(text, THOUGHT_ROWS,
   width)`, `Open` the whole, `Shut` nothing — so the two halves of
   `thinking` differ by the heading alone; the shared body keeps one name
   (today's `streaming`, renamed to what it now is, e.g. `thought_rows`)
   and its doc says why a finished thought wears no cut mark. `EXPAND`
   stays for results. Tests: the close pins — for a thought with text,
   `drawn(thinking_item(t))[1..] == drawn(thought_item(t, 2))[1..]` and
   the same length, across a short text, one long paragraph, and text
   with paragraph breaks; the three states of a finished thought at
   `Peek` (last two rows), `Open` (all), `Shut` (row alone); the existing
   "closes to the row alone" test re-aimed at "holds its two rows".
3. **`screens/thinking.rs`**: `a_finished_thought_closes_to_its_own_row`
   becomes the held scene (`reasoning_held`, both sizes; delete the
   `reasoning_closed` snapshots); the `Peek` scene (`reasoning_peek`)
   goes — the peek is now the default the held scene shows — and a
   `Shut` scene takes its place (`reasoning_shut`: the row alone after a
   click went round); the `Open` scene and its snapshots unchanged;
   `thinking_and_its_decay`'s `thinking` snapshots change by the one row
   the short thought now holds — read them and say so. Every other
   snapshot in the crate byte-identical (`git status` lists only the
   thought scenes).
4. **`input.rs`** `folds`: unchanged in behaviour (`ctrl+o` on a
   finished thought climbs `Peek → Open → sheet`); fix its doc comment,
   which still says the row wears `… +N lines`.
5. **Docs**: `docs/design/tui.md` §4 thinking row, §6 `thinking` row, §7
   "a click goes round" (the ring, the start), a dated §10 entry
   (2026-09-06, later; user-directed, quote the words) in the voice of
   the entries before it; `fold.rs` module docs.

## Files

`crates/bingo-surface-tui/src/{fold.rs, transcript.rs, input.rs,
screens/thinking.rs}`, snapshots `reasoning_{held,shut}_{80x24,120x40}`
new, `reasoning_{closed,peek}_*` deleted, `thinking_*` updated,
`docs/design/tui.md`.

## Exit criteria

- [ ] The close pins pass (brick 2) and fail before the change (run them
      first, paste the red).
- [ ] `reasoning_held` snapshots show `✻ Thought for 4s` over the same two
      `⎿` rows `reasoning_streaming` shows under `✻ Thinking…` for the
      same text; `reasoning_shut` shows the row alone; no snapshot outside
      `screens/thinking.rs` changed.
- [ ] `cargo fmt --all -- --check`, `cargo check --workspace
      --all-targets --locked`, `cargo clippy --workspace --all-targets
      --locked -- -D warnings`, `cargo test -p bingo-surface-tui`,
      `scripts/check_discipline.sh`, `scripts/budget.sh` pass in the
      worktree; the full `cargo test --workspace --locked` and
      `scripts/tui-smoke.sh` run at the merge.

## Non-goals

- No hint on the heading and no `… +N lines` row under a held thought:
  either would spend a row or move the heading's words on every close.
- `THOUGHT_ROWS` stays two; the pager, `deeper`, the pointer's one
  gesture, `esc` forgetting the entry — all as they are.
- A finished *tool* result keeps its five-line cut and its `EXPAND` row.

## Risks

- `Fold::Shut` stays a variant: `ran.rs` and `pictured.rs` match on it
  and it is still reached by the ring. Nothing is deleted from them.
- A transcript now carries three rows per thought for the life of the
  session instead of one; the user chose stability over the row.
