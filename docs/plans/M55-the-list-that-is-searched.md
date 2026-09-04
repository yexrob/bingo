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

- [x] `/mo` and `/mdl` both offer `/model`; `/model son` offers every
  model with `son` as a subsequence, tightest first.
- [x] `ctrl+g`, typing `rev` narrows to `reviewer`'s row; `esc` clears;
  `esc` again closes and restores the view.
- [x] Every AGENTS.md gate; budget 331 (nucleo is in the tree);
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

## Verified

2026-09-04, on `m55-search` off dev `5c063cd7`. One commit per brick, in the
plan's order: `bbab7f2` (1, the matcher), `169aacd` (2, the switcher's query),
`ac0348b` (3, `↓`'s two meanings asserted with a query up).

### What landed

**1 — one matcher.** `crates/bingo-surface-tui/src/matching.rs`, 42 non-test
lines and one function: `rank<'a, T>(query, items: &'a [T], key: impl Fn(&T)
-> &str) -> Vec<&'a T>` — `Pattern::parse` with `CaseMatching::Smart` and
`Normalization::Smart`, `Pattern::score` per item, and a *stable* sort by
`Reverse(score)`, which is what leaves the list's own order under the score.
An empty query is nucleo's own answer (`score` returns `Some(0)` for a pattern
with no atoms), not a branch of ours. Eight tests: subsequence, a later word,
a typo, the tighter match first, an empty query, ties, smart case, and that the
key is what is matched.

`commands::rank` and `commands::arguments` read it and their prefix/substring
code is deleted. Ranking a command needed one thing the plan did not mention:
a `CommandSpec` may be typed under its name *or* an alias, and today's code
offered one row per spec under whichever spelling matched first. So
`commands::spellings` flattens each spec into `(spelling, spec)` pairs, the
whole flat list is ranked once, and a spec already offered is skipped — one
row per command, under the spelling that matched best. `complete::rank` is now
four lines over `matching::rank`.

**Two configs became one.** `complete::rank` used `Config::DEFAULT.match_paths()`
and `CaseMatching::Ignore`; the matcher uses `Config::DEFAULT` and smart case
for names and paths alike. `match_paths` only re-weights the boundary bonus and
drops `,:;|` as delimiters — `/` is a delimiter in `DEFAULT` too — and the old
code already ranked *names* through the path config, so one config is the
smaller thing as well as the truer one. Smart case is a real behaviour change
at the edge: `@CAR` no longer finds `Cargo.toml` (`@car` and `@Car` still do).

**2 — the switcher's query.** `Switcher` gains `query: String` and a
`session(&self, tree, cursor)` method — the list composed on the spot from the
tree, the store's answer and the query, so a click and a keypress cannot be
reading two different lists (`pointer::under` is now one line over it).
`roster::listing(tree, rows, query)` narrows each run through the matcher;
`roster::searched` is the pure brick that says what a row is found by.
`roster::asked` is the query line and `roster::headed` puts it at the head with
`None` for its click, so `Listed::at` keeps working; the window is asked for one
row fewer when the line is there, so the room asked for is the room drawn.

In `input.rs`: `queried` says what a key leaves the query as (a printable char
appends, backspace pops, an empty query answers no backspace so `esc esc` is
not stolen), `narrow` applies it, `placed` re-places the cursor, and `settle`
gained the query rung of §7's `esc` stack. `narrow` ends in the same `walk_to`
the arrows use, so the view follows the cursor on a keystroke exactly as it
does on a walk — without that, `⏎` would keep a session the cursor had left.

**3 — `↓`'s two meanings.** Unchanged code;
`input::tests::down_opens_the_list_and_then_walks_what_the_query_left` asserts
both halves with a query up.

**The glyph table gave up a field to make room.** `Glyphs` was at 15 fields and
the cap is 16 (`scripts/check_discipline.sh`'s cohesion check); the query line
needs two glyphs, a mark and a caret. `rule` came out: its own doc said it was
"a rule between blocks, **and the fill of a box's edge**", and it was `"─"`
beside `border::ROUNDED.horizontal_top` `"─"` in one table and `"-"` beside
`"-"` in the other — one fact spelled twice. `theme::rule()` now reads
`glyphs().border.horizontal_top`, and the ASCII test asserts it is still `-`.
The two new fields are `find` (`⌕` / `/`) and `caret` (`▌` / `|`); `search.rs`
stopped hardcoding `"▌"` and reads `theme::caret()`, which is also how the
transcript search row gained an ASCII caret it never had.

### What the plan got wrong

1. **"for a room, its members' names and its topic."** A room has no topic in
   this surface — nothing publishes one and nothing draws one — so there was
   nothing to match. And the direction is reversed from what the plan says: a
   **session** is found by the room it sits in, a **room** by its name alone.
   Matching a room on its members would mean typing `rev` brings up `#design`
   as well as `reviewer`, which is noise; the seats are rows of their own, so
   typing `design` already brings the room *and* every seat in it up together —
   both directions, one traversal, no room learning its members' spellings.
2. **The Files list is short by three.** `view.rs` (one argument — the query
   reaches `roster::lines`), `pointer.rs` (a click must resolve against the
   narrowed list, or it lands on a row nobody can see) and `theme.rs` (the two
   glyphs) had to change; `motion.rs`, `screens.rs`, `search.rs` and `lib.rs`
   are the call sites and the module registration.
3. **The Risks section's snapshot reordering did not happen.** Not one existing
   snapshot or assertion moved — the whole suite passed unchanged after brick 1
   (790 tests, before the new ones). The `/` dropdown's snapshots are of a bare
   `/`, which is an empty query, and the only ranked assertion was
   `the_dropdown_ranks_prefixes_before_substrings`, whose two cases nucleo
   answers identically (`/co` → `/compact`; `/o` → both, equal scores, so
   catalogue order decides). It was re-aimed to
   `a_subsequence_of_a_command_offers_it` and gained `/mo`, `/mdl` and a typo.

### Snapshots

Two new (`switcher_query_80x24`, `switcher_query_120x40`); **no existing
snapshot changed**. The new pair is the list narrowed by `rev`:

```
"⏺ reviewer(review the diff)                                                     "
"  ⎿  Running… 3 tools · 1.2k tokens                                             "
"⌕ rev▌                                                                          "
"❯ ⏺ reviewer  running · 3 tools · 1.2k tokens                                   "
"╭──────────────────────────────────────────────────────────────────────────────╮"
```

### Not verified

- **Hands-on** (left for the main session, as briefed).
- **Windows.** Not cross-checked: the TUI cannot be checked for another target
  locally (ADR-0041's note), and nothing here is platform-shaped — no process,
  path, signal or clock is touched.
- **The store's answer landing on a typed list.** `run::fill_switcher` sets
  `open.stored` without re-placing the cursor, so a stored row arriving can
  shift what the cursor names. Pre-existing (the stored rows already shifted the
  rooms run's indices); a query widens the window a little, because a ranked
  run can take the new row anywhere. Left alone: it wants its own decision about
  whether the cursor should chase its session there, and the read lands within
  a frame or two of the opening, before anything is typed.
- **`input.rs` is at 935 non-test lines** (warn at 700, fail at 1000). The
  switcher's keys are the obvious module to lift out and the brief pinned this
  file to the switcher's keys for a clean merge, so it was not done here.

### Gates

```
$ cargo fmt --all -- --check
=== fmt exit 0

$ cargo check -j 2 --workspace --all-targets --locked
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 56.11s
=== check exit 0

$ cargo clippy -j 2 --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 32.53s
=== clippy exit 0

$ cargo test -j 2 --workspace --locked   # tee'd to target/m55-test.log
79 `test result: ok` lines, 3639 passed, 0 failed, no rerun needed
=== test exit 0

$ scripts/check_discipline.sh
kernel names no tool
cohesion ok
warn crates/bingo-core/src/session.rs:129 fn handle is 66 lines (>60)
discipline ok
=== discipline exit 0

$ scripts/budget.sh
dependencies (unique, normal): 331 (max  331)
warm cargo check -p bingo-core: 0s (max  20s)
relink isolation: touching the TUI recompiled 0 crates for core (must be 0)
target/debug: 7 GB (soft max  5)
warn: target/debug exceeds the soft limit
budget ok
=== budget exit 0

$ cargo deny check
advisories ok, bans ok, licenses ok, sources ok
=== deny exit 0

$ TMUX_TMPDIR=$(mktemp -d) scripts/tui-smoke.sh
  ...
  ctrl+g opens the one list of sessions and esc closes it
  ...
tui-smoke ok
=== smoke exit 0
```

The smoke needed the private `TMUX_TMPDIR`: the script's socket and session
names are fixed (`-L bingo`, `smoke`) and it opens by killing that server, so
two workers running it at once drive one pane — the first two attempts died
that way (a killed server, then a second `say hello` and `script exhausted`).
`TMUX_TMPDIR` isolates the socket *directory* without touching the script.
Worth fixing in the script one day; not this milestone's.
