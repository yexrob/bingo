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

- [ ] `![shot](docs/x.png)` in assistant text draws the picture on a
  kitty terminal, shows `shot` as a chip elsewhere; a URL does the same.
- [ ] A destination that is not a picture, or not there, is a dim note
  after the alt, tried once.
- [ ] The identity tells the model; snapshot pasted.
- [ ] Every AGENTS.md gate; budget 331; tui-smoke; pty; `run.rs` under
  1000 non-test lines.
- [ ] Hands-on (main session with the user): "直接展示" answered with a
  markdown image, no `Read`.

## Non-goals

Pictures in a person's `--print` output (text is text there). Sending a
markdown picture out through Feishu. Animated GIF. Images inside table
cells (the chip line falls after the table). A cache across sessions.

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
