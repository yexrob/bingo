# M57 — The form card, compared

## Goal

M53's form card was driven side by side with Claude Code's on
2026-09-04 (both in tmux, the same three-question prompt, the main
session's own hands). bingo's card is right in the large — one card,
tabs, the preview beside the option, multi-select, one submit — and
four small things Claude Code does read better. Take those; keep
bingo's own card (the bordered box is §2's law: the only bright
border on screen), its descriptions under the options, and its
`☐/☒` boxes.

What Claude Code draws (captured):

```
←  ☒ 布局  ☐ 主题  ☐ 功能  ✔ Submit  →
默认主题用亮色还是暗色？
❯ 1. 亮色（Light）
     白底为主 …
  2. 暗色（Dark）
     深色底为主 …
  3. Type something.
──────────────────────────
  4. Chat about this
Enter to select · Tab/Arrow keys to navigate · Esc to cancel
```

What bingo draws today:

```
│ ☒ 布局 · 主题 · 功能            │
│                                 │
│ 默认主题用亮色还是暗色？          │
│ ❯ 1. 亮色                       │
│      整体更明快 …               │
│   2. 暗色                       │
│      整体更沉浸 …               │
│   3. Other                      │
```

**Amended 2026-09-04, at the user's word ("直接对齐claude code的那种交互"):
the card aligns with Claude Code's interaction outright**, including
what the first draft kept back. The user's taste rules the surface
(§10 records it as user-directed); the design doc's §2 border law is
amended for this one card, with the date.

## Bricks

1. **The shape.** No bordered box: the card is a band between two dim
   rules the transcript's width, like Claude Code's. Row 1 is the tab
   row `←  ☐ 布局  ☐ 主题  ☐ 功能  ✔ Submit  →` — a box per question
   (`☒` once fixed), a final `Submit` tab, arrows at both ends that
   are dim when there is nowhere to go; the active tab in `text`, the
   rest dim. Then the question, then its options.
2. **The options.** Numbered; a description under each **except when
   the active question has previews**, where the options are compact
   and the preview of the option under the cursor stands beside them
   (≥ 100 columns) or above them (narrower) inside a dim single-line
   frame. Multi-select options wear `[ ]`/`[✔]` (ASCII `[ ]`/`[x]`).
   The last numbered option of every question is `Type something.` —
   choosing it opens the words row under the options, and `⏎` there
   fixes the typed answer.
3. **Chat about this.** Under the band's lower rule, one more numbered
   row `N. Chat about this`: choosing it cancels the form (`Answer::
   Cancel`) and puts the caret in the composer with nothing typed, so
   the person talks to the model instead of answering — the same
   outcome `esc` has today, named as a choice.
4. **The Submit tab.** `⏎` on Submit with every question fixed sends
   the `Form`; with one open it walks to the first open question
   (Claude Code's behaviour) rather than sending.
5. **The key line.** One dim line under the lower rule, after the
   chat row: `Enter to select · ↑/↓ to navigate · Tab to switch
   questions · Esc to cancel` (multi-select adds `Space to toggle`;
   a previewed question adds nothing).
6. **Notes on an answer (`n`).** Claude Code lets a person attach a
   note to the option under the cursor (`Notes: press n to add
   notes`); the note travels with the answer. Do this last and only
   if the `Answer` shape takes it without a wire change (`Answer::
   Choice { ids }` → a note needs a field: `notes: Option<String>`,
   serde default, on `Choice` and `Form`'s items — a schema fixture
   test first). The tool's result line carries it as `header: label
   — note`. If it does not fit, record it in Verified and skip.

## Files

`bingo-surface-tui/src/form.rs` (+ its `TestBackend` catalogues in
`screens.rs`: the three `form_*` snapshots move, read each),
`docs/design/tui.md` §2/§7 (dated). Nothing outside the TUI.

## Exit criteria

- [x] The card is the rule band with the tab row, arrows, boxes and
  a Submit tab; `⏎` on Submit with an open question walks to it.
- [x] `Type something.` opens the words row; `Chat about this`
  cancels into the composer; the key line stands under the rule.
- [x] A previewed question shows compact options and a framed
  preview; multi-select wears `[ ]`/`[✔]`.
- [x] Every AGENTS.md gate; tui-smoke; the three form snapshots
  re-read and accepted.
- [ ] Hands-on (main session): the same prompt in tmux, captured.

## Non-goals

A note on a free-text answer. Claude Code's rule-band for any *other*
card (the permission card keeps its border).

## Risks

- One more row for the key line on a short screen: it is the first
  row the card yields (before an answer, as §2 says of the preview).

## Verified

*2026-09-04, worktree `.claude/worktrees/m57` on `m57-form-compared`,
base ca1c8431. Three commits.*

### What landed

- **`12be91e0` — the form's own catalogue module.** `screens.rs` was
  1009 non-test lines and `scripts/check_discipline.sh` fails at 1000,
  **on the base commit already**: the gate was red before this branch
  began. The form's screens moved to `screens/forms.rs` (983 left), which
  is where the two this milestone adds belong. The snapshot files keep
  their names — insta reads the module of the macro's call site, and
  `both`/`without_glyphs` still live in `screens.rs`.
- **`3a662f43` — the card.** Bricks 1–5, as one change: they are one
  card and one `form::rows`, and no split of them compiles on its own
  (`Head` gains the room the card lays itself out in, and the tab row is
  inert without `Submit`'s `⏎`). The brief asked for a commit per brick;
  this is the one deviation from it, and the commit body names each.
  1. `layers::Shape` (`Boxed` | `Band`) is the one place a card's shape
     is written: it owns the rows the chrome spends, the cells the lines
     are held in, and the height the whole wants, so `view::card` reads
     it three times instead of branching on the kind. `dialog::shape` is
     the only mapping. The band draws one dim rule where the box's top
     border was; the rule that closes it is a line of the card, so what
     hangs under it hangs outside it. Tab row:
     `←  ☐ Auth method  ☒ Library  ☐ Targets  ✔ Submit  →`.
  2. Options numbered; descriptions unless the question carries mockups,
     and then compact labels with the mockup of the option under the
     cursor in a dim frame that hugs it (one cell of padding, corners
     from `theme::border()`), beside them from 100 columns and above
     them under that. A set wears `[ ]`/`[✔]`; the words row is
     `Type something.`
  3. `Chat about this`, numbered, under the band's rule: `Answer::Cancel`.
  4. `⏎` on `Submit` sends every answer; with a question open it walks to
     the first and the tab says `1 question is still open.`
  5. The key line, and `Card::within` — the card gives up the keys first,
     then the mockup **whole**, and never an answer.
- **The records (this commit).** `docs/design/tui.md` §2 (the band, and
  that the border law holds for every other card), §4 (the `card` row's
  "no hint row" exception, and a `form card` row of its own), §7 (the
  `Submit` tab and the two named ways out) and §10 (the dated,
  user-directed decision).

### What the plan got wrong, and what was decided under it

- **Brick 6 (`n` for notes) did not land.** Its own condition — "only if
  the `Answer` shape takes it without a wire change" — is not met.
  `Answer::Choice { ids }` is matched or built at 48 sites across 14
  files in 9 crates, and `schema/rpc.json` and `schema/plugin.json` are
  both committed and would be regenerated; the plan's Files section says
  "Nothing outside the TUI." The shape it would take is recorded and not
  built: `notes: Option<String>` with `#[serde(default,
  skip_serializing_if = "Option::is_none")]` on `Choice`, a fixture
  pinning that an old frame still loads, and the tool's result line as
  `header: label — note`. With it goes the `Notes: press n to add notes`
  line under the preview's frame, which is its half of the same brick.
- **`Type something.` is on every question that takes words, not every
  question.** `Question.free_text` is the tool's own field and
  `dialog.rs`'s law is that a row may never promise what the kernel
  would refuse; a question asked with `free_text: false` gets no words
  row. Every question `bingo-tool-fs` opens does take them.
- **The preview's frame is `╭─╮ ╰─╯`, not `┌─┐ └─┘`.** A second
  box-drawing set is four more glyphs, and `Glyphs` is capped at 16
  fields by the cohesion check. `theme::border()` is the set every box
  already draws with and has an ASCII twin (`+ - |`) for free.
- **`Glyphs` had no room for `✔`.** `todo`/`todo_done` became one
  `todo: [&str; 2]` — one fact in two states, the shape `sparkles`
  already has — and the freed field is `tick` (`✔`, `x` in ASCII), spent
  by `✔ Submit` and by the `[✔]` inside a set's brackets. Only
  `theme::todo(done)` ever read the two, so the merge touched one file.
- **`←`, `→` and the key line's `↑/↓` are the card's words, not
  glyphs.** They have no ASCII twin and there is no field for one; they
  stand in `BINGO_ASCII=1` exactly as `·` and the help sheet's own
  `↑↓ to walk it` and `sheet → card → dropdown` already do. Pinned by
  `form_in_ascii`. If that is wrong, it is wrong for `keys.rs` too, and
  the fix is one decision for both.
- **The `Submit` tab draws a `1. Submit` row.** No capture of Claude
  Code's own `Submit` tab was available, so this is a decision, not a
  copy: the tab says where you are and the row says what `⏎` does, which
  keeps `⏎` meaning "the row under the cursor" everywhere on the card —
  otherwise `⏎` on the chat row would have to mean two things.
- **The last `⏎` no longer sends.** M53's card sent when the last
  question was fixed; now the walk lands on `Submit` and a person presses
  `⏎` once more. That is Claude Code's shape and brick 4's word, and it
  costs one keystroke.
- **`crowded_form` split into two scenes.** With the keys and the mockup
  giving way, twelve options and a mockup now *fit* at 80×24, so
  `form_end`'s old `…` was no longer true. It asserts what happens
  instead (nothing cut, the keys gone, the mockup gone whole, every
  answer kept), and a new `form_crowded` — twenty-four options — keeps
  the window-and-`…` behaviour covered. No test was weakened.
- **`dialog::rows` gained a `room` argument** beside `width`, and
  `form::Head` a `room` field. Without it the card cannot know which of
  its rows to give away, and `layers`' own fitting (first line + newest
  rows) would cut a frame in half. `view::card` already computed the
  number for `fitted_answers`; it now passes the same one twice. It
  counts rows **before** `view.rs` wraps them, so a key line too long for
  the width — the multi-select one is 93 cells and wraps at 80 — is
  counted as one row and drawn as two. The card is then a row taller
  than it budgeted and `layers`' own fitting takes the question line.
  Pinned as it is by `form_part_answered_80x24`.

### Not verified

- Exit criterion 5 (hands-on) is the main session's, with the user.
- **No form has been drawn on a real terminal.** Neither
  `scripts/tui-smoke.sh` nor `tests/pty.rs` opens one, so the card is
  pinned by the `TestBackend` catalogues and by nothing that has seen a
  real cell. This was already true after M53.
- `Chat about this` was verified to answer `Answer::Cancel`
  (`the_chat_row_cancels_the_whole_form`). That the caret is then in the
  composer is the existing behaviour of a card closing, not something
  this milestone tested or changed.
- The `form_layout` catalogue is an English stand-in for the plan's
  Chinese capture (AGENTS.md: UI copy and tests are English), so it can
  be compared to Claude Code's row for row in shape but not in width.
- `bingo-surface-tui` **cannot** be cross-checked against Windows
  locally (ADR-0041's 2026-09-04 note: `aws-lc-sys` wants `windows.h`).
  Nothing here touches a process, a path, a signal or a clock — it is
  glyphs, styles and arithmetic on rows — so CI's `windows` job is the
  backstop.

### Gates

```
$ cargo fmt --all -- --check                      # exit 0, no output
$ cargo check --workspace --all-targets --locked -j 2
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 57.82s
$ cargo clippy --workspace --all-targets --locked -j 2 -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 33.60s
$ cargo test --workspace --locked -j 2            # tee target/m57-test.log
79 test binaries, 3696 passed, 0 failed; PIPESTATUS=0 0
(no flake hit: mentions::a_question_left_unanswered…, peers::one_kickoff_post…,
 bingo_plugin_rpc::connection::a_request_whose_process_ends… all green)
$ scripts/check_discipline.sh
kernel names no tool / cohesion ok
warn crates/bingo-core/src/session.rs:129 fn handle is 66 lines (>60)   # pre-existing
discipline ok
$ scripts/budget.sh
dependencies (unique, normal): 331 (max  331)
budget ok
$ cargo deny check
advisories ok, bans ok, licenses ok, sources ok
$ TMUX_TMPDIR=$(mktemp -d) scripts/tui-smoke.sh
tui-smoke ok
```
