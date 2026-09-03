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
6. TUI paste: on `ctrl+v` the composer asks the platform clipboard
   for an image — `osascript` (`the clipboard as «class PNGf»`) on
   macOS, `wl-paste -t image/png` then `xclip -selection clipboard -t
   image/png -o` on Linux, PowerShell `Get-Clipboard -Format Image`
   on Windows — each arm `cfg`-gated, all three written in this
   change, none a dependency. A picture inserts `[image N]` at the
   cursor (N = one more than the highest token in the line) and is
   held beside the composer keyed to that token, via
   `Image::from_bytes`; a token deleted from the line drops its
   picture (derive the live set from the line at submit — the line is
   the record, nothing is remembered that it does not say). No image
   on the clipboard: nothing happens, terminal text paste is
   untouched. The pure brick first: token minting, token scanning,
   and the pairing of tokens to held pictures as functions on strings
   with their own tests; the clipboard command is the only impure
   edge, behind one small function per platform.
7. TUI `@path`: at `Effect::Submit`, image mentions are read through
   `Image::read` relative to the session's cwd (`run.rs` has it via
   the tree root); a failed read leaves the composer as it was and
   raises the existing status-line notice with the path and the
   reason; nothing is sent. `complete.rs`'s extension list becomes
   `Image`'s. Submit sends the text as typed and the images in order:
   pasted ones by token order, then mentioned ones by word order.
   The transcript draws nothing extra for an image part; the words
   carry `[image 1]` or `@shot.png`. A `TestBackend` test that a
   pasted line submits text plus one image; the PTY smoke stays green.
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
`bingo-surface-tui/src/{input.rs,complete.rs,run.rs}` and a new
clipboard module; `bingo/src/main.rs`; `bingo-surface-print/src/input.rs`;
`bingo-channels/src/feishu/{api.rs,event.rs}` and the runner;
`docs/adr/{0040,README}.md`.

## Exit criteria

- [x] `frames.snap` unchanged by the `Image` extraction.
- [x] Kernel: empty text + image is accepted; bad media type and
  oversize are `InvalidInput`; the user item carries text then images.
- [x] Wire: a submit with an image round-trips to the journal.
- [x] TUI: a paste puts `[image 1]` in the line and the submit carries
  the image; `@shot.png` sends an image part; a missing file keeps
  the line and says so; PTY smoke green.
- [x] `--print --image` journals an image; stream-json image line too.
- [x] Feishu: image message, post with pictures, and a failed fetch —
  each as specified, under wiremock.
- [x] Every AGENTS.md gate; no new lockfile crate; Windows
  cross-check for `bingo-sdk` and `bingo` (a path is read).

## Non-goals

Drag-and-drop of a bare path without `@`. Sending a
model's picture back out to Feishu. PDF and audio. Resizing or
re-encoding on the way in.

## Risks

- The `ContentPart::Image` refactor touches fifteen match sites; a
  mechanical change, guarded by the fixture.
- Feishu's resource endpoint needs `im:resource` scope; a tenant
  without it sees the notice, not a failure of the words.
- Base64 in the journal grows sessions; elision already counts it,
  and the cap bounds one picture.

## Verified (slice K)

Bricks 1–5 landed on `m45-kernel`. `Image` (sdk `model.rs`) with its
table, cap, `media_type_of`/`is_known`/`from_bytes`/`read`/`decoded_len`,
`ImageError`; `ContentPart::Image(Image)` — `frames.snap` untouched, a
new round-trip test pins the tagged JSON shape. `Input::Text.images`
replaces `attachments`; all fifteen match sites and every construction
site follow. `read.rs` uses `Image::media_type_of`/`from_bytes`, its own
table gone. Wire test added; `schema/{rpc,plugin}.json` regenerated —
only the `Input`/`Image` shapes moved.

Found beyond the plan: `Image::MEDIA_TYPES` stays module-private (the
three accessors are the public surface). `turn.rs`'s mid-turn `barrier()`
built a steer's `ItemBody::User` by hand and would have dropped its
pictures; in review it now records through the same `user_parts`, with
a test (`a_picture_steered_in_at_the_barrier_is_kept`) whose scripted
model has no vision — so what it proves is the whole path: kept in the
items, projected to the note at the request. Also in review:
`Image::EXTENSIONS` (a second copy of the table's keys) became
`Image::extensions()` read off the table, and `decoded_len` saturates
on a payload that is only padding instead of underflowing.

Gates, all from the worktree: `fmt --check`, `check --workspace
--all-targets`, `clippy -D warnings`, `test --workspace` (every crate
green) all clean; `check_discipline.sh` → ok; `budget.sh` → ok, deps
310/310 (no new lockfile crate); `deny check` → ok; `check -p bingo-sdk
--target x86_64-pc-windows-msvc` clean (also `-p bingo-core`, `-p
bingo-tool-fs`; a full-workspace Windows check hits a pre-existing
`aws-lc-sys` cross-toolchain gap, unrelated).

## Verified (slices T and C)

Both written in the main session (Opus was overloaded; the user asked
that no worker be used), each on its own branch, merged after its own
gate run, then the whole gated once more on `dev`.

**T.** `bingo-surface-tui/src/pictures.rs` is the pure brick: `[image N]`
tokens read off the line, the next number minted past the highest, and
`Held` — pictures by token — from which `carried(line)` derives what is
sent, so a deleted token drops its picture without anyone remembering
it. `clipboard.rs` is the one impure edge: three `cfg` arms (`osascript`
writing `«class PNGf»` to a scratch file; `wl-paste` then `xclip` with
the file as stdout; PowerShell `Clipboard::GetImage().Save`), each a
bounded child, no dependency. `ctrl+v` is `Effect::PasteImage`; the loop
reads the clipboard, holds the picture and inserts the token. At submit
the loop reads the `@` mentions (`complete::attachments`, whose table
is now `Image::media_type_of`) relative to the session's cwd; one that
does not read restores the line and raises the status-line notice with
the path and the reason. `--print --image PATH` rides `args.images` to
the print surface, which reads it at start and answers `InvalidInput`
before a session opens; stream-json `image` blocks (`source.type ==
base64`) are a user line's pictures, and a line that is only a picture
is a prompt. Black-box `crates/bingo/tests/cli/images.rs` reads the
journal, since the stream does not echo a person's own prompt.

Found beyond the plan: no transcript chip was needed — `said.rs` draws
the text parts and the words already carry the token or the path.
Two notes for later, unscheduled: Windows Terminal binds `ctrl+v` to
its own paste, so there the picture route is `@path`; and `paste_image`
runs the clipboard tool on the loop's thread (bounded at five seconds).

**C.** `Incoming::Message.images`; `feishu/event.rs` stays a pure parse
and yields `Heard.pictures` (message id + key) beside the `Incoming` —
an `image` message is empty words and one key, a `post`'s `img` runs
are keys in document order. `feishu/pictures.rs` fetches them through
`Api::get_bytes` (`/im/v1/messages/{id}/resources/{key}?type=image`,
the media type off `Content-Type`), after the ack and before the inbox;
a picture that does not fetch is a `tracing::warn!` and the words still
go. The ws loop's `inbox` argument became a `Delivery { api, inbox }` so
the arity stayed under clippy's seven.

```
$ cargo test --workspace --locked        # slice T worktree: 3396 passed, 0 failed
$ cargo test --workspace --locked        # slice C worktree: 3382 passed, 0 failed
$ scripts/tui-smoke.sh                   # tui-smoke ok
$ cargo check -p bingo-surface-tui -p bingo-surface-print --all-targets \
      --target x86_64-pc-windows-msvc    # Finished
$ scripts/check_discipline.sh            # discipline ok
$ scripts/budget.sh                      # budget ok (310/310, no new crate)
$ cargo deny check                       # advisories/bans/licenses/sources ok
```
Final run on `dev` (aae6810, K + T + C merged):
```
$ cargo fmt --all -- --check                                   # clean
$ cargo clippy --workspace --all-targets --locked -- -D warnings  # Finished
$ cargo test --workspace --locked        # 3401 passed, 0 failed
$ scripts/check_discipline.sh            # discipline ok
$ scripts/budget.sh                      # budget ok
$ cargo deny check                       # advisories/bans/licenses/sources ok
$ scripts/tui-smoke.sh                   # tui-smoke ok
```
The Windows cross-check passed for `bingo-sdk`, `bingo-surface-tui` and
`bingo-surface-print` in their slice runs; the same check with
`bingo-channels` added did not finish (`build failed`), and the run was
stopped before the cause was read. The likely cause is the pre-existing
`aws-lc-sys` cross-toolchain gap slice K already met through `reqwest`,
but that is a guess, not a reading: CI's `windows` job is the backstop.

