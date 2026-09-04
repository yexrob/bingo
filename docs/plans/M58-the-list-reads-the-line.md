# M58 — The list reads the line

## Goal

M55 gave the `ctrl+g` list a query line of its own at its head. The
user, having used it (2026-09-04): no — one rule for every list in
the TUI: **what you type goes in the input box, the list narrows on
it, `tab` completes it, `⏎` takes it.** That is already how the `/`
and `@` dropdowns work; the session list should be the same shape,
not a list with a search box bolted on.

## Bricks

1. **The query is the line.** `Switcher.query` is deleted. On open
   the composer's draft is set aside (`Switcher.draft: String`, taken
   with `Composer::take`) and the box is empty; whatever is typed lands
   in the box as anywhere else, and the list is ranked by
   `ui.composer.text()` through the one matcher (`matching::rank`).
   On close — `⏎`, `esc`, a click — the draft is put back exactly as
   it was (`Composer::set`, caret at its end, as history recall does).
   The `⌕` query row and its glyphs go (the `Glyphs` fields M55 added
   are removed again; `search.rs`'s caret keeps its glyph if it still
   needs one — check).
2. **`tab` completes.** With the list open, `tab` puts the row under
   the cursor's name into the box (a session's name; a room's `#name`)
   — the way `tab` on the `/` dropdown completes the command — and the
   list narrows to it. `⏎` then, as now, keeps the session the walk
   landed on and closes; with a name completed and nothing else, `⏎`
   switches to that session. `esc` with text in the box clears the
   box first (the query), then closes and restores the draft and the
   view it was opened from.
3. **`↓` on an empty box** still opens the list; with text in the box
   `↓` walks it. The typed-into list is the same list either door
   opens (§3 "one list, two doors").

## Files

`bingo-surface-tui/src/{ui.rs,input.rs,roster.rs,view.rs,theme.rs}`
(the M55 files, minus the query row), `docs/design/tui.md` §3/§4/§7
(the M55 dated line is amended, dated again), the two
`switcher_query_*` snapshots (re-read: the query row gone, the box
holding the text).

## Exit criteria

- [ ] `ctrl+g`, typing `rev`: the text is in the input box, the list
  shows `reviewer`'s row; `tab` completes the name into the box; `⏎`
  switches; the draft that was in the box before is back after.
- [ ] `esc` twice: clears, then closes and restores view and draft.
- [ ] No query row anywhere; `Glyphs` back to its M55-minus-two
  fields.
- [ ] Every AGENTS.md gate; tui-smoke.
- [ ] Hands-on (main session with the user).

## Non-goals

Completing a session name into a `@mention` for sending (that is the
`@` dropdown's). Fuzzy-matching the transcript.

## Risks

- A draft set aside and a crash between: the draft is in memory only,
  as it is today while typing. Accept.
