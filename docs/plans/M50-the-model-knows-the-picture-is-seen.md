# M50 — The model knows the picture is seen

## Goal

Asked to show a picture, bingo today answers that it cannot display
images (observed 2026-09-04 in the user's TUI, right after M48). The
machinery exists: `Read` on an image file returns an image part, the
TUI draws that part in the transcript (M46 `pictured::read`), the
person pastes pictures the model sees (M45). What is missing is the
words the model reads: the `Read` description says only "images are
returned as images", the identity prompt never mentions pictures, and
`WebFetch` refuses every image by content type. This milestone changes
model-facing text and one tool, nothing in the kernel's types.

## Bricks

1. **The identity says it.** A `# Pictures` section in
   `bingo-core/src/prompt.rs` `IDENTITY` (cacheable; identical across
   sessions): the person can paste or attach pictures and the model
   sees them; a picture a tool returns is placed in the transcript for
   the person as well, where their surface can draw it; so the model
   must never say it cannot view or show an image — it reads the file
   or fetches the URL and describes what it sees. Four lines at most,
   in the prompt's existing voice. The prompt snapshot
   (`snapshots/bingo_core__prompt__tests__identity_is_cacheable…snap`)
   is reviewed and accepted with `cargo insta`/`INSTA_UPDATE`, and the
   diff pasted in Verified. Discipline rule 4c: the kernel names no
   tool — the sentence names no `Read` or `WebFetch`.
2. **`Read` says what happens to the picture.** `bingo-tool-fs/src/
   read.rs` `DESCRIPTION`: an image file (the sdk's `Image::extensions()`
   list, spelled from the table, not a second list) comes back as the
   picture itself, shown to the model and drawn for the person beside
   the call. A test asserts the description names every accepted
   extension so the two cannot drift.
3. **`WebFetch` fetches a picture.** `bingo-tool-web` gains
   `bingo-pictures` (a library-tier crate: any plugin may depend on it,
   ADR-0012 §1 / ADR-0041; the crate is already in the lock, budget
   stays 331). `body::render` grows a third kind: a body whose media
   type starts with `image/` is handed to `bingo_pictures::sniffed`
   (magic bytes decide, not the header) and comes back as
   `ContentPart::Image`; the `Accept` header lists `image/*` after the
   text types; pictures bypass the in-process cache (it stores text
   the model saw; a picture is bounded by `Image::MAX_BYTES` and
   `sniffed`'s own cap, so refetching is cheaper than a second cache);
   `DESCRIPTION` says an image URL comes back as the picture and that
   the person sees it too. The `Unreadable::ContentType` refusal stays
   for PDF and everything else. Wiremock tests: a `image/png` body
   returns an image part; a body claiming `image/png` that is not a
   picture is refused with `sniffed`'s error; a PDF is still refused.
   `body.rs` must not grow past one responsibility: the image arm calls
   into a new `picture.rs` in the crate.
4. **The print surface's word.** `--print` text mode renders an image
   tool result as a one-line placeholder already? Check
   `bingo-surface-print` (`stream_json.rs` emits `image_block`; the text
   mode may drop the part silently). If text mode is silent, one line
   `[image: image/png, N KiB]` — a note in Verified either way, no new
   surface work beyond that line.

## Files

`bingo-core/src/prompt.rs` (+ its snapshot); `bingo-tool-fs/src/read.rs`;
`bingo-tool-web/{Cargo.toml,src/{fetch.rs,body.rs,picture.rs}}`;
`docs/adr/0041-the-picture-from-anywhere.md` gains a dated line under
its consequences (the web tool reads pictures through the library);
`bingo-surface-print` only per brick 4.

## Exit criteria

- [ ] The identity prompt tells the model pictures it returns are seen
  by the person; snapshot accepted and pasted.
- [ ] `Read`'s description names the accepted extensions from the
  table; a test pins it.
- [ ] `WebFetch` on an `image/*` URL returns an image part, sniffed;
  non-pictures and PDFs still refused; `Accept` lists `image/*`.
- [ ] Every AGENTS.md gate; budget 331; `cargo deny`; discipline
  (rule 4c: no tool name in the kernel's prompt).
- [ ] Hands-on after merge (main session): in the TUI, ask bingo to
  show `docs/…/some.png` and a public image URL; it reads/fetches and
  the transcript draws the picture without a disclaimer.

## Non-goals

A per-surface capability the kernel plumbs ("this surface can draw")
— the prompt says "where the surface can draw it", which is true of
every surface today (TUI draws, print emits the block, Feishu is M45's
inbound only). Sending pictures *out* through Feishu. GIF animation.
A picture in `Bash` output. Changing `Image`'s table.

## Risks

- The identity block changes → every session's cached prefix misses
  once. Accepted; it is one block.
- `sniffed` transcodes wide formats to PNG (ADR-0041 §2); a 10 MiB
  fetch cap and `Image::MAX_BYTES` both apply; the smaller wins and
  the error says which.
- An `image/svg+xml` body is text the decoder does not read: it stays
  `Kind::Text` (starts with `image/` but `+xml` — order the match so
  `+xml` wins, and test it).
