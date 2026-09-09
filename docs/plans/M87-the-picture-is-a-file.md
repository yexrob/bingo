# M87 — The picture is a file

## Goal

User, 2026-09-09, from session `ses_01M22J277HYZ902AZX9R4X5F7M`: a pasted
picture reached the model as bytes and nothing else, so a skill that takes
`--image <path>` sent the model hunting the disk for a file that was never
written, and it ended up digging the base64 out of the journal. The rule
after this milestone: **a picture a person hands in is a file on their
machine before it is anything else**, and the path travels with the picture.
`[image N]` stays exactly as it is in the line; the path rides on the image
part, and a provider says it beside the picture. One way in — from a path
into memory — for a paste, an `@word`, and a chat attachment alike
(ADR-0052).

## Bricks, in build order

1. `bingo_sdk::Image` gains `path: Option<PathBuf>` (serde `default`,
   skipped when `None`: the journal bytes of every picture without one are
   unchanged, fixtures prove it). `Image::read` sets it. `Image::extension_of`
   is the table read the other way. `Image::whereabouts() -> Option<String>`
   is the one spelling of "this picture is the file …" a provider writes.
2. `bingo_pictures::file`: `written(path, bytes)` — the directory made, a
   temporary name, a rename — is the one atomic writer; `cache.rs` and the
   Feishu attachments call it instead of their own. `hashed(bytes) -> u128`
   moves beside it. `bingo_pictures::keep(dir, &Image) -> io::Result<PathBuf>`
   writes a picture as `<dir>/<hash>.<ext>` and says where.
   `load(Source::Path)` hands back an image that knows its path.
3. Providers: an image part with a path is followed by its whereabouts as a
   text part — OpenAI `input_text`, Anthropic `text` block. Snapshot each. A
   model that cannot see is told the path in place of the picture
   (`models/vision.rs`). The `Read` tool's picture knows its path.
4. TUI: `pictures::Held` holds a path per token, nothing else. A paste is
   written under `<data>/pictures/pasted/` at once and the token names that
   file. At submit the tokens' paths and the `@word`s are one list of
   sources read on one task (`submit::read_all`); `input.rs` no longer
   touches pictures. The strip and a click read a draft's picture through
   the `Linked` memo, which already reads a path once and keeps it: a
   draft's paths join `wanted`. The viewer opens a picture that has a path
   where it is. A withdrawn line's pictures come back as their paths.
5. Feishu: a picture lands under `<files>/<message_id>/` like every other
   attachment, through brick 2, and reaches the journal with its path.
6. Prompt: the Pictures block says a pasted or attached picture is a file
   whose path is written beside it, and that path is what a tool takes.

## Files

- `bingo-sdk/src/model.rs`; every `Image { media_type, data }` pattern
  gains `..` (core, mcp, print, tui, providers, tool-fs, pictures).
- `bingo-pictures/src/{file.rs,cache.rs,load.rs,lib.rs}`.
- `bingo-provider-openai/src/input.rs`, `bingo-provider-anthropic/src/request.rs`,
  `bingo-core/src/models/vision.rs`, `bingo-core/src/prompt.rs` (+ snapshot),
  `bingo-tool-fs/src/read.rs`.
- `bingo-surface-tui/src/{pictures.rs,run.rs,input.rs,viewer.rs}`,
  `run/{submit.rs,withdraw.rs,showing.rs}`, `composer/strip.rs`,
  `graphics/picture.rs`.
- `bingo-channels/src/feishu/attachments.rs`.
- `docs/adr/0052-the-picture-is-a-file.md`, ADR-0040 status line, ADR README.

## Exit criteria

- [x] frames fixture unchanged for a picture without a path; a picture
      with one round-trips through the journal
- [x] OpenAI and Anthropic requests: image, then its whereabouts (asserted
      block by block, no new snapshot)
- [x] TUI: a paste writes the file and the token names it (tempdir test);
      submit reads tokens and `@word`s in line order on one task; a withdrawn
      line restores the paths; strip `TestBackend` test still draws the
      thumbnail; click opens the pasted file itself
- [x] Feishu: a picture lands under the message's directory and the journal
      image carries the path (wiremock test)
- [x] prompt snapshot updated; every gate green; Windows check for
      `bingo-sdk`, `bingo-core`; [ ] `bingo-pictures`, `bingo-surface-tui`
      (`aws-lc-sys` under `reqwest` will not cross-build on this Mac, as in
      M86 — CI's `windows` job is the check)
- [x] tmux hands-on: paste a picture, ask the model for its path, it answers
      the path without a search

## Non-goals

- No sweep of `pictures/pasted/`: the journal keeps the bytes, so a file
  gone is a file the viewer writes again; a sweep is decided when the
  directory is a problem someone has.
- An RPC or ACP client that sends bytes inline gets no path: it had no file.
- No change to `[image N]`, to `Input::Text`, or to the kernel.

## Risks

- `Image` is on the wire and in every fixture: the field is optional and
  skipped, so no recorded frame changes; the RPC schema snapshot does.
- The strip reads a draft through `Linked`, bounded at `KEPT` destinations:
  a draft past that bound sends its picture and shows no thumbnail.
- The pasted file is the picture the model is told about; a person who
  deletes it under a running session gets a tool error, not a lost turn.

## Verified (2026-09-09, dev, before commit)

```
cargo fmt --all -- --check                                    ok
cargo check --workspace --all-targets --locked                ok
cargo clippy --workspace --all-targets --locked -- -D warnings ok
cargo test --workspace --locked                               ok (schema/plugin.json regenerated: `path` on Image)
scripts/check_discipline.sh                                   discipline ok (plan-length warnings are history)
scripts/budget.sh                                             budget ok
cargo check -p bingo-sdk  --target x86_64-pc-windows-msvc     ok
cargo check -p bingo-core --target x86_64-pc-windows-msvc     ok
```

tmux drive (`target/debug/bingo`, Road/gpt-6-astra): `ctrl+v` with a PNG on
the clipboard put `[image 1]` in the line and wrote
`~/.bingo/data/pictures/pasted/9ffba564fa7f7d821b9ff44b4fb68fe9.png`
(51061 bytes, the clipboard's own). Asked "reply with only the absolute file
path of the picture I pasted … do not run any tool", the model answered that
path in 7 s with no tool call. The session's journal carries the image part
as `{"type":"image","mediaType":"image/png","data":…,"path":"/Users/…/pasted/9ffb….png"}`.
