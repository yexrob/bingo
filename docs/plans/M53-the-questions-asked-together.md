# M53 — The questions, asked together

## Goal

`AskUserQuestion` today opens one interaction per question, so four
questions are four cards one after another, each answered before the
next is seen (user, 2026-09-04: "现在是一个一个问"). Claude Code asks
them as one card with a tab per question, walks the tabs, submits once,
and — for a layout or design choice — shows an ASCII preview beside the
option under the cursor. The user wants that shape. Nothing about *who
answers* changes (ADR-0039: the door, the stance, the roles); what
changes is that a set of questions is one interaction, and an option
may carry a preview.

## Bricks

1. **The shape, in the sdk.** `InteractionKind::Form { questions:
   Vec<Question> }` where `Question` is the struct the current
   `Question { question, header, options, free_text, multi }` variant
   spells inline — extract it, and the old variant keeps its wire shape
   by holding it flattened (`#[serde(flatten)]`), so every existing
   frame and the gate/ACP door are untouched. `QuestionOption` gains
   `preview: Option<String>` (a mockup in monospace, a few lines).
   `Answer::Form { answers: Vec<Answer> }`, one per question in order,
   each a `Choice`, a `Text`, or `Cancel` for a question left
   unanswered. `answer_for(role)` (ADR-0039 §2) answers a `Form` with
   every question's role option, or `None` if any lacks one. Contract
   first: the sdk's schema/fixture tests (find where `InteractionKind`
   and `Answer` are pinned — `bingo-sdk` snapshots, `bingo-rpc`'s wire
   fixtures) gain the new kind and the new answer *before* anything
   reads them; an old fixture with a bare `Question` still loads.
2. **The tool asks once.** `bingo-tool-fs/src/ask.rs` opens one `Form`
   for all its questions; `AskOption` gains `preview: Option<String>`
   ("shown beside the option, monospace; single-select questions
   only — refused on a multi-select"); the result is one line per
   question as today, `header: skipped` for a `Cancel`. `DESCRIPTION`
   says the questions are shown together and the preview's job (a
   layout, a snippet, a config to compare). Validation as today plus
   the preview rule.
3. **The card.** A new `bingo-surface-tui/src/form.rs` beside
   `dialog.rs` (507 lines; the form is its own noun): a tab row of
   headers under the title (`Auth method · Library · Approach`, the
   active one bright, an answered one with `✓`), the active question's
   text, its options with descriptions, and — when any option of the
   active question has a preview — the preview of the option under the
   cursor in a pane to the right at ≥ 100 columns, below the options
   otherwise. Keys: `←`/`→`/`tab` walk tabs; `↑`/`↓`/digits move the
   cursor; `space` toggles in multi-select; `⏎` fixes the answer and
   steps to the next unanswered tab; `⏎` on the last with all answered
   submits the `Form`; typing answers in the person's words as the
   dialog does today; `esc` cancels the whole form. The card obeys
   §2's "the preview gives way, never the answers" on a short screen.
   `TestBackend` snapshots: two tabs, one answered; a preview pane at
   120 columns and stacked at 80; the plain-colour degrade.
4. **The other surfaces.** `--print`'s stream-json carries the new
   kind and answer as JSON (schema test); `--print` text mode and a
   headless run meet a `Form` as they meet a `Question` (the stance,
   else the fail-closed fate); the Feishu channel (`channels/src/
   question.rs`) asks a `Form`'s questions one message each, its
   honest degrade, and folds the replies into one `Answer::Form`; the
   RPC doors (`plugin-rpc/src/doors.rs`) and the ACP question mapping
   are untouched (they open single `Question`s). Windows screens
   (`screens/windows.rs`) list a pending form as one item.
5. **MCP elicitation, the client half.** `bingo-mcp` declares the
   `elicitation` capability at handshake and answers a server's
   `elicitation/create` (spec 2025-06-18: `message` + a flat
   `requestedSchema` of string/number/boolean/enum properties, with
   `required`, `default`, `title`/`description`) through the ADR-0039
   door as one `Form`: an enum property is a choice question, a
   boolean a two-option one, a string or number a question with no
   options that must be answered in words (`Question.options` empty
   and `free_text: true` — the card shows an input row; a number is
   checked before it is sent back). The reply maps to `accept` with
   the content, `decline` (the person answered nothing / the
   headless stance), or `cancel` (`esc`). The URL mode (`mode:
   "url"`) opens the link the way `/login` opens one and waits for the
   person's confirm. Contract first: a fixture of a real request from
   the spec, the mapped `Form`, and the reply for each action. The
   card's title names the server that is asking.
6. **Docs.** ADR-0039 gains a dated note (≤12 lines): the form is a
   set of the existing question, not a new door; the preview is the
   option's; elicitation is the mapping §3 recorded and now builds.
   `docs/design/tui.md` §2/§7 gain the card (dated).

## Files

`bingo-sdk/src/event.rs` (+ fixtures/snapshots), `bingo-tool-fs/src/
ask.rs`, `bingo-surface-tui/src/{form.rs,dialog.rs,input.rs,layers.rs,
screens/windows.rs}`, `bingo-surface-print/src/{lib.rs,stream_json.rs}`,
`bingo-channels/src/question.rs`, `bingo-mcp/src/` (capability,
`elicitation.rs`), `docs/adr/0039-…md`, `docs/design/tui.md`.
`run.rs` untouched.

## Exit criteria

- [x] One `AskUserQuestion` call with three questions is one card with
  three tabs; answers land as one tool result, in order.
- [x] An option's preview shows beside it under the cursor (pane at
  ≥ 100 columns, stacked below otherwise — it landed *above* the
  options, see Verified); a multi-select with a preview is refused by
  the tool.
- [x] Old frames with a bare `Question` still load; the schema
  fixtures pin `Form` and `Answer::Form`.
- [x] Headless/bypass answer a `Form` by the roles or fail closed.
- [x] An MCP server's `elicitation/create` is a form card naming the
  server; accept/decline/cancel each reach the server (fixture test
  against a scripted server).
- [x] Every AGENTS.md gate; budget 331; tui-smoke; pty.
- [ ] Hands-on (main session with the user): a three-question ask with
  a layout preview, seen and answered.

## Non-goals

Nested or array schemas in elicitation (the spec keeps it flat; refuse
with `decline` and a notice). Images in previews. Reordering or
skipping questions from the model's side. A form spanning turns.

## Risks

- The wire: a client that knows `Question` and not `Form` sees an
  unknown kind. `SessionState::apply` must not drop the interaction;
  surfaces that cannot render a `Form` answer `Cancel` (fail closed),
  as they would an unknown kind today — check what `apply` does with
  an unknown `InteractionKind` and say so in Verified.
- `input.rs` and `layers.rs` route keys to the dialog; a second card
  type is a second key map. Keep one `Card` trait or one enum with
  the form as an arm, not a parallel path.

## Verified

*2026-09-04, worktree `.claude/worktrees/m53` on `m53-form`, base
72033a64. Six commits, one per brick.*

### What landed

1. **The shape, in the sdk** (`6471cde6`). `Question { question, header,
   options, free_text, multi_select }` extracted from the variant that
   spelled it inline; `InteractionKind::Question(Question)` holds it as a
   newtype. The plan's `#[serde(flatten)]` is *rejected* by serde on a
   newtype variant and is not needed — an internally-tagged newtype
   variant flattens on its own, and
   `a_bare_question_on_the_wire_is_the_shape_it_always_was` pins the
   byte-identity. `InteractionKind::Form { title, questions }`,
   `Answer::Form { answers }`, `AnswerSpec::Form`,
   `QuestionOption.preview`; `answer_for(role)` answers a form with every
   question's role option, or nothing if one lacks it
   (`a_form_is_answered_by_every_questions_role_option`,
   `a_form_one_of_whose_questions_is_unmarked_is_a_persons_alone`).
   Contract first: the frames snapshot gained the `Form` frame (seq 26,
   with a preview) and the `Answer::Form` frame (seq 27), and both
   committed schemas — `schema/rpc.json`, `schema/plugin.json` — were
   regenerated in the same commit, before anything read the new kind.
   Five fail-closed `Form` arms went in with it (core gate, ACP question,
   plugin-rpc doors, channels ladder, tui dialog), so the door, the
   stance and the roles are untouched.
2. **The tool asks once** (`0dd5a0d1`). `ask.rs` opens **one** `Form` for
   every question of one call (`title: None` — the model's questions say
   what they are about); `AskOption.preview` is refused on a
   multi-select question ("the option {..} carries a preview, which a
   multi-select question cannot show"); the result is one line per
   question, `header: skipped` where the answer was `Cancel` or a deny,
   and a form that comes back with the wrong number of answers is an
   error, not a guess.
3. **The card** (`4afc3ba2`). A new `crates/bingo-surface-tui/src/form.rs`
   (825 lines, 13 tests) owns the form noun; `dialog.rs` keeps the
   one key path — `input.rs → Dialog::on_key` and `view.rs →
   dialog::rows` are still the only ways in, and the active question's
   cursor **is** `Dialog::focus`, so click mapping, `fitted_answers` and
   the windows screen needed no second map. Tab row of headers as the
   card's own head (`✓` on an answered one), the active question's text
   and options, and the preview of the option under the cursor: a pane
   beside the options at ≥ 100 columns, stacked above them under that.
   Snapshots: `form_asked_80x24`, `form_asked_120x40`,
   `form_part_answered_80x24`, `form_part_answered_120x40`,
   `form_in_ascii`, and `windows__form_end` for the short screen.
4. **The other surfaces** (`8c205122`). `--print` writes each question to
   stderr and reads a line per question into one `Answer::Form` (an
   option id is a `Choice`, other words are `Text` where the question
   takes them, an empty line is `Cancel`); the headless/bypass paths meet
   a `Form` through `answer_for` exactly as they meet a question. The
   Feishu channel asks a form one message at a time —
   `Question::settles(answer) -> Settled::{Whole, More}` carries the
   answers already given and the questions left, and the last reply
   settles the whole interaction.
5. **MCP elicitation, the client half** (`1f1f83ab`). `bingo-mcp`
   declares `elicitation: {form: {}, url: {}}` at the handshake and
   answers `elicitation/create` through the ADR-0039 door. The mapping is
   a pure module (`elicitation.rs`, 600 lines, 10 tests) that reads
   the schema as **raw JSON**, so the five fixtures are the spec's own
   request bodies quoted verbatim (`elicit-username`, `-contact`,
   `-choices`, `-url`, `-nested`) and a new revision is a property to
   read, not a type to grow. A server may only ask while it is answering
   us, so the `tools/call` in flight is the door: an RAII `Guard` on an
   id-keyed stack (`Asker::during`), and a request with nothing in flight
   is declined with a warning. Round trips against a scripted server:
   `a_servers_question_becomes_a_form_card_and_the_answer_reaches_it`,
   `leaving_the_card_cancels_and_an_unanswered_requirement_declines`.
   A nested or array schema is declined with a notice (non-goal, honoured).
6. **Docs** (`df767503`). ADR-0039's dated note, and the card in
   `docs/design/tui.md` §2 and §7.

### What the plan got wrong

- `#[serde(flatten)]` on the newtype variant: serde rejects it. Nothing
  else about brick 1's wire shape changed.
- `Form { questions }` was not enough. An MCP server's elicitation has a
  `message` that belongs to the whole set and names the server, so the
  kind carries `title: Option<String>` — skipped when absent, so a
  model's form serializes as the plan drew it.
- **The preview is stacked *above* the options, not below.** §2's law is
  "the preview gives way, never the answers", and the fitting machinery
  keeps line[0] and the *newest* rows (`layers::card::fitted`,
  `view::fitted_answers`): a preview below the options would push the
  answers off a short screen. Above them — where a permission card's diff
  already sits — the answers are what survive. Proven by `form_end`.
- `dialog::rows` gained a `width` argument (the pane needs the width to
  decide), which is one line in `view.rs`.
- `theme.rs`'s allow-list had to move: `form.rs` joins `text`, `dim` and
  `presence`, and `dialog.rs` leaves `presence` (the agent suffix moved
  into `form::head`).
- `input.rs` now routes an open interaction *before* the `Tab` branch: a
  card that has its own use for `tab` must see it first.
- The Feishu channel degrades a multi-select form question to one choice
  per reply; a chat has no rung for a set.
- URL-mode elicitation shows the full URL and takes the person's consent,
  but this client opens nothing itself (no browser opener in `bingo-mcp`,
  and reaching for another plugin's would be a plugin-to-plugin import).
- `rmcp`'s dev-dependency gained the `elicitation` feature, without which
  `Peer::create_elicitation` does not exist and the scripted server could
  not raise one. It adds `url`, already in the tree: `Cargo.lock` +1
  line, budget unchanged at 331.
- Elicitation property order is the server's own: `serde_json`'s
  `preserve_order` is on transitively (rmcp, schemars), so the tests are
  order-insensitive rather than pinning a transitive feature.

### The `apply` finding (risk 1)

`SessionState::apply` never inspects `InteractionKind`:
`interaction_opened` is `retain(|i| i.id != interaction.id)` then
`push(interaction.clone())`. **No kind is dropped**, so an older
surface folding a `Form` frame keeps the interaction and answers it with
whatever its own fail-closed arm says — which is what every surface here
does. The real edge is one layer earlier: `InteractionKind` is
`#[serde(tag = "kind")]` with no `#[serde(other)]`, so a client of an
older sdk **cannot deserialize the frame at all** — it fails on the
whole `Frame`, not on the field. That is the wire's existing contract
(every kind is known to every reader of one version), not something this
milestone changed; a tolerant reader would be its own decision, and it
is recorded here, not built.

### Gates

```
$ cargo fmt --all -- --check                      # exit 0, no output
$ cargo check --workspace --all-targets --locked
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 8.22s
$ cargo clippy --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.89s
$ cargo test --workspace --locked      # tee target/m53-test.log
79 test binaries, 3635 passed, 0 failed; PIPESTATUS=0 0 0
(no flake hit: mentions::a_question_left_unanswered…, peers::one_kickoff_post… both green)
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
17 steps, tui-smoke ok
$ cargo test -p bingo --locked --test pty
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
$ cargo check -p bingo-sdk -p bingo-core -p bingo-tool-fs --all-targets \
    --target x86_64-pc-windows-msvc
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 21.06s
```

`scripts/tui-smoke.sh` hardcodes `tmux -L bingo` and kills that server on
the way in, so two worktrees running it at once take each other's
terminal away ("server exited unexpectedly" on the first run here). A
`TMUX_TMPDIR` of its own is the workaround; a socket named after the
worktree would be the fix, and is recorded, not built.

`bingo-mcp` **cannot** be cross-checked locally: `reqwest → rustls →
aws-lc-rs → aws-lc-sys`, whose build script wants `windows.h` (ADR-0041's
2026-09-04 note, the same wall `bingo-surface-tui`, `bingo-pictures` and
`bingo-channels` already hit). Nothing in this milestone touches a
process, a path, a signal or a clock; CI's `windows` job is the backstop.

### Not verified

- Exit criterion 7 (hands-on) is the main session's, with the user.
- No `Form` has been drawn on a real terminal: neither `scripts/
  tui-smoke.sh` nor `tests/pty.rs` drives one, so the card is pinned by
  the `TestBackend` catalogues and by nothing that has seen a real cell.
- The URL mode's own card was not driven through a live server (the
  scripted server raises the form mode); the mapping and its fixture are
  tested, the round trip is not.
