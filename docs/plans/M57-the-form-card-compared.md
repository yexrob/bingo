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

- [ ] The card is the rule band with the tab row, arrows, boxes and
  a Submit tab; `⏎` on Submit with an open question walks to it.
- [ ] `Type something.` opens the words row; `Chat about this`
  cancels into the composer; the key line stands under the rule.
- [ ] A previewed question shows compact options and a framed
  preview; multi-select wears `[ ]`/`[✔]`.
- [ ] Every AGENTS.md gate; tui-smoke; the three form snapshots
  re-read and accepted.
- [ ] Hands-on (main session): the same prompt in tmux, captured.

## Non-goals

A note on a free-text answer. Claude Code's rule-band for any *other*
card (the permission card keeps its border).

## Risks

- One more row for the key line on a short screen: it is the first
  row the card yields (before an answer, as §2 says of the preview).
