# M47 — The picture from anywhere

## Goal

A picture reaches the model from a path or a URL, in any format
`bingo-pictures` decodes (ADR-0041): `@shot.bmp` and
`@https://x/y.jpg` in the TUI line, `--print --image <path|url>`, a
stream-json image block of a wider type, and a Feishu picture of a
type the table would refuse. Whatever the source, the journal holds
one `Image` in a type a provider accepts. Starts after M46 lands the
crate. Owner: one worker.

## Bricks

1. **`Source`** in `bingo-pictures`: `enum Source { Path(PathBuf),
   Url(Url-as-String) }` with `Source::parse(word, cwd)` — a word that
   parses as `http://`/`https://` is a URL, anything else a path
   joined to `cwd`. Pure; tests.
2. **`load(source) -> Result<Image, PictureError>`**: read the bytes
   (std fs, or reqwest blocking-free `async fn load` — pick the shape
   the callers have; the TUI loop is sync, the print surface and the
   channels are async: give both, one over the other) with
   `Image::MAX_BYTES` as the cap on what is read (a `Content-Length`
   over it is refused before the body; a body that grows past it is
   cut and refused); sniff the format with `image::guess_format`, not
   the extension or the `Content-Type` alone; a type in the provider
   table passes through as it is, any other decodable type becomes
   PNG through `to_png`; an undecodable one is `PictureError`. A
   remote read has a bounded timeout (a constant; the machine rule).
3. **The TUI** reads mentions through `load`: `complete::attachments`
   accepts any extension the decoder knows plus any `@http…` word;
   `run.rs::with_mentioned` calls `Source::parse` + `load`; a failure
   still keeps the line and says why. The paste path is PNG already.
4. **`--print --image`** takes a path or a URL; `images_from` in the
   print surface goes through `load`. Stream-json image blocks of a
   wider type are transcoded on the way in (the kernel's table stays
   the providers').
5. **Feishu**: `pictures::fetch` hands the bytes to `to_png` when the
   `Content-Type` is outside the table, so a `.bmp` in a chat still
   goes.
6. **Tests**: `Source::parse`; `load` on a temp `.bmp` written with
   `image` (becomes PNG) and a temp `.png` (passes through, bytes
   equal); a wiremock URL serving JPEG (passes through) and one
   serving TIFF (becomes PNG), one over the cap (refused at the
   header), one 404; the TUI run test with `@https://…` against
   wiremock; a `--print --image http://…` black-box against wiremock
   (the cli harness has it); the Feishu pictures test gains a BMP.

## Files

`bingo-pictures/src/{lib.rs,source.rs,load.rs}` (+ `reqwest`,
`wiremock` dev — both in the tree); `bingo-surface-tui/src/{complete.rs,
run.rs}`; `bingo-surface-print/src/{lib.rs,input.rs}`;
`bingo-channels/src/feishu/pictures.rs`; `bingo/src/main.rs` (help
text of `--image`); `docs/adr/0041`'s §1 stays as written.

## Exit criteria

- [ ] `@shot.bmp` and `@https://…/x.jpg` reach the journal as png /
  jpeg `Image` parts; a missing URL keeps the line and says so.
- [ ] `--print --image <url>` journals the picture; over-cap and 404
  are exit 1, stdout empty.
- [ ] Feishu BMP arrives as PNG.
- [ ] No lockfile crate beyond M46's; every AGENTS.md gate; Windows
  cross-check for `bingo-pictures` and `bingo-surface-tui`.

## Non-goals

A provider's URL-source block (ADR-0041 §3). Animated GIF beyond the
first frame. Resizing. Authentication for remote reads (a URL that
needs a cookie is a URL that does not load). `data:` URLs.

## Risks

- A URL in the composer that is not a picture (a web page) fails the
  sniff and keeps the line; the notice must say "not a picture", not
  hang on a large body — the cap and the timeout bound it.
- `image::guess_format` needs the first bytes only; a `Content-Type`
  that lies is ignored on purpose.
