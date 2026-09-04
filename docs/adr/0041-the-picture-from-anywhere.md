# ADR-0041 — The picture from anywhere

Status: accepted · 2026-09-03 · Plans: M46, M47

## Context

ADR-0040 made one shape of a handed-over picture (`Image`, base64 in
`ContentPart::Image`) and refused a decoder: the budget was at its cap
and kitty takes PNG as it is. Two asks arrived the same evening: draw
the picture in the terminal for every format a person is likely to
have, and take a picture from a URL as well as a path. Both need bytes
read as pixels — the first to hand kitty a PNG it can show, the second
to turn a `.bmp` off the web into something a provider accepts. The
provider table (png, jpeg, gif, webp) is the providers' limit, not
ours; everything wider has to become PNG at the edge, and that is a
decoder. M11e measured `image` + `ratatui-image` at +33 and refused
the sixel quantiser; `image` alone with ten decoders measures +20.

## Decision

1. **One library crate for pixels.** `bingo-pictures` (library tier,
   depends on `bingo-sdk`, `image` and — for §3 — `reqwest`) owns the
   decoder:
   `to_png(&Image) -> Png { bytes, width, height }` (a PNG passes
   through; anything else is decoded, GIF at its first frame, and
   encoded as PNG), and in M47 `load(Source) -> Image` — a path or a
   URL, capped, transcoded to PNG when the provider table would refuse
   the type, else passed through. The TUI, the binary's `--print` and
   the channels all read pictures through it; no second decoder.
2. **The journal keeps what a provider accepts.** `Image` stays the
   ADR-0040 shape and the table stays the providers' four. A wider
   format is PNG by the time it is journaled, so a session replays on
   any provider and the vision projection has nothing new to learn.
3. **A remote picture is fetched at the edge, once.** `https://…` is a
   source the loader reads (reqwest, already in the tree), with the
   same cap and the same table; a provider's own URL-source block is
   not used — it would be a second representation of a picture, one
   the journal could not replay.
4. **Budget** 310 → 331: `image` with `png jpeg gif webp bmp tiff ico
   qoi tga pnm` (+20, licences pass `cargo deny`) and the member (+1).
   `ratatui-image` and its quantiser stay refused; the kitty encoder is
   ours (M46). `hdr`, `exr`, `avif`, `dds` are refused too — measured
   at +40 more for formats no chat carries.

## Consequences

- Every picture a decoder reads is pixels on a kitty terminal and PNG
  in the journal; the `[image: type]` degrade is left for what no
  decoder reads and for terminals without graphics.
- A picture from the web is read by the person's machine, not by the
  provider: the sender's IP is the person's, and a private URL a
  provider could not reach still works.
- The sdk does not depend on `image`: the sdk's `Image::read` stays
  the table-only path reader, and the loader is where formats widen.
- The budget comment line in `scripts/budget.toml` cites this record.
- *2026-09-04, M50:* the web tool reads a picture behind a URL through
  this crate: an `image/*` body is sniffed here, so a URL and a path
  widen by the same rules and no second decoder is spelled anywhere.
- *2026-09-04, M47:* `reqwest` in the library pulls `aws-lc-sys` into
  every crate above it, and that build script wants `windows.h`, so the
  local Windows cross-check no longer runs for `bingo-surface-tui` or
  `bingo-pictures` (M45 met the same for `bingo-channels`). CI's
  `windows` job is the backstop; a `ring`-backed rustls would give the
  local check back and is recorded, not decided.

Refs: ADR-0040, ADR-0001 (library tier), M11e; Plans: M46, M47
