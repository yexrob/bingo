# M105 — The file behind the picture

## Goal

User, 2026-09-16, on M104: "默认发压缩图，但是未压缩的图 如果模型觉得需要参
照 需要让模型有地方能找到". After M104 a model is told where a picture's
file is (`[picture: /path]`, ADR-0052 §3) but has no way to see that file
as it is: `Read` on the path bounds it again. After this milestone `Read`
says when what it returned is a bounded rendering, names the file's own
type and size beside it, and takes `original: true` to return the file
itself, up to the journal's cap. One spelling of a byte count for the
model, in the sdk, used by this note and by ADR-0061's. Dated note on
ADR-0062 §3/§4; no new ADR (a plugin's tool argument, not a boundary).

## Bricks, in build order

1. `bingo-sdk` — `pub fn bytes::words(n: usize) -> String`: decimal
   `KB`/`MB`, one decimal, rounded before the unit is chosen (`999_999`
   → `1.0 MB`, `2_212_534` → `2.2 MB`, `3_000` → `3.0 KB`). Moved from
   `bingo-core/src/context/elide/images.rs::size` with its boundary
   tests; `images.rs` calls it. The sdk is the one place because two
   plugins and the kernel now say a size to a model.
2. `bingo-tool-fs/src/read.rs` — `ReadArgs.original: Option<bool>`
   (schema doc: "For a picture: `true` returns the file as it is instead
   of the bounded rendering; refused above the journal's cap"). The
   picture arm becomes two functions: `original(media_type, bytes,
   path)` → `Image::from_bytes(media_type, &bytes)` (its `TooLarge`
   error already names the bytes and the cap) `.at(path)`, one image
   part, no words; `bounded(media_type, bytes, path)` as today, plus a
   text part after the image **only when the rendering is not the
   file**: `seen.media_type != media_type || seen.decoded_len() !=
   bytes.len()` (a pure `fn rendered(seen, media_type, len) -> bool`).
   The words: `[shown bounded: <seen type> <seen size>; the file is
   <file type> <file size>. Read it with original: true for the file as
   it is]`. A text file ignores the flag.
3. `description()` — the picture sentence becomes: "A picture ({exts})
   comes back as the picture itself, bounded to what a model is sent
   (inside 2000×2000 pixels, under 1 MB); when that changed it, the
   result says so and names the file's own size, and `original: true`
   returns the file as it is, up to 5 MB. It is placed in the user's
   transcript beside this call, where their surface can draw it." The
   numbers come from `bingo_pictures::{MODEL_BOX, MODEL_BUDGET}` and
   `Image::MAX_BYTES` through `bytes::words`, never typed twice.
4. Tests in `read.rs`: a noisy PNG over the budget comes back as
   `[Image(jpeg), Text(note)]` with the note naming `image/png` and the
   file's size; a small PNG is one image part and no words; `original:
   true` on that noisy PNG returns the file's own bytes and media type,
   no words, `path` set; `original: true` on a file over
   `Image::MAX_BYTES` (and under the 8 MB file cap) is refused naming
   the cap; `original: true` on a text file reads the text. The spec's
   description contains "original: true" and "1.0 MB".
5. `docs/adr/0062-the-picture-a-model-is-sent.md` — dated note after
   Consequences: §3's "gets the file bounded the same way" gains "unless
   it asks with `original: true`; the result says when it was bounded".

## Files

- `crates/bingo-sdk/src/{lib.rs,bytes.rs (new)}`
- `crates/bingo-core/src/context/elide/images.rs`
- `crates/bingo-tool-fs/src/read.rs`
- `docs/adr/0062-the-picture-a-model-is-sent.md`

## Exit criteria

- [ ] `bytes::words` tests pass in the sdk; `images.rs` has no `size`
      of its own and its tests still pass.
- [ ] The five `read.rs` tests above pass; the description names the
      flag and the caps from the constants.
- [ ] Every gate green (fmt, check, clippy, test, discipline, budget: no
      new dependency); `cargo check -p bingo-sdk -p bingo-core
      --all-targets --target x86_64-pc-windows-msvc` compiles
      (`bingo-tool-fs` is under the ADR-0041 local limit).

## Non-goals

- A crop or zoom on `Read` (a region of the file at full resolution);
  the one way to see past 2000 px on a picture over 5 MB. Recorded, not
  scheduled.
- Words beside a pasted or fetched picture: the provider's
  `[picture: /path]` and the `Read` description already tell the model
  where the file is and how to ask.
- Any change to the doors, the ladder, the box or the budget.

## Risks

- R-upload: `original: true` re-opens a large upload on purpose; it is
  the model's explicit ask, at most 5 MB, and ADR-0061 takes it off the
  wire after three answers as any picture.
- R-words: the note is a text part inside a tool result; both providers
  already carry mixed image-and-text results (`text_of`, `blocks`), and
  the TUI draws the image and shows the words as today.
