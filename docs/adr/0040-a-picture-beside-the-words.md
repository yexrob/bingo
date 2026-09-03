# ADR-0040 — A picture beside the words

Status: accepted · 2026-09-03 · Plan: M45

## Context

The model side already sees: `ContentPart::Image` is in the sdk, all
three providers encode it, `models/vision.rs` projects it out of a
model that cannot see, and a tool result (`Read` on a `.png`) carries
one today. The person's side does not: `Input::Text` has an
`attachments: Vec<String>` the kernel rejects ("attachments are not
supported"), and the TUI's `@shot.png` mention is the one producer of
that rejection. Nothing crosses the wire, the chat, or `--print`.

The door this opens is a field on a kernel type (`Input`). The
ratchet's question: would refusing it force a second representation?
Yes — every surface would have to smuggle a picture through text
(a path the model is told to `Read`, a data URL in the prose), each
its own spelling of "an image the person handed over", and none of
them the `ContentPart::Image` the journal already keeps.

## Decision

1. **One image, one shape.** `Image { media_type, data }` becomes a
   struct in `bingo-sdk`, and `ContentPart::Image(Image)` holds it.
   The journal bytes do not change: an internally tagged newtype
   variant of a struct serializes its fields beside the tag, and the
   frames fixture proves it. The media-type table (`png jpg jpeg gif
   webp`) and the size cap live on `Image`, nowhere else; the fs
   tool's copy of the table is deleted in favour of it.
2. **A surface resolves, the kernel journals.** `Input::Text` carries
   `images: Vec<Image>` — the picture crosses exactly as it will be
   journaled. `attachments` is removed: no client ever had one
   accepted. Reading a path is a surface's job because only a surface
   knows whose directory the path is in; the kernel does no file I/O
   for input. The one reader is the sdk brick
   `Image::read(path) -> Result<Image, ImageError>` (extension →
   media type, cap, base64), so the TUI, `--print` and any RPC client
   spell the read once.
3. **The kernel fails closed, then keeps everything.** `validate`
   refuses a media type outside the table and a payload over the cap
   with `InvalidInput`; an empty line with an image is a valid ask
   (the words are optional, the picture is the ask). The user item's
   parts are the text, if any, followed by the images in the order
   sent. Nothing downstream changes: `ContextView::fold` passes parts
   through, the vision projection already handles a model that cannot
   see, elision already counts image bytes.
4. **Each surface reads its own idiom.**
   - TUI: `@path` mentions naming a picture are read at submit,
     relative to the session's cwd. A path that does not read keeps
     the line in the composer and says why; nothing is sent. The
     transcript draws an image part as a marked chip on the person's
     bar (the line still holds the word).
   - `--print`: a repeatable `--image <PATH>` beside the prompt; in
     `--input-format stream-json` a user line's `image` blocks
     (Claude Code's `source: {type: base64, media_type, data}`) are
     the images, and an image-only line is a prompt.
   - RPC: nothing to build — the field is on the wire by serde
     (ADR-0007). A wire test pins the shape.
   - Feishu: an `image` message and the `img` runs of a `post` are
     fetched through the message-resource endpoint and become
     images; a caption-less picture is an ask with empty text.

## Consequences

- One representation of a handed-over picture from composer to
  journal to provider; a fourth surface adds a reader, never a shape.
- The wire gains one optional field and loses one that was never
  honoured; the RPC schema snapshot changes accordingly.
- The kernel stays I/O-free on input and cannot be made to read an
  arbitrary path by a remote RPC client — a client sends bytes it
  already holds.
- Documents (PDF) and audio are not images; when a provider door for
  them is wanted it is a new `ContentPart`, decided then.
- `base64` joins the sdk's dependencies (already in the workspace;
  no new crate in the lockfile).

Refs: ADR-0002, ADR-0007, ADR-0009, ADR-0016; Plan: M45
