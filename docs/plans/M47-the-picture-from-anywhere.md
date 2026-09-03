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

- [x] `@shot.bmp` and `@https://…/x.jpg` reach the journal as png /
  jpeg `Image` parts; a missing URL keeps the line and says so.
- [x] `--print --image <url>` journals the picture; over-cap and 404
  are exit 1, stdout empty.
- [x] Feishu BMP arrives as PNG.
- [x] No lockfile crate beyond M46's; every AGENTS.md gate. **Not** the
  Windows cross-check for `bingo-pictures` and `bingo-surface-tui`:
  `reqwest` puts `aws-lc-sys` in both trees and it will not cross-build
  from macOS. See "What is not verified".

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

## Verified

### What landed

The six bricks, with three shapes different from the plan (below).

1. **`Source`** — `bingo-pictures/src/source.rs`. `Source::parse(word,
   cwd)`: a word whose first bytes are `http://`/`https://`
   (case-insensitively, and read through `str::get` so a multi-byte word
   cannot panic on the slice) is a `Url`, everything else a `Path` joined
   to `cwd`. Beside it `names_a_picture(word)` — the predicate that makes
   an `@word` an attachment: any URL, or a path whose extension
   `image::ImageFormat::from_extension` knows. Pure; 6 tests.
2. **`load`** — `bingo-pictures/src/load.rs`, one `async fn` and no sync
   twin (see below). A path is `std::fs::metadata` for the cap and then
   `std::fs::read`; a URL is `reqwest` with a 30 s client timeout, its
   `Content-Length` refused before the body is asked for and the body cut
   at `Image::MAX_BYTES` as it streams. Both caps are the same
   `refuse_over`, which answers in `bingo_sdk::ImageError::TooLarge`'s own
   words — one table, one cap, one error. 10 tests, five of them against
   wiremock.
   The widening is `bingo-pictures/src/accepted.rs`, a fourth module the
   plan does not list: `sniffed(bytes)` reads the format off the bytes
   with `image::guess_format`, maps it through `ImageFormat::to_mime_type`
   and asks `Image::is_known` — in the table it is handed over as it came,
   otherwise decoded and re-encoded as PNG; nothing recognised is
   `PictureError::NotAPicture`. `accepted(image)` is the same rule for a
   picture already in the `Image` shape. 6 tests.
3. **The TUI.** `complete::attachments` filters on
   `bingo_pictures::names_a_picture`; its own `is_image` and the sdk table
   it read are gone. The `@` dropdown is untouched and still offers files
   only — a URL is typed, not completed, and a test asserts no row
   contains `://`. `run.rs` no longer reads a mention on the loop thread:
   `submit` splits into `submit_text` (mentions? spawn : send),
   `Reply::Mentioned` / `Run::mentioned` (the reply arm) and `send_text`
   (history, held pictures, mint, submit). The reading itself is the free
   `async fn read_mentions`. 5 tests, two against wiremock.
4. **`--print --image`** takes a path or a URL: `images_from` goes through
   `Source::parse` + `load`, `start` and `drive` became `async` for it,
   and a relative path is now resolved against the session's `cwd`.
   Stream-json `image` blocks go through `accepted`, so a host may hand
   over a BMP and the journal still holds a PNG. 5 tests here, 5
   black-box in `crates/bingo/tests/cli/images.rs`.
5. **Feishu.** `pictures::one` hands the bytes to `sniffed`, and
   `Api::get_bytes` no longer returns a `Content-Type` nobody reads. 3
   tests: a BMP and a TIFF arrive as PNG, a PNG and a JPEG arrive as they
   came *under a header that said png for both*, and bytes no decoder
   reads are still dropped with the words going on.
6. **Tests.** 29 in `bingo-pictures` (13 new bricks + the 16 M46 left),
   4 new in `complete.rs`, 5 in `run.rs`, 3 in print's `lib.rs`, 2 in
   `input.rs`, 3 in Feishu's `pictures.rs`, 5 new black-box in
   `cli/images.rs`. Workspace total 3454 → 3493.

### What the plan got wrong

- **There is no sync `load`, and the TUI does not want one.** The plan
  offered "give both, one over the other". A blocking fetch on the loop
  thread is thirty dropped frames whatever it is called, and
  `Handle::block_on` inside a runtime is a panic. So `load` is async only
  and the TUI reaches it the way it already reaches `ListSessions` and
  `Open`: `Run::spawn` puts the read on a task and it comes back as a
  `Reply` the reducer folds. The cost is a hop the sync version did not
  have — the line leaves the composer at the key press and comes back if
  a mention fails — and that is the honest shape: nothing can know a URL
  is not there until it has asked.
- **`Content-Type` is not evidence, so Feishu does not read it either.**
  Brick 5 said to key on the header and call `to_png` when it is outside
  the table. That leaves a server able to journal a BMP labelled
  `image/png`, which is exactly the picture no provider can replay
  (ADR-0041 §2). One rule for raw bytes — sniff, then widen — is spelled
  once in `sniffed` and used by the loader and the channel alike. The two
  Feishu tests that proved the old behaviour with `b"png-a"` and `b"tiff"`
  now serve real pictures under a header that lies, which is a stronger
  assertion, not a weaker one.
- **Widening needs two entry points, not one.** Raw bytes are sniffed;
  an `Image` that a host already base64'd is not, because re-encoding an
  in-table block would rewrite bytes the host chose (and the existing
  `"iVBOR"` fixture is not decodable base64 at all). `accepted(image)`
  keeps a table type untouched and only decodes what is wider.
- **`Source::parse` is not enough for the composer.** Whether a word *is*
  a mention is a different question from where it points, and
  `complete::attachments` needed the first. `names_a_picture` is that
  question, in the same module, so the TUI spells no table of its own.
- **`--image` was `Vec<PathBuf>`.** A URL is not a path; it is
  `Vec<String>` now, which also drops a lossy `to_string_lossy` on the
  way into `SurfaceOptions::args`.
- **`bingo-pictures` is four modules, not three.** `accepted.rs` owns
  "the type a provider accepts"; `load.rs` owns "bytes from where they
  live". One noun each was worth the extra file.
- **ADR-0041 §1's parenthetical is now stale** — it says the crate
  "depends on `bingo-sdk` and `image` only", while §3 of the same record
  requires reqwest. The plan says §1 stays as written, so it does; the
  contradiction is noted here rather than edited in.

### What is not verified

- **The Windows cross-check could not be run for either crate.**
  `reqwest` pulls `rustls` → `aws-lc-rs` → `aws-lc-sys`, whose build
  script compiles C against `windows.h` for the target; there is no
  Windows SDK on this machine, so `cargo check -p bingo-pictures -p
  bingo-surface-tui --target x86_64-pc-windows-msvc` dies in that build
  script before rustc sees a line of this repository's source. The same
  gap is on record from M45 (`bingo-channels`). The target toolchain
  itself is fine — `cargo check -p bingo-sdk --target
  x86_64-pc-windows-msvc` still finishes — so what is lost is local
  coverage, not portability: this milestone adds no `cfg`, no signal, no
  process, no path-length and no clock assumption, and every API it
  reaches for (`std::fs::metadata`, `std::fs::read`, `Path::join`,
  `Path::extension`, reqwest, image) is portable. CI's `windows` job
  (`cargo test --workspace --locked` on `windows-latest`) is the backstop
  — and note that M46's local Windows check of `bingo-surface-tui` cannot
  be repeated after this change.
- **"Refused at the header, before the body"** is proven structurally,
  not through a served response: `refuse_over` has its own unit test at
  the cap and one above it, and both call sites use it. Which of the two
  refuses a real over-cap response depends on how the socket chunked it,
  and an assertion on that would pin this machine (AGENTS.md).
- **No real network.** Every URL in these tests is wiremock on the
  loopback. Redirects, TLS, a slow server hitting the 30 s timeout,
  and a `Content-Length` that lies are all unexercised.
- **The hop has two windows the sync version did not have.** While a
  mention is being read the composer is empty, so a second `enter` can
  submit again; and `Ui::pictures.clear()` fires when the send goes, so a
  picture pasted *during* a fetch is dropped with the ones that were sent.
  Both are seconds wide and neither is tested.
- **`bingo_sdk::Image::read` now has no caller outside its own tests.**
  ADR-0041's consequences keep it deliberately ("the sdk's `Image::read`
  stays the table-only path reader"), so it stays — but it is the one
  remaining reader that trusts an extension, and a future surface reaching
  for it would be reaching for the wrong thing.
- **`run.rs` is 899 → 966 non-test lines**, still a warning but 34 from
  the 1000-line failure. The next change there should split it.

### Gates, all from the worktree

```
$ cargo fmt --all -- --check                                    # clean
$ cargo check --workspace --all-targets --locked                # Finished
$ cargo clippy --workspace --all-targets --locked -- -D warnings   # Finished
$ cargo test --workspace --locked                # 3493 passed, 0 failed
$ scripts/check_discipline.sh                                   # discipline ok
$ scripts/budget.sh    # dependencies (unique, normal): 331 (max 331); budget ok
$ cargo deny check                 # advisories ok, bans ok, licenses ok, sources ok
$ scripts/tui-smoke.sh                                          # tui-smoke ok
$ cargo check -p bingo-pictures -p bingo-surface-tui --all-targets --locked \
      --target x86_64-pc-windows-msvc
                    # FAILS in aws-lc-sys' build script: 'windows.h' not found
$ cargo check -p bingo-sdk --all-targets --locked \
      --target x86_64-pc-windows-msvc                           # Finished
```
No known flake was hit. `Cargo.lock` gains ten lines and no package: the
crate list is byte-identical before and after (`grep '^name = '` diffed
empty), which is what "no lockfile crate beyond M46's" means.
