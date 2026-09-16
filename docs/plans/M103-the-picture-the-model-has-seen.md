# M103 — The picture the model has seen

## Goal

User, 2026-09-16, after the `blender_demo` diagnosis (a 2.2 MB PNG read
once at round 4 and uploaded again on every round after, three sessions
in parallel, the relay cutting the body and logging "Failed to read
request body"): "按照你的思路实施吧 先不压缩". After this milestone a
picture the model has answered three times is replaced, on the wire
only, by a note that says what it was and where it is; the journal, the
transcript and every surface keep the picture. No shrinking of pictures
this round. ADR-0061.

## Bricks, in build order

1. `context/budget.rs` — `pub const IMAGE_ROUNDS_KEPT: usize = 3;` with
   the one-line why (ADR-0061 §1, §3).
2. `context/elide/images.rs` (new; `elide.rs` declares `pub mod images`
   and keeps the result elision as it is) —
   `pub fn elide_old_images(messages: &[Message], rounds_kept: usize)
   -> Option<Vec<Message>>`: walk messages from the end counting
   assistant messages; a `ContentPart::Image` — at the top of a user
   message or inside a `ToolResult` — with `rounds_kept` or more
   assistant messages after it becomes `ContentPart::text(note(image))`;
   `None` when nothing changes, so the common case never clones.
   `pub fn note(image: &Image) -> String` = `[image elided: <media_type>
   <size>]` + `" " + image.whereabouts()` when there is one; `size` from
   `image.data.len() * 3 / 4` as decimal `KB`/`MB` (1 MB = 1 000 000
   bytes) with one decimal (a pure `fn size(bytes: usize) -> String`,
   tested at the boundaries).
   Tests: no images → `None`; an image in the newest round is kept; an
   image followed by exactly `rounds_kept` assistant messages is elided
   and one fewer is kept; inside a tool result the id and `is_error`
   survive and sibling text parts are untouched; a top-level image in a
   user message; a pathless image gets the note without whereabouts;
   proptest: message count and every non-image part are unchanged, and
   projecting a projection is `None` (idempotent).
3. `turn.rs` — `without_images` is followed by a sibling
   `without_old_images(&self, messages) -> Vec<Message>` that applies
   brick 2 with `budget::IMAGE_ROUNDS_KEPT`; `assemble_request` chains
   `without_images` → `without_old_images` → `elide_after_overflow`.
   Each is one function; no arm grows a body.
4. `test_support.rs` — a scripted model **with** vision (the existing one
   has none), so a turn test can watch a picture age.
5. `turn/tests.rs` — with the seeing model: round 1 `Read` returns an
   image part; three scripted assistant rounds follow; the fake
   provider's recorded requests show the image whole in requests 2–3
   and the note (with the whereabouts words) from request 4, while
   `items` still hold the `Image`. A second test: a picture pasted at
   the top of the user message ages the same way.
6. `docs/adr/0048-stable-request-prefixes.md` §4 and
   `docs/adr/0006-context-budget.md` §3 — one dated note each pointing
   at ADR-0061; `docs/adr/README.md` index line for 0061.

## Files

- `crates/bingo-core/src/context/budget.rs`
- `crates/bingo-core/src/context/elide.rs`
- `crates/bingo-core/src/context/elide/images.rs` (new)
- `crates/bingo-core/src/turn.rs`
- `crates/bingo-core/src/test_support.rs`
- `crates/bingo-core/src/turn/tests.rs`
- `docs/adr/{0061-the-picture-the-model-has-seen.md,0048-stable-request-prefixes.md,0006-context-budget.md,README.md}`

## Exit criteria

- [x] `elide_old_images` unit tests and the proptest pass; the note for
      a 2 212 534-byte PNG at `/a/b.png` reads
      `[image elided: image/png 2.2 MB] [picture: /a/b.png]`.
- [x] Turn test: request 4 carries the note, requests 2–3 the image,
      `items` the image throughout.
- [x] `bingo --print` and the RPC frames are byte-identical for a session
      with a picture (nothing user-visible changes; the existing
      black-box picture tests stay green).
- [x] every gate green (fmt, check, clippy, test, discipline, budget: no
      new dependency).

## Non-goals

- Shrinking or re-encoding a picture before the model sees it (the
  user: "先不压缩"); the `Read` cap stays `Image::MAX_BYTES`.
- A setting for the threshold; a per-model threshold.
- Any change to what a surface draws, to the journal, to the RPC schema,
  to either provider's request encoding.
- Eliding by wire size, by count of pictures, or at compaction.

## Risks

- R-prefix: each picture breaks the cached prefix once, at the round it
  leaves. Accepted in ADR-0061; measured nowhere this round.
- R-pathless: a picture with no `path` cannot be read back after it is
  elided. The note names its type and size; ADR-0061 Consequences.
- R-blind: a model without vision already sees no picture; the two
  projections compose (vision first), and the age rule finds nothing.
- R-ACP: an endpoint that holds its own context (ADR-0055) reads only
  what it is newly sent; the projection is harmless there.

## Verified (2026-09-16)

- `cargo fmt --all -- --check`, `cargo check --workspace --all-targets
  --locked`, `cargo clippy --workspace --all-targets --locked -D warnings`:
  clean.
- `cargo test -p bingo-core --locked`: 396 passed, 0 failed (387 before);
  the 9 new are 7 in `context::elide::images::tests` (one a proptest) and
  2 in `turn::tests::pictures`.
- `cargo test --workspace --locked --no-fail-fast`: 4 838 passed, 0 failed,
  2 ignored, twice in a row. Nothing flaked. The black-box picture tests
  (`crates/bingo/tests/cli/images.rs`) and the RPC `Image` wire test are
  among them and were not touched; no live `--print` byte diff was taken
  beyond them.
- `scripts/check_discipline.sh`: ok. `scripts/budget.sh`: ok — 342
  dependencies, the same 342 as before.
- `cargo check -p bingo-core --all-targets --locked --target
  x86_64-pc-windows-msvc`: compiles. The note carries a path through
  `Path::display`, which keeps the string it was given.

Three things came out other than the plan said:

- The counting in the second exit criterion is a round early for a picture
  a tool returned: that picture first reaches the wire in request 2, so it
  is whole in requests 2–4 and a note from request 5. A picture pasted at
  the top of the prompt is whole in requests 1–3 and a note from request 4,
  which is the criterion as written. Both are pinned as exact vectors in
  `turn::tests::pictures`.
- `size` reads the decoded bytes from `Image::decoded_len()` rather than
  spelling `data.len() * 3 / 4` a second time; it is the same number, and
  the padding the sdk subtracts is the more exact one. It also rounds
  before it picks the unit, so 999 999 bytes reads `1.0 MB` and never
  `1000.0 KB`.
- The turn tests live in `crates/bingo-core/src/turn/tests/pictures.rs`,
  beside `budget.rs` and the other test modules: `turn/tests.rs` was at 927
  non-test lines and the discipline gate fails a file at 1 000.
