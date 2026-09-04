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

- [ ] A multi-select with ticks and typed words reaches the model as
  both, in one line; an old frame still loads; fixtures pin `other`.
- [ ] Walking the tabs of a three-question form moves no row: the
  lower rule, the chat row and the key line stay put (test).
- [ ] Every AGENTS.md gate; budget unchanged; tui-smoke; Windows
  cross-check for sdk/core/tool-fs/mcp.
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
