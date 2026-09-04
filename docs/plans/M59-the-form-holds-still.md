# M59 — The form holds still, and keeps every answer

## Goal

Two things the user found in M57's card (2026-09-04): a multi-select
answered with ticks **and** a typed answer keeps only the typed words
("用户多选+选了自定义的时候 就只看自定义了"); and the band's height
changes from question to question, so everything above it moves as
the tabs are walked ("多个问题切换的时候 布局不稳 高低会跟随问题变
化"). The first is a wire shape: `Answer` is a `Choice { ids }` *or*
a `Text { text }`, never both. The second is the card's own geometry.

## Bricks

1. **A choice may carry words.** `Answer::Choice` gains `other:
   Option<String>` (`serde(default, skip_serializing_if = none)`): the
   ticked ids and, beside them, the words a person typed under `Type
   something.`. An old frame without the field loads as before; the
   schema/fixture tests are updated first (find every fixture that
   pins `Answer` — sdk snapshots, rpc wire fixtures, `docs/` schemas)
   and a new fixture pins the shape with `other`. `Answer::Text`
   stays for a question answered in words alone. Every match on
   `Answer::Choice { ids }` (48 sites across 9 crates, M57 Verified
   counted them) becomes `Answer::Choice { ids, .. }` where it does
   not care — a mechanical change, one commit.
2. **The tool reads both.** `AskUserQuestion`'s result line for a
   multi-select is the ticked labels and then the words: `功能: 仪表盘,
   搜索, <typed>`; for a single-select with words only, as today. The
   MCP elicitation mapping (`bingo-mcp`): an enum property with `other`
   is answered with the words (the schema has no room for both — say
   so in the mapping's doc). Feishu: unchanged (one choice per reply).
3. **The card keeps every tick.** In `form.rs`, choosing `Type
   something.` on a multi-select opens the words row without clearing
   the ticks; `⏎` fixes `Choice { ids, other: Some(words) }`; on a
   single-select, words alone (`Text`) as today. The tab's `☒` and the
   Submit tab's count treat it as one answered question. Snapshot of a
   multi-select with two ticks and words.
4. **The band holds still.** The band's height is one number for the
   whole form: the tallest question at the current width (options,
   descriptions or the framed preview, the words row when open, the
   key line), measured once per draw over *every* question rather
   than the active one; shorter questions are padded with blank rows
   under their options so the lower rule, `Chat about this` and the
   key line never move, and nothing above the band moves either. A
   resize re-measures. `TestBackend` test: three questions of
   different heights, walk the tabs, every row's y is the same across
   the three frames (compare the lower rule's row). §2/§3's "nothing
   jumps" applies to the card too — record it in the design doc.

## Files

`bingo-sdk/src/event.rs` (+ fixtures/schemas), every `Answer::Choice`
matcher, `bingo-tool-fs/src/ask.rs`, `bingo-mcp/src/elicitation.rs`,
`bingo-surface-tui/src/{form.rs,screens/forms.rs}`,
`docs/design/tui.md` §2 (dated), ADR-0039's M53 note (one line).

## Exit criteria

- [x] A multi-select with ticks and typed words reaches the model as
  both, in one line; an old frame still loads; fixtures pin `other`.
- [x] Walking the tabs of a three-question form moves no row: the
  lower rule, the chat row and the key line stay put (test).
- [x] Every AGENTS.md gate; budget unchanged; tui-smoke; Windows
  cross-check for sdk/core/tool-fs — `bingo-mcp` cannot be
  cross-checked locally (ADR-0041's note), and CI's `windows` job is
  its backstop.
- [ ] Hands-on (main session with the user).

## Non-goals

Notes on an answer (`n`) — a different field with the same kind of
change; do not bundle. A band taller than the screen (the existing
window-and-`…` handling stands).

## Risks

- The fixed height is the tallest question's, so a form with one
  long question and two short ones shows air under the short ones.
  That is the price of stillness; §3 chose it for the activity band
  already.

## Verified

*2026-09-04, worktree `.claude/worktrees/m59` on `m59-form-still`, base
dev `d0ed37dd`. Five commits.*

### What landed

1. **A choice may carry words** (`e016c93f`, then the sweep `1ae32e71`).
   `Answer::Choice` gains `other: Option<String>` with `#[serde(default,
   skip_serializing_if = "Option::is_none")]`. Contract first: frames 1–27
   of `bingo_sdk__event__tests__frames.snap` are **byte-identical** (the
   whole diff of that snapshot is the added frame 28, a form answered with
   two ticks and words), a new
   `a_choice_carries_the_words_typed_beside_the_ticks` pins both wire
   shapes in both directions, and `schema/rpc.json` and
   `schema/plugin.json` were regenerated in the same commit — `other` is a
   property, never in `required`. The sweep is 30 constructions naming
   `other: None` and 6 matches reading `{ ids, .. }`, across 14 files in
   9 crates, exactly the count M57 recorded.
2. **The tool reads both** (`35701940`). `ask.rs` grew one pure brick,
   `said(question, ids, other)`: the labels in the order the question
   listed them, then the words, joined once — `Features: dashboard,
   search, and an audit log`. Words alone still answer; words that are
   only blanks answer nothing and fail as they did. `DESCRIPTION` says
   the result carries both. `bingo-mcp`'s `Field::value` prefers the
   words, and `Wants::Choice` now accepts them: a `requestedSchema`
   property holds one value, so the client sends what the person said
   rather than a value they did not choose. Said in the mapping's module
   doc and in ADR-0039's new note.
3. **The card keeps every tick** (`adf3b157`). `form::answer` was a
   three-way `return` ladder — words row *or* ticks — and is now two
   bricks read together (`ticked_ids`, `written`): what the card shows is
   what it sends. `Type something.` never cleared the ticks; only the
   answer dropped them. Screens: `form_set_ticked_and_typed_on` at both
   sizes.
4. **The band holds still** (`f42d0361`). `holding()` measures the band
   once per draw over **every tab** — the `Submit` tab included, or
   walking to it would shrink the band — and over **every cursor position
   on each tab**, because the mockup drawn is the one under the cursor and
   a tab may not change height as its own rows are walked either.
   `Card::within` pads the tab on screen out to that number with blank
   rows under its options, and the key line with blanks under itself.
   Rows are counted through `wrap::wrap`, so a line too long for the width
   costs the two rows it takes.

### What the plan got wrong, and what was decided under it

- **Brick 1 cannot be one commit that builds the workspace.** Naming a
  field on a variant breaks every construction of it, so `e016c93f`
  builds `-p bingo-sdk` and `1ae32e71` builds the workspace. The brief
  asked for a commit each and that is what these are; the schemas are in
  the first, generated (not hand-edited) by running the drift test with
  the sweep already in the working tree.
- **`input.rs` had to be touched.** The brief fenced it off for another
  worker, but it holds five `Answer::Choice` constructions in tests and
  the crate does not compile without them. The change there is five
  `other: None,` lines, nothing else.
- **The plan's "the words row when open" is not enough for stillness.**
  A tab's height also changes as its *own* cursor walks it, because the
  mockup shown belongs to the option under the cursor and mockups differ
  in height. `deepest()` therefore takes the tallest over every cursor
  position, not the height at the current one.
- **Two held numbers, not one.** The mockup is still the first thing the
  band gives away after the keys (§2, M57), and only a previewed tab has
  mockup rows — so holding every tab out to a height that includes them
  would have made the giving-away pointless. `Held` carries `body` (with
  the mockup) and `bare` (without); the decision is made once for the
  whole form, so every tab is held out to the same one.
- **The key line needed its own pad.** It hangs *below* the rule, so its
  height does not move the rule — but the card is anchored at the foot of
  the region when the row that asked is off screen, and there a taller
  foot pushes the top up. `held.keys` is the longest key line's wrapped
  height and shorter ones stand on a blank row.
- **M57's row-count quirk is fixed as a consequence.** `Card::within`
  counted unwrapped rows against the room, so the multi-select key line
  (93 cells, two rows at 80) made the card a row taller than it budgeted.
  It now counts what the screen draws. This is what changed
  `form_part_answered_80x24` and `form_submit_with_one_left_*`.
- **`the_key_line_is_the_cards_last_row_…` was amended, not weakened.**
  The key line is now the last row that *says* anything; a blank may
  stand under it. The assertion is the same string, found from the end.
- **Eight of the eleven form screens moved and were read one by one.** At
  both sizes the three tabs of the fixture now open the band on the same
  row and close it on the same row — `form_asked_80x24`,
  `form_part_answered_80x24` and `form_submit_with_one_left_80x24` all put
  the band's rules at screen rows 5 and 16, the way out at 17 and the keys
  at 18. `windows__form_end` and `windows__form_crowded` (the short-screen
  scenes) and the two 120-column screens whose tab is already the tallest
  did not change at all: there the tallest tab is the one drawn and the
  keys either fit in one row or are given away, so there is nothing to pad.
- `form.rs` is **846 non-test lines** (737 before), still a warn and 154
  from the fail line. A split was considered and refused: the measure *is*
  the layout, read twice, and pulling it out would have to make `Form`,
  `Slot`, `body`, `keys` and `rows_of` visible across a module edge — more
  coupling for less cohesion. The next thing added here should split it.

### Not verified

- Exit criterion 4 (hands-on) is the main session's, with the user.
- **Opening the words row still grows the band by one row** — on every
  tab together, since the measure reads the form's current state. That is
  the plan's own "the words row when open"; reserving the row always would
  put a blank under `Type something.` on every question that takes words.
  Recorded, not built.
- The measure lays every tab out at every cursor position on every draw
  (four tabs × ~six rows here). It is arithmetic over already-built lines
  and no draw was measured as slower, but no benchmark was run either.
- **No form has been drawn on a real terminal.** Neither
  `scripts/tui-smoke.sh` nor `tests/pty.rs` opens one, so the card is
  pinned by the `TestBackend` catalogues and by nothing that has seen a
  real cell. True since M53.
- `bingo-mcp`'s enum-property behaviour was verified against the mapping
  and its fixtures, not against a server that validates its own
  `requestedSchema`: a strict server may refuse words that are not one of
  the values it named. The trade-off is the plan's and is recorded in the
  module doc.
- `bingo-surface-tui` and `bingo-mcp` **cannot** be cross-checked against
  Windows locally — `aws-lc-sys`'s build script wants `windows.h`
  (ADR-0041's 2026-09-04 note); the attempt and its failure are below.
  Nothing here touches a process, a path, a signal or a clock.

### Gates

```
$ cargo fmt --all -- --check                                 # exit 0, no output
$ cargo check --workspace --all-targets --locked -j 2
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.39s
$ cargo clippy --workspace --all-targets --locked -j 2 -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 35.45s
$ cargo test --workspace --locked -j 2 | tee target/m59-test.log
pipestatus=0 0
81 test binaries, 3755 passed, 0 failed   (3696 before; 0 `test result: FAILED`)
(no flake hit: mentions::a_question_left_unanswered…, peers::one_kickoff_post…,
 bingo_plugin_rpc::connection::a_request_whose_process_ends… all green)
$ scripts/check_discipline.sh
dependency direction ok / kernel names no tool / cohesion ok
warn crates/bingo-surface-tui/src/form.rs: 846 non-test lines (>700)
warn crates/bingo-core/src/session.rs:129 fn handle is 66 lines (>60)   # pre-existing
discipline ok
$ scripts/budget.sh
dependencies (unique, normal): 332 (max  332)
relink isolation: touching the TUI recompiled 0 crates for core (must be 0)
target/debug: 9 GB (soft max  5) — warn, this worktree's own build
budget ok
$ cargo deny check
advisories ok, bans ok, licenses ok, sources ok
$ TMUX_TMPDIR=$(mktemp -d) scripts/tui-smoke.sh
tui-smoke ok
$ cargo check -p bingo-sdk -p bingo-core -p bingo-tool-fs --all-targets \
    --locked -j 2 --target x86_64-pc-windows-msvc
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 18.52s
$ cargo check -p bingo-mcp --all-targets --locked -j 2 --target x86_64-pc-windows-msvc
  error occurred in cc-rs: … aws-lc-sys-0.44.0 … jitterentropy-timer.c
  (ADR-0041's wall; CI's `windows` job is the backstop)
```

No manifest and no `Cargo.lock` line changed: `git diff d0ed37dd -- Cargo.lock
Cargo.toml 'crates/*/Cargo.toml'` is empty, and the budget is what the brief
said it was.
