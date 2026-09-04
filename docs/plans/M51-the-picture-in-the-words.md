# M51 — The picture in the words

## Goal

A picture the model names in its own words draws in the transcript.
Today only a picture *part* draws — a `Read` of an image file, a
`WebFetch` of an image URL, a paste — and the model, asked to "just
show it", has to read the file to show it (M50 Verified: the user's
next ask, 2026-09-04). After M51, `![what it is](path-or-URL)` in
assistant markdown is the picture where the surface draws pictures, the
alt text where it does not, and the model is told so. A person's own
markdown gets the same treatment; nothing in the kernel or the journal
changes — the words stay words, the surface derives the picture at
render time like everything else it draws (ADR-0002).

## Bricks

1. **The link, from the markdown.** `markdown::render` today drops or
   flattens `Tag::Image`. Make it emit one logical line of its own for
   each image — the alt text in the chip style `transcript::pictured`
   already uses for a picture the terminal cannot draw — and return,
   beside the lines, the images it found: `Vec<Linked { line: usize,
   alt: String, dest: String }>` in document order. Pure; tests for an
   inline image inside a paragraph (its own line after the paragraph's
   words), an image alone, a reference-style image, a `<path with
   spaces>` destination, and text with none (the lines are byte-identical
   to today's — a snapshot proves it).
2. **A third kind of picture.** `graphics::picture::Source` gains
   `Linked { dest: String }`. Its `id()` is a hash of the destination in
   the id space the other two do not use: today `DRAFT` is one bit; make
   the top two bits the kind (`00` journal, `10` draft, `01` linked),
   never zero. A test pins the three kinds apart and the same `dest`
   to the same id across items — the same picture named twice is sent
   once (`Stored` already reconciles by id).
3. **The memo that loads.** A new `graphics/linked.rs`: `Linked`
   memo, `dest → Wanted | Loading | Loaded(Image) | Failed(String)`.
   The draw asks the memo for a `dest`; unknown → `Wanted` and the draw
   shows the chip (the alt) for this frame. Between frames, the run
   hands every `Wanted` to a task (`bingo_pictures::load` on
   `Source::parse(dest)`; a bare path resolves against the session's
   cwd; `~` expands; only `http(s)` URLs and files that exist are
   tried) and marks it `Loading`; the reply is one `Reply::Linked {
   dest, result }` in the run's existing reply seam, next to
   `Reply::Mentioned` — the code goes in the new module and *one* match
   arm in `run.rs`, which is at 976 non-test lines and fails at 1000:
   if the arm does not fit, move `Reply::Mentioned`'s body out first.
   `Failed` is never retried in the session and the chip says why in
   dim text after the alt (`(not found)`, `(not a picture)`). Tests:
   the memo's state machine; a load of a temp PNG through the seam.
4. **The rows.** `transcript::pictured` draws a `Loaded` linked picture
   under its chip line the way a journal picture hangs under `⎿`
   (`IMAGE_ROWS` peek, `ctrl+o` opens — the same fold), keyed by the
   memo rather than the item's parts; `Painted.blocks.pictures()`
   carries it so `Stored` sends it. On a terminal without graphics, or
   while `Wanted`/`Loading`/`Failed`: the chip line alone. Snapshot
   tests with `graphics::drawing()` and without; the chip line is what
   `--print` already shows (assistant text is printed as text there;
   nothing to change).
5. **The model's word.** M50's `# Pictures` section in `bingo-core`
   `IDENTITY` gains one bullet: writing a picture as markdown
   `![what it is](path or URL)` in the reply draws it in the transcript
   where the person's surface draws pictures, without a tool call;
   reading it with a tool is still how the model *sees* it. Snapshot
   accepted and pasted. Discipline 4c holds (no tool named).

## Files

`bingo-surface-tui/src/{markdown.rs,graphics/{picture.rs,linked.rs,
mod.rs},transcript/pictured.rs,run.rs}` (+ a new module for the
load-and-reply if `run.rs` needs relief), `bingo-core/src/prompt.rs`
(+ snapshot), `docs/design/tui.md` §5 (dated), `crates/bingo/tests/
pty.rs` if a scene fits without a network (a temp PNG on disk does).

## Exit criteria

- [x] `![shot](docs/x.png)` in assistant text draws the picture on a
  kitty terminal, shows `shot` as a chip elsewhere; a URL does the same.
- [x] A destination that is not a picture, or not there, is a dim note
  after the alt, tried once.
- [x] The identity tells the model; snapshot pasted.
- [x] Every AGENTS.md gate; budget 331; tui-smoke; pty; `run.rs` under
  1000 non-test lines.
- [ ] Hands-on (main session with the user): "直接展示" answered with a
  markdown image, no `Read`.

## Non-goals

Pictures in a person's `--print` output (text is text there). Sending a
markdown picture out through Feishu. Animated GIF. Images inside table
cells (the chip line falls after the table). A cache across sessions —
*done in M61: a fetched URL is kept under `<data_dir>/pictures/cache`
for a fortnight, so the same address across sessions is one fetch.*

## Risks

- Model text now causes the surface to read files and fetch URLs at
  render time. Bounded: `bingo_pictures::load`'s 30 s and size caps,
  once per destination per session, `http(s)` and existing files only.
  Recorded, not cured: a URL fetch the person did not ask for is a
  request their surface makes; the same is true of `WebFetch`, which
  the model can already call.
- A picture the model names before it exists (it is about to write it)
  fails once and stays failed for the session. Accept; the chip says
  `(not found)`, and the next reply can name it again — a different
  item, the same `dest`, the same memo entry. Say so in Verified if it
  bites; the cure is keying the memo on `(item, dest)`.

## Verified

*2026-09-04, worktree `.claude/worktrees/m51` on `m51-linked`, base a6e9512.*

### What landed

The five bricks, with five shapes different from the plan (below).

1. **The link, from the markdown** — `markdown::rendered(text, width) ->
   Rendered { lines, images: Vec<Linked { line, alt, dest }> }`;
   `render` is `rendered(..).lines` and every other caller keeps its
   signature. `Tag::Image` no longer flattens to inline alt text: the
   writer collects the alt between the tags (through the same
   `flat_text` a table's cells go through — one function, two readers)
   and `Writer::chips` puts each image on a line of its own after the
   words it stood among, as `[image: <alt>]` in `theme::dim()`, or the
   destination where the alt is empty. 7 new tests: an image inside a
   paragraph, one alone, a reference and a `<bracketed dest>`, an empty
   alt, two in one paragraph, a bulleted picture, and the byte-identity
   snapshot.
2. **A third kind of picture** — `Source::Linked { dest }`. The id space
   is now partitioned by its **top two bits** (`00` journal, `10` draft,
   `01` linked) over a 22-bit hash, so no two kinds can share a number
   whatever the hash does; `the_three_kinds_of_picture_never_share_a_number`
   pins all three pairs, and `the_same_destination_is_one_picture_wherever_it_is_named`
   pins that two answers naming one file are one picture — sent once,
   because `Stored` already reconciles by id.
3. **The memo that loads** — `graphics/linked.rs`: `Linked`, keyed by
   destination, `Loading | Loaded(Image) | Failed(String)` with *unknown*
   as the fourth state; `take` is "ask once, ever", `take_all` is a
   frame's whole list, `answers()` is what tells a block drawn before an
   answer from one drawn after it, and `source`/`read`/`note` are where a
   destination points, how it is read and the few words a chip carries.
   `MOST` (32, `stored::KEPT`) is a bound as much as a memo. In `run.rs`
   it is **one match arm** (`Reply::Linked`), one `Reply` variant and one
   eight-line `read_linked` after `hand_pictures`.
4. **The rows** — `pictured::in_the_words` puts the cells of a `Loaded`
   picture under its own chip line and a dim `(not found)` after the name
   of a `Failed` one, and hands back the destinations the words named so
   the run can go for them. `drawn` is now shared by both callers — a
   journal picture and a named one differ only in their `Source`, their
   room and their height. `Block` gained `wanted`, which rides with the
   lines through `blocks::Entry` for exactly the reason `pictures` does
   (M46), and `Blocks::wanted()` is the frame's list. 8 new tests in
   `pictured.rs`, 1 in `blocks.rs`, 3 in `run.rs`, 1 pty scene.
5. **The model's word** — one bullet, last in `IDENTITY` (below).
   Discipline 4c passes: no tool is named.

`run.rs` is **976 → 886** non-test lines: the submit path (`Mentioned`,
`submit`, `submit_text`, `mentioned`, `send_text`, `read_mentions`) moved
whole to a new `run/submit.rs`, as M47, M48 and M49 all said the next
change there had to do. Workspace tests 3547 → 3587.

### What the plan got wrong

- **There is no `Wanted` state, and the memo has no `RefCell`.** The
  plan had the *draw* mark an unknown destination `Wanted`, which needs
  interior mutability — and then `Picture::image_in` could no longer hand
  back a `&Image`, since a reference cannot leave a `Ref` guard. Making
  it hand back an owned picture instead would clone a multi-megabyte
  base64 string for the journal's pictures too. What a frame *wants* is a
  fact about the frame, so the block carries it (`Block::wanted`) the way
  it already carries its pictures, and the memo is plain `&mut` state the
  run owns. One representation, on the side that knows.
- **A memo is not enough to put a late picture on the screen.** A block
  is drawn once and cloned ever after (M46's lesson), so an answer that
  landed after its block was drawn would never be seen. `Revision` gained
  `linked: u64` from `Linked::answers()`: one answer redraws every block
  once, which is rare and cheap, and `a_picture_read_in_after_its_block_makes_it_draw_again`
  pins both halves — the read in flight changes nothing, the answer draws
  them again, and only once.
- **A terminal that draws no pictures reads nothing in.** The plan
  recorded "a URL fetch the person did not ask for" as a risk and bounded
  it by caps; it is bounded harder than that. `read_linked` returns
  immediately on `Graphics::Off`, so on Apple Terminal, under a tmux with
  the passthrough off, under `BINGO_GRAPHICS=off` and in every test and
  smoke run, an answer's words send this surface nowhere at all. The cost
  is that `(not found)` is a note only where a picture could have been
  drawn; the chip is the whole of the degrade elsewhere, which is what
  brick 4 asked for.
- **A person's own markdown does *not* get the same treatment**, against
  the goal's aside. `said::user` does not render markdown and never has:
  a person's line is the bytes they typed, verbatim, on a raised bar
  (§4). Turning it into markdown would change how every line anyone types
  is drawn — their `*` and `#` would vanish — which is a design decision
  about the person's bar, not a consequence of this milestone. It is not
  in the bricks or the exit criteria; it is left undone deliberately.
- **`item_lines` answers with a `Block`, not lines.** The chip's line
  number is an index into the markdown's own output, so the picture has
  to be put in before `speaks` wraps and marks the block; that makes
  `assistant` the one arm that produces pictures, and the other twelve
  wear `.into()`. `pictured::under_the_words` now takes the block and
  extends it rather than building one.
- **Two types are called `Linked`** — `markdown::Linked` (the link the
  words carried) and `graphics::Linked` (what became of a destination).
  The plan named both; they are the same noun from the two sides, and
  every use site carries its module. Recorded rather than renamed.

### The identity

The new last line of
`bingo_core__prompt__tests__the_identity_is_the_same_words_for_every_session.snap`
(accepted with `INSTA_UPDATE=always`; nothing else in the file moved):

```
- To show the user a picture that is already on their machine or at an address, write it in your reply as markdown — ![what it is](path or URL) — and it is drawn there, where their surface draws pictures; no call is needed to show one, only to see it yourself.
```

### The byte-identity snapshot

`markdown_without_pictures` is a document with headings, emphasis, a
link, a soft break, two lists, a nested item, a quote, a fence, a table
and a rule. It was **written against the pre-change renderer**: the file
was reverted to `a6e9512`, a probe test that calls `render` was appended,
and it passed against the snapshot this milestone's test produces —
`markdown::tests::a_document_with_no_picture_renders_exactly_as_it_did
... ok`. The chip branch therefore moves no row of any answer that names
no picture.

### What is not verified

- **No real terminal drew a linked picture.** As in M46, M48 and M49,
  every terminal in these tests is one this repository wrote. What is
  proven is that the right bytes go out (`ESC _ G a=T,f=100,q=2,U=1`, a
  PNG payload, once) and the right cells are drawn. The hands-on
  criterion is the main session's.
- **No URL was fetched in a test of this milestone.** The path arm is
  driven end to end (`graphics::linked`'s seam test, `run.rs`'s, the pty
  scene); the URL arm is proven only as far as `Source::parse` —
  `bingo_pictures::load`'s own suite covers the fetch, and M47's
  `a_mentioned_url_is_fetched_by_this_machine` covers the same call
  through the mention path. A `wiremock` scene here would have tested
  `bingo-pictures` twice.
- **The memo is keyed by the destination as written, across the whole
  tree.** A parent and a child with different working directories that
  both name `x.png` share one entry and one picture id. One memo, one
  `Ui`; not hit, not fixed, and the cure is keying on the resolved
  source rather than the word.
- **Streaming.** An answer's words are re-parsed every frame, so a
  half-written `![a](b` is prose and only a closed `![a](b)` is an image
  — which is when the read starts. A destination that becomes valid mid
  stream and then changes cannot happen (a stream appends), but nothing
  tests a delta boundary inside a destination.
- **`MOST` is a cap on reads, not on bytes.** Thirty-two `Loaded`
  pictures at `Image::MAX_BYTES` is ~160 MB worst case, beside the
  ~160 MB `Decoded` already records (M46). Not measured. Past 32
  destinations a session the chips draw and nothing is read; nothing is
  ever evicted, which is what keeps a transcript of many links from
  re-reading in a loop.
- **The Windows cross-check for the TUI cannot run here**, for the
  reason ADR-0041's note records: `reqwest` → `rustls` → `aws-lc-sys`,
  whose build script compiles C against `windows.h`, and there is no
  Windows SDK on this machine. The output is below. This milestone adds
  no `cfg`, no signal, no process and no clock; the one path-shaped thing
  it adds is `paths::expand`, which is pure string work over `~` and is
  asserted by a test that runs everywhere. `bingo-core` and `bingo-sdk`
  cross-check clean. CI's `windows` job is the backstop.
- **`transcript.rs` is 877 → 911 non-test lines.** Warned, not failing;
  the next change there should split it, as `run.rs` has now been.

### Gates, all from the worktree, `-j 2`

```
$ cargo fmt --all -- --check                                    # silent, exit 0
$ cargo check --workspace --all-targets --locked                # Finished
$ cargo clippy --workspace --all-targets --locked -- -D warnings   # Finished
$ cargo test --workspace --locked | tee target/m51-test.log
    exit 0; 79 result lines, 0 with a failure; 3587 passed, 0 failed
$ scripts/check_discipline.sh
    dependency direction ok / kernel names no tool / cohesion ok / discipline ok
    (pre-existing warns only; run.rs 886, transcript.rs 911)
$ scripts/budget.sh
    dependencies (unique, normal): 331 (max  331)
    warm cargo check -p bingo-core: 0s (max  20s)
    relink isolation: touching the TUI recompiled 0 crates for core (must be 0)
    budget ok
$ cargo deny check                 # advisories ok, bans ok, licenses ok, sources ok
$ scripts/tui-smoke.sh                                          # tui-smoke ok
$ cargo test -p bingo --locked --test pty              # 8 passed, 0 failed
$ cargo check -p bingo-core --all-targets --locked \
      --target x86_64-pc-windows-msvc                           # Finished
$ cargo check -p bingo-sdk --all-targets --locked \
      --target x86_64-pc-windows-msvc                           # Finished
$ cargo check -p bingo-surface-tui --all-targets --locked \
      --target x86_64-pc-windows-msvc
                    # FAILS in aws-lc-sys' build script (ADR-0041's note)
```
No known flake was hit. No crate joined the tree: the budget is 331
before and after.

### Hands-on (main session with the user)

*To be filled after the merge: "直接展示" answered with a markdown image
and no tool call, on the user's own terminal — including anything wrong.*

### Hands-on (main session with the user, 2026-09-04)

In the user's Ghostty, `target/debug/bingo` at dev `71493c97`: asked
to show a desktop screenshot and a remote PNG as markdown, the model
wrote both as `![…](…)` and both drew under their chips ("有了有了").
Two earlier failures were not this milestone's code: a path with
spaces written bare is not a CommonMark link (the words showed as
words — the identity now says to use `<…>`, and not a code block), and
one session had no graphics at all (chips with no note: the surface
reads nothing where it can draw nothing). A dead URL now says its
status (`79c14d2b`).

- [x] Hands-on: "直接展示" answered with a markdown image, no `Read`.
