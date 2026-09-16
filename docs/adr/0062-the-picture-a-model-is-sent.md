# ADR-0062 — The picture a model is sent

Status: accepted · 2026-09-16 · Plan: M104 · Amends: ADR-0041 §2, ADR-0052 §2

## Context

ADR-0061 stopped a picture from being re-uploaded forever; it said
nothing about how big the picture is the first three times. The one
that broke `blender_demo` was a 2.2 MB PNG of 1312×1199 pixels: the
same picture as JPEG at quality 85 is 536 KB and a model cannot tell
them apart, because a model sees a picture as roughly one token per
750 pixels and JPEG at that quality moves nothing so coarse. Above a
provider's own long-edge limit (Anthropic 1568 or 2576 px, OpenAI
2048) the pixels are discarded server-side anyway, so a larger upload
buys nothing.

Surveyed 2026-09-16: codex resizes to 2048 px and re-encodes; opencode
fits a 2000 px box and walks PNG, then JPEG at falling qualities, then
smaller, until the base64 is under a budget; gemini-cli, goose, crush,
cline and aider bound bytes or pixels and never re-encode, and carry
the 413 issues to show for it. opencode's default budget is 5 MB,
which would have let today's picture through untouched.

`bingo-pictures` is already "the one place the wider becomes the
narrower" (ADR-0041 §1) and already decodes and resizes; every picture
a person hands in and every fetched one passes through `sniffed`,
`accepted` or `load`. Two producers build an `Image` on their own: the
`Read` tool and the MCP tool bridge.

## Decision

1. **One brick bounds every picture a model is sent.**
   `bingo_pictures::bounded(media_type, bytes) -> Result<Image>` fits a
   picture into `MODEL_BOX = 2000×2000` pixels and under
   `MODEL_BUDGET = 1 MB` of encoded bytes, by opencode's ladder: a
   picture already inside both is the bytes it came as, untouched;
   otherwise it is decoded once, resized to the box (Lanczos3: this is
   the picture, not a thumbnail), tried as PNG, then as JPEG at
   quality 85, 75, 65, 55, 45 with any alpha flattened on white, then
   shrunk by three quarters and tried again, at most eight times. The
   first result under budget wins; none is `PictureError::TooBig`.
   An animated picture contributes its first frame.
2. **The three doors call it.** `sniffed`, `accepted` and `load` answer
   with a bounded picture. The `Read` tool and the MCP bridge stop
   building an `Image` by hand and go through `bounded`. A picture a
   wire client sends inline stays as it came (ADR-0040 §3: the kernel
   validates and decodes nothing); its cap is `Image::MAX_BYTES`.
3. **The file is what was handed over; the data is what the model was
   shown.** ADR-0052 §2's "the same bytes" becomes "the same picture":
   `Image.path` still names the file a person or a tool gave, and
   `Image.data` is the bounded rendering of it. A model that reads the
   path with a tool gets the file bounded the same way.
4. **The `Read` tool's own cap is the file cap** (8 MB of file); the
   5 MB `Image::MAX_BYTES` no longer refuses a large photo there,
   because what reaches the journal is the bounded picture.
5. Box and budget are constants of `bingo-pictures`, not settings.

## Consequences

- Today's picture reaches the journal as 536 KB of JPEG; a 12 MP
  photo as a 2000 px JPEG. A screenshot small enough to fit is
  byte-identical to the file, so a PNG of flat colour keeps its text
  crisp; only a picture that cannot fit as PNG goes JPEG.
- A decode and a Lanczos3 resize cost hundreds of milliseconds for a
  large picture; it runs on a blocking thread everywhere (M61's rule).
- No new dependency: `image` already carries the PNG and JPEG
  encoders. `bingo-tool-fs` and `bingo-mcp` gain an edge to the
  library tier, which ADR-0012 §1 allows.
- A picture already in a journal is not rewritten; ADR-0061 takes it
  off the wire after three answers as before.

- *2026-09-16, M105:* §3's "a model that reads the path with a tool
  gets the file bounded the same way" gains "unless it asks with
  `original: true`; the result says when it was bounded". A `Read`
  whose rendering is not the file's own bytes carries one text part
  after the picture naming the file's type and size, so a model that
  needs the file knows there is one; `original: true` answers with the
  file itself and meets only `Image::MAX_BYTES`, the cap §4 left
  standing. `bingo_sdk::bytes::words` is the one spelling of a byte
  count those words and ADR-0061's elision note both write.

Refs: ADR-0040, ADR-0041, ADR-0052, ADR-0061; Plans: M104, M105
