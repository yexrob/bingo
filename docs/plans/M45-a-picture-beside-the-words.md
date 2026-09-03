# M45 — A picture beside the words

## Goal

A person hands the model a picture from every surface that has a
person on it: the TUI (`@shot.png`), `--print` (`--image PATH` and
stream-json image blocks), the RPC wire (the `images` field), and
Feishu (an image message, a post with pictures). The picture reaches
the journal as the `ContentPart::Image` the providers already encode,
and a model without vision still gets the existing note. ADR-0040.

## Bricks

Three slices; the first lands before the other two start.

**K — kernel and sdk** (branch `m45-kernel`)
1. `bingo_sdk::model::Image { media_type, data }` with
   `ContentPart::Image(Image)`; `Image::MEDIA_TYPES` (`png jpg jpeg
   gif webp` → types), `Image::MAX_BYTES` (5 MiB decoded),
   `Image::media_type_of(path)`, `Image::read(path)` (tokio-free, std
   fs is fine — a surface calls it off its own thread or accepts the
   blocking read), `Image::from_bytes(media_type, bytes)`. Errors as a
   `thiserror` enum. `frames.snap` proves the journal bytes are
   unchanged; a round-trip test pins `{"type":"image","mediaType",
   "data"}`.
2. `Input::Text { text, images: Vec<Image>, origin }` with
   `#[serde(default, skip_serializing_if = Vec::is_empty)]`;
   `attachments` deleted; every `Input::Text` site (tui `input.rs`,
   plugin-rpc stub hook, tests) follows. `Input::text` unchanged.
3. Kernel: `validate` accepts an empty line with images, refuses an
   unknown media type or an oversized payload (`InvalidInput`, the
   message names which); `journal_prose` takes the images and writes
   text-then-images (text omitted when empty); the queue preview of
   an image-only ask is `(an image)`; the title minted from a first
   ask that is only a picture falls back as an empty line does today.
4. `bingo-tool-fs/src/read.rs` uses `Image::media_type_of` and
   `Image::from_bytes`; its own table goes.
5. RPC: `tests/wire` submits an input with one image and sees the
   user item's second part be that image; the schema snapshot is
   regenerated and read for surprises.

**T — TUI and print** (branch `m45-tui-print`, after K)
6. TUI: at `Effect::Submit`, the `@` image mentions are read through
   `Image::read` relative to the session's cwd (`run.rs` has it via
   the tree root); a failed read leaves the composer as it was and
   raises the existing notice kind with the path and the reason. The
   `attachments` derivation in `complete.rs` becomes the list of
   paths to read; its extension list is `Image::MEDIA_TYPES`' keys.
7. TUI transcript: `said.rs` draws an image part as one chip on the
   person's bar — a marked span, `▣ image/png`, in the same style the
   design uses for a tool row's marks — after the text lines. A
   `TestBackend` test and the PTY smoke.
8. `--print`: `--image <PATH>` (repeatable, text mode) read through
   `Image::read` at the binary edge, an unreadable path is exit 1
   with the reason on stderr and nothing on stdout; stream-json
   `user` lines: `image` blocks become images, an image-only line is
   a prompt. Black-box: a `--print` run with `--image` against the
   fake provider journals an image part; the stream-json input test
   gains an image line.

**C — Feishu** (branch `m45-channels`, after K, beside T)
9. `feishu/api.rs` grows one bytes-returning `get` for
   `/open-apis/im/v1/messages/{message_id}/resources/{file_key}?type=
   image` (bearer as today; a non-2xx is an `ApiError`).
10. `feishu/event.rs`: an `image` message yields `Incoming::Message`
    with empty text and one image key; a `post` yields its text and
    its `img` runs' keys in order. Fetching happens where the message
    is turned into an `Input` (the runner side has the api); a key
    that does not fetch is dropped with a notice, the text still goes.
    `Image::from_bytes` builds the part; the content type from the
    response header names the media type, the table refuses the rest.
11. Tests: wiremock serves a PNG for the resource path; an image
    message becomes an `Input::Text` with empty text and one image;
    a post with two pictures keeps their order; a 404 drops that
    picture and delivers the words. The M13 "left alone" test turns
    into the positive one; audio and files remain left alone.

## Files

`bingo-sdk/src/{model.rs,host.rs,lib.rs,Cargo.toml}`;
`bingo-core/src/session.rs`, `session/{queue.rs,title.rs}`, tests;
`bingo-tool-fs/src/read.rs`; `bingo-surface-rpc/{src/schema.rs,
tests/wire}`; `bingo-plugin-rpc/examples/stub_plugin/hooks.rs`;
`bingo-surface-tui/src/{input.rs,complete.rs,run.rs,transcript/
said.rs}`; `bingo/src/main.rs`; `bingo-surface-print/src/input.rs`;
`bingo-channels/src/feishu/{api.rs,event.rs}` and the runner;
`docs/adr/{0040,README}.md`.

## Exit criteria

- [ ] `frames.snap` unchanged by the `Image` extraction.
- [ ] Kernel: empty text + image is accepted; bad media type and
  oversize are `InvalidInput`; the user item carries text then images.
- [ ] Wire: a submit with an image round-trips to the journal.
- [ ] TUI: `@shot.png` sends an image part; a missing file keeps the
  line and says so; the chip draws (`TestBackend`); PTY smoke green.
- [ ] `--print --image` journals an image; stream-json image line too.
- [ ] Feishu: image message, post with pictures, and a failed fetch —
  each as specified, under wiremock.
- [ ] Every AGENTS.md gate; no new lockfile crate; Windows
  cross-check for `bingo-sdk` and `bingo` (a path is read).

## Non-goals

Pasting a picture from the clipboard into the TUI (no portable
clipboard-image path without a dependency; recorded). Sending a
model's picture back out to Feishu. PDF and audio. Resizing or
re-encoding on the way in.

## Risks

- The `ContentPart::Image` refactor touches fifteen match sites; a
  mechanical change, guarded by the fixture.
- Feishu's resource endpoint needs `im:resource` scope; a tenant
  without it sees the notice, not a failure of the words.
- Base64 in the journal grows sessions; elision already counts it,
  and the cap bounds one picture.
