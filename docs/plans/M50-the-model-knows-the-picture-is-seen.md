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

- [x] The identity prompt tells the model pictures it returns are seen
  by the person; snapshot accepted and pasted.
- [x] `Read`'s description names the accepted extensions from the
  table; a test pins it.
- [x] `WebFetch` on an `image/*` URL returns an image part, sniffed;
  non-pictures and PDFs still refused; `Accept` lists `image/*`.
- [x] Every AGENTS.md gate; budget 331; `cargo deny`; discipline
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

## Verified

*2026-09-04, worktree `.claude/worktrees/m50` on `dev`, base 251cb50.*

### What landed

1. **The identity says it.** A `# Pictures` section, three bullets, last in
   `IDENTITY`: the user may paste or attach a picture and the model sees it; a
   picture a tool returns also goes into the transcript, where the user's
   surface can draw it; so the model never says it cannot view or display an
   image — it reads the file or fetches the URL and says what it sees. No tool
   is named (discipline 4c passes). The block is pinned by a new snapshot
   (below).
2. **`Read` says what happens to the picture.** `DESCRIPTION` is now
   `description()`, formatted from `extensions()` — `Image::extensions()`
   joined, so the sentence *is* the sdk's table and not a second list. The
   rendered text, printed from the spec:

   > Read a file from the filesystem. Give an absolute path, or one relative
   > to the session's working directory. Text is returned with line numbers,
   > starting at line 1; use `offset` and `limit` to read a window of a long
   > file. A picture (.png, .jpg, .jpeg, .gif, .webp) comes back as the
   > picture itself: you see it, and it is placed in the user's transcript
   > beside this call, where their surface can draw it. Long results are
   > truncated, and say so on the last line.

   `the_description_names_every_extension_a_picture_may_arrive_under` walks
   the table and fails if the sentence drops one.
3. **`WebFetch` fetches a picture.** `bingo-tool-web` depends on
   `bingo-pictures` (library tier; the lock gained one edge, no package, and
   the budget is unchanged at 331). `body::render` takes bytes now and answers
   with `Content::Page(String)` or `Content::Picture(Image)`; `Kind::Picture`
   is `image/…` *after* the `+xml` arm, so `image/svg+xml` stays text.
   `picture.rs` owns the picture in a fetch: `seen(bytes)` hands them to
   `bingo_pictures::sniffed` (magic bytes, not the header) and `output(image)`
   is the one-part `ToolOutput`. `retrieve` returns that output; only text is
   cached, so a picture is refetched rather than held twice. `ACCEPT` lists
   `image/*;q=0.8` after the document types (`*/*` drops to `q=0.7`).
   5 new tests: a served PNG comes back as the `Image` byte for byte and the
   cache stays empty (the mock expects, and gets, two requests); a page served
   as `image/png` is refused with `sniffed`'s "not a picture"; an SVG is still
   text; a PDF is still refused by name; `ACCEPT` asks for pictures after the
   documents.
4. **The print surface.** No change — see the finding.

### What the plan got wrong

- **The snapshot it named pins the `<env>` block, not the identity.**
  `identity_is_cacheable_and_the_env_block_is_not` snapshots `blocks[1].text`;
  the identity text had no snapshot at all, so a change to it was invisible to
  the suite. Brick 1 added one — `the_identity_is_the_same_words_for_every_session`,
  `blocks[0].text` — rather than accepting a diff to a file that never held
  these words.
- **`body::render` had to take `&[u8]`, not `&str`.** `sniffed` decides on the
  bytes; the text arms lose nothing (`String::from_utf8_lossy` moved into
  `body::text`).
- **`picture.rs` holds two functions, not the image arm alone.** A module whose
  only content was a one-line delegation to `bingo_pictures::sniffed` would be
  a second name for one thing, so it also owns the `ToolOutput` a picture
  becomes; `fetch.rs` keeps the cache decision and `body.rs` keeps the
  classification.

### Brick 4's finding

`--print` text mode does not drop the image part: it renders **no** tool-result
content at all. `render.rs::tool_done` writes the verdict line
(`[tool] Read ok (12ms)`) and, if there is one, the `display` view's fold; a
`ToolOutput`'s `parts` are never read in `Mode::Text`, for text results as much
as for pictures. So an `[image: …]` line would be the only tool-result content
that mode ever printed, a special case for one part kind, and a third place
spelling the degrade the TUI already owns. Nothing added. `stream-json` is
unchanged and already correct: `tool_content` switches to the block-array form
when a part is an image, so a `WebFetch` picture reaches a host the same way a
`Read` picture does.

### The snapshot

New file
`crates/bingo-core/src/snapshots/bingo_core__prompt__tests__the_identity_is_the_same_words_for_every_session.snap`
(accepted with `INSTA_UPDATE=always`). It is the whole identity block; the
words this milestone added are its last four lines:

```
# Pictures
- You can see images: the user may paste or attach one, and a tool that returns a picture puts it in front of you.
- A picture a tool returns also goes into the transcript, where the user's surface can draw it — reading a picture is how you show it to them.
- Never say you cannot view or display an image: read the file or fetch the URL, then say what you see.
```

No other snapshot changed.

### The gates

```
$ cargo fmt --all -- --check                          # silent, exit 0
$ cargo check --workspace --all-targets --locked -j 2
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 56.00s
$ cargo clippy --workspace --all-targets --locked -j 2 -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 35.74s
$ cargo test --workspace --locked -j 2 | tee target/m50-test.log
    79 result lines, 0 not ok; 3534 passed, 0 failed (no flake hit)
$ scripts/check_discipline.sh
    kernel names no tool / cohesion ok / discipline ok      (pre-existing warns only)
$ scripts/budget.sh
    dependencies (unique, normal): 331 (max  331)
    warm cargo check -p bingo-core: 0s (max  20s)
    relink isolation: touching the TUI recompiled 0 crates for core (must be 0)
    budget ok
$ cargo deny check
    advisories ok, bans ok, licenses ok, sources ok
$ cargo check -p bingo-tool-fs -p bingo-core --all-targets --locked \
      --target x86_64-pc-windows-msvc
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 16.48s
```

`bingo-tool-web` has no local Windows cross-check: `aws-lc-sys`, which
`reqwest`'s rustls pulls in, fails in its own build script on this machine
(`error occurred in cc-rs`, the jitterentropy translation unit) — the M47 note
in ADR-0041's consequences, unchanged by this milestone, since the crate
already depended on `reqwest`. CI's `windows` job is the backstop.
