# M55 — The list that is searched

## Goal

Two asks from the user (2026-09-04): the `ctrl+g` list of agents and
rooms should be searchable the way the `/model` dropdown is — type and
the rows narrow — and the matching everywhere should stop being a
prefix test. Today three lists match three ways: `@` mentions rank
with `nucleo` (an editor's fuzzy finder, already a dependency);
`/` commands and their catalogue arguments (`/model`, `/resume`, …)
take prefix matches first and substring matches second
(`commands::rank`, `arguments`); the switcher has no filter at all.
One matcher, one filter behaviour: `nucleo` for all three, and the
switcher gains a query line.

## Bricks

1. **One matcher.** A `matching.rs` (or `complete::rank` promoted)
   with one function `rank<'a, T>(query, items, key: impl Fn(&T) ->
   &str) -> Vec<&'a T>`: nucleo's `Pattern::parse` with smart case and
   Unicode normalization, scored, ties broken by the items' own order
   (catalogue order, roster order); an empty query returns everything
   in order. Pure; tests: subsequence (`mdl` finds `model`), a later
   word (`sonnet` finds `anthropic/claude-sonnet-5`), a typo does not
   match, ranking prefers the tighter match, and stability on ties.
   `commands::rank` and `arguments` read it — their prefix/substring
   code is deleted, their tests re-aimed to the new order where it
   changes (say which in Verified).
2. **The switcher's query.** `Switcher` gains `query: String`. A
   printable key appends, backspace removes, and the roster's rows
   are ranked by the query over the name and, for a room, its
   members' names and its topic (whatever `tree::roster` already
   reads for the row); labels (`Agents`, `Rooms`) stay only over a
   group with a surviving row; the cursor stays on its session when
   it survives the filter, else moves to the first row; the walk
   still switches the view. `esc` with a query clears the query; `esc`
   with none puts back the session it was opened from, as today. The
   query is drawn as one dim line at the list's head (`⌕ sonn▏`), the
   list's width, and is nothing when empty — the one-line-of-furniture
   rule holds because the line is the person's own typing. Design
   §3's "typing returns the line" was the old chip strip's; the doc's
   Teams entry gets a dated line saying the list is typed into now.
   `TestBackend` tests: a query narrows the rows and keeps the cursor
   on its session; `esc` twice; `↓` on an empty composer opens the
   same list with the same query behaviour.
3. **`↓`'s double meaning.** `↓` on an empty composer opens the list;
   inside it `↓` walks. Unchanged, but assert it once with a query
   present.

## Files

`bingo-surface-tui/src/{matching.rs,complete.rs,commands.rs,ui.rs,
input.rs,roster.rs,tree.rs}`, `docs/design/tui.md` §3/§4 (dated).
`run.rs` untouched.

## Exit criteria

- [ ] `/mo` and `/mdl` both offer `/model`; `/model son` offers every
  model with `son` as a subsequence, tightest first.
- [ ] `ctrl+g`, typing `rev` narrows to `reviewer`'s row; `esc` clears;
  `esc` again closes and restores the view.
- [ ] Every AGENTS.md gate; budget 331 (nucleo is in the tree);
  tui-smoke.
- [ ] Hands-on (main session with the user).

## Non-goals

Searching the transcript (`ctrl+f`, `search.rs`, is a different thing
and stays). A history of queries. Matching over a session's transcript
text.

## Risks

- Ranking reorders the `/` dropdown against today's snapshots; the
  snapshots that change must be read one by one — a row moving is the
  point, a row vanishing is a bug.
