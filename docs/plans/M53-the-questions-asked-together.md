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

- [ ] One `AskUserQuestion` call with three questions is one card with
  three tabs; answers land as one tool result, in order.
- [ ] An option's preview shows beside it under the cursor (pane at
  ≥ 100 columns, stacked below otherwise); a multi-select with a
  preview is refused by the tool.
- [ ] Old frames with a bare `Question` still load; the schema
  fixtures pin `Form` and `Answer::Form`.
- [ ] Headless/bypass answer a `Form` by the roles or fail closed.
- [ ] An MCP server's `elicitation/create` is a form card naming the
  server; accept/decline/cancel each reach the server (fixture test
  against a scripted server).
- [ ] Every AGENTS.md gate; budget 331; tui-smoke; pty.
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
