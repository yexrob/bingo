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

## Bricks

1. **Every tab wears its box, and the last tab is Submit.** The tab
   row becomes `☐ 布局 · ☒ 主题 · ☐ 功能 · Submit`: a box per
   question (ticked once fixed), and a final `Submit` tab the walk
   reaches after the last question — `⏎` there sends the form, and
   the card says which questions are still open if any are (`2 left`
   in dim beside Submit). Today the head shows one `☒` for the card;
   a person cannot see which tab they left open. `←`/`→`/`tab` reach
   Submit like any tab. The row keeps §7's rule: the active tab is
   the one in `text`, the rest dim.
2. **The card says its keys.** One dim line inside the box's last
   row: `⏎ choose · ↑↓ move · tab next · type to answer · esc
   cancel` (multi-select: `space tick` first). Claude Code puts the
   hint under its rule; bingo's cards say their keys on the card
   (the permission card's `y/a/n` precedent) — same place.
3. **The free-text row says what it is.** `Other` becomes `Type your
   own answer` — the row's job in words, as Claude Code's `Type
   something.` does; it stays last and numbered.
4. **The preview wears a frame.** The pane is drawn inside a dim
   single-line box the width of the pane, so a mockup made of box
   characters does not run into the card's own border; stacked
   (narrow) mode gets the same frame above the options.

## Files

`bingo-surface-tui/src/form.rs` (+ its `TestBackend` catalogues in
`screens.rs`: the three `form_*` snapshots move, read each),
`docs/design/tui.md` §2/§7 (dated). Nothing outside the TUI.

## Exit criteria

- [ ] The tab row shows a box per question and a Submit tab; `⏎` on
  Submit with an open question says how many are left, not sends.
- [ ] The key line is the card's last row; the free-text row is
  named; the preview is framed.
- [ ] Every AGENTS.md gate; tui-smoke; the three form snapshots
  re-read and accepted.
- [ ] Hands-on (main session): the same prompt in tmux, captured.

## Non-goals

Notes on an answer (`n`), "Chat about this" as a separate door (a
typed answer is that door), hiding descriptions when a preview shows,
removing the border.

## Risks

- One more row for the key line on a short screen: it is the first
  row the card yields (before an answer, as §2 says of the preview).
