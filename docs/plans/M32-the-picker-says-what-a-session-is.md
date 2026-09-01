# M32 — The picker says what a session is

## Goal

A `/resume` row today reads `1. untitled · <timestamp> · <id>` — a
number, not a conversation. After this milestone a row says what the
session is about (its first ask, as its title) and how much happened
in it (a message count), and the raw id leaves the card. The two
facts land where ADR-0005 §5 already keeps derived facts — the
summary — and the journal does not grow a frame per message.

## Bricks, in build order

1. **sdk: the count and its one rule.**
   `SessionSummary.messages: Option<u64>` (`serde` default `None`;
   `None` means "never counted" — an old summary file must show
   nothing rather than a lying `0`). One pure rule, written once
   beside `ItemBody`, says which items a person reads as messages:
   their own prose and the model's answers; a tool call, a receipt,
   a notice are not messages. The fold (`SessionState::apply`)
   maintains `summary.messages` as such items land, so every client
   and the actor hold the same fresh count. Both schemas regenerate
   (`schema/rpc.json`, `schema/plugin.json`); an added optional
   field bumps no `PROTOCOL`.
2. **core: an untitled session takes its first ask as its name.**
   One mint rule: a session whose `title` is `None` and whose fold
   holds a user line takes that line's first sentence — truncated at
   a char boundary, ~48 chars — as its title, published on the
   `SessionUpdated` heads the actor already sends (open) and once
   when the first prose lands live. A session anyone named — an
   agent's seat, a `#room` — is never renamed; the mint fires at
   most once.
3. **store: fresh at rest, no new frames.** `summary::write` on
   `SessionUpdated` stays; appending a message-shaped item also
   updates the count in `summary.json` (a small read-modify-write
   beside the append — the file, not the journal, absorbs
   freshness). `rebuild` recounts from the whole journal with the
   same sdk rule, so a deleted or torn summary comes back true.
4. **TUI: the row.** `N. <title or untitled> · <n> msgs · <relative
   time>` — count shown only when `Some`, time as a person reads it
   (`2h ago`; a small helper if none exists), the raw id gone
   (`/resume <id>` still takes one for hands). Snapshot pins the
   row; the stored rows of the switcher inherit the better titles
   for free.
5. **Black-box.** A run with two exchanges, then `--continue`: the
   picker (or `summary.json` directly) carries the first ask as the
   title and the count; a summary file written before this milestone
   shows no count and no lie; the child/room titles are untouched.

## Files

`crates/bingo-sdk/src/{event,state,item?}.rs`,
`crates/bingo-core/src/session.rs` (the two publish sites),
`crates/bingo-store-jsonl/src/{lib,summary}.rs`,
`crates/bingo-surface-tui/src/view.rs` (+ a time helper),
`schema/{rpc,plugin}.json`, tests beside each. No new dependencies;
budget unchanged.

## Exit criteria

- [x] a resumed picker row reads title · count · relative time, and
      the title is the session's first ask, truncated safely
- [x] the count is maintained by the one fold rule, survives a
      summary rebuild, and an old summary shows no count rather
      than `0`
- [x] a named session (agent seat, room) is never renamed; the mint
      fires once and only where `title` was `None`
- [x] no journal grows a frame per message; both schemas
      regenerated; every gate green (fmt, check, clippy, test,
      discipline, budget unchanged, deny)

## Non-goals

Model-written summaries as titles; a `/title` command; backfilling
old summary files wholesale (an old session earns its fields the
next time its journal is written or its summary rebuilt — say so in
Carried, do not sweep the store); message counts in the switcher
rows; any new `SessionUpdated` cadence beyond the one first-prose
mint.

## Risks

R-bloat — the journal must not pay for freshness: no per-item
`SessionUpdated`; the store's file absorbs it. R-rule — the message
rule lives in ONE sdk fn used by the fold, the store's append and
the rebuild; a second counting site is the ADR-0011 debt. R-mint —
the mint must lose to every explicit name, past and future; pin
with a test where a title arrives after the first prose. R-truncate
— CJK first lines must cut at a char boundary, pinned. R-stale —
`Option` honesty: `None` renders as nothing, `Some(0)` as `0`.

## Verified

Integrated on main at `3a1d6a9` (2026-09-01): every gate green. The
first integrated run's one red was a single bingo-core lib test at
the moment an external load spike crested 83 — the run's own grep
discarded the name, a gate-template debt now fixed by capturing the
`failures:` block. The target is green solo at load 69 (204/204,
twice), and the settling full rerun is green whole: 69 targets, 0
failures on `--no-fail-fast`, load 54→26. Recorded as machine-load
flake, not regression. Budget 302, deny ok.

```
$ cargo fmt --all -- --check
fmt: clean
$ cargo check --workspace --all-targets --locked
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.74s
$ cargo clippy --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.81s
$ cargo test --workspace --locked
passed: 2887 failed: 0
$ scripts/check_discipline.sh
dependency direction ok / kernel names no tool / cohesion ok
warn crates/bingo-core/src/session.rs:121 fn handle is 62 lines (>60)
discipline ok
$ scripts/budget.sh
dependencies (unique, normal): 302 (max  302)
budget ok
$ cargo deny check
advisories ok, bans ok, licenses ok, sources ok
$ scripts/tui-smoke.sh
tui-smoke ok
```

Load averages: 8.92 before the test run, 14.71 after. Nothing flaked;
no target was rerun.

What each criterion rests on:

- **The row.** `view::picker_row` and the snapshot
  `the_session_picker_lists_what_can_be_resumed`, which pins all three
  count states in one card (`12 msgs`, `1 msg`, and a summary with no
  count showing none). `clock::ago` has its own boundaries test.
  `a_row_wider_than_the_card_is_cut_and_pushes_nothing_down` pins that
  the sheet clips a CJK name rather than wrapping it.
- **The title.** `session::title::mint`, pure, with
  `a_long_ask_is_cut_at_a_character_boundary` (CJK and ASCII) and
  `a_stop_inside_a_word_is_not_a_stop` (`1.5`, `main.rs`).
  End to end: `cli::sessions::a_resumed_session_carries_its_first_ask_
  and_what_was_said_in_it`.
- **The count.** One rule, `Event::completes_a_message`, called from
  exactly three places: the fold (`state::apply`), the store's append
  (`JsonlStore::append`) and `summary::rebuild`. Pinned by
  `a_message_is_counted_where_it_is_completed_and_only_there`,
  `the_count_in_the_file_moves_with_a_message_and_the_journal_grows_none`
  and `a_rebuilt_summary_recounts_the_whole_journal`.
- **Old summaries.** `a_file_that_never_counted_is_rebuilt_rather_than_
  guessed`, and the black-box strips `messages` from a real
  `summary.json` and continues the run: the count comes back as the
  journal's own number, not `1`.
- **The mint loses to every name.** `session::tests::naming`:
  `a_session_somebody_named_is_never_renamed`,
  `a_name_that_landed_after_the_first_ask_wins_on_resume` (the journal's
  own order decides), and `an_unnamed_session_takes_its_first_ask_and_is_
  not_renamed_again` (one `SessionUpdated`, and none on the next ask).
  Black-box: `cli::agents::the_mint_names_the_root_and_leaves_a_seat_its_
  own_name`.
- **No bloat.** The black-box counts `sessionUpdated` lines in a real
  journal after three segments and two exchanges each: four — three
  segment heads and the mint.

Decisions the plan left open:

- **Where the count is taken.** `ItemCompleted` and nowhere else. The
  store sees one frame at a time and has no item list, so the rule had
  to be per-frame; counting a started or updated item would count it
  again. A completion is also the only authoritative frame an item has.
- **`Some(0)` at birth.** `Host::summarize` seeds `messages: Some(0)`, so
  `None` means exactly "written before M32" and nothing else, and the
  store's append never has to guess.
- **The mint's one live site.** `record_inputs` and `log_input` both built
  a user item by hand; they now share `Actor::journal_prose`, so prose
  enters the journal at one place and the mint fires there.
- **A sentence ends at `.!?。！？`**, and an ASCII stop only counts where
  whitespace or the end of the line follows it — `1.5` and `main.rs`
  survive; `e.g.` would not, which is a title one word short and not a
  fault worth a parser.
- **48 characters, not 48 cells.** The plan asked for characters; a CJK
  name is therefore up to twice as wide as an ASCII one, and the card
  clips what will not fit (pinned above). Cells would be the better
  ruler if a row ever has to carry more than a name.

## Carried

- **`speaker` and `holder_of` read "no title" as "is the root".** The
  mint makes that false, so both now read the fact — `parent.is_none()`.
  They are the same rule in two plugins that may not import each other
  (`bingo-agents::names::speaker`, `bingo-rooms::name::signed_by`); the
  duplication predates this milestone and is structural. If a third
  reader appears, the rule wants a home the sdk can hold.
- **Old sessions are not swept.** A summary written before this
  milestone earns its count the next time its journal is appended to or
  its summary rebuilt; until then its row shows a name if it has one and
  no count. This is the plan's non-goal, restated as what is true on
  disk.
- **The live session appears in its own `/resume` card**, now reading
  `untitled · 0 msgs · just now`. That is M31's listing, not this
  milestone's row, but the count makes it easier to notice. Worth a
  filter.
- **Two files crossed the 700-line advisory**: `bingo-sdk/src/event.rs`
  (683 → 706, the two rules) and `crates/bingo/tests/cli/agents.rs`
  (→ 714). Warnings, not failures; `event.rs` is the one to watch, since
  it owns the whole vocabulary and every addition lands there.
