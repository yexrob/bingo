# M79 — A run of thoughts is one thought

## Goal

User, 2026-09-06, with a screenshot: "会有这种连续的思考 我感觉可以合并
成一个". The journal behind it (`Free`/`gpt-6-astra`, the Responses API)
has, after a failed `Bash`, **ten reasoning items in a row** — 57 to 149
characters each, 13 to 31 s each, nothing between them — then the answer.
The endpoint closes a reasoning item and opens the next as the model goes
on thinking; each carries its own encrypted state and replays as its own
item, so the kernel keeps ten. The screen showed ten `✻ Thought for Ns`
rows; with M78 it would show ten blocks of three rows. A person reads
that as one thought that took three and a half minutes, and that is what
the transcript should say.

Surface only, at render time (ADR-0002): consecutive thoughts — reasoning
items that are not an ACP agent's calls, adjacent in `state.items` with no
other item between — draw as **one block**. Its heading is `✻ Thinking…`
while the last of them is still being had and `✻ Thought for 3m 21s` (the
sum of their times; `Ns` under a minute, `Mm Ns` from one) once it is over;
its body is the run's texts joined by a blank line — the same `thought_rows`
at every fold, so the tail is the newest of the whole run and the whole
opens in place and in the sheet. One fold, one click, one `ctrl+o` for the
run. No kernel, journal, `--print` or RPC change.

## Bricks

1. **`thoughts.rs`** (new; the surface's one place that knows a run):
   `is_thought(item)` (reasoning, not `acp::is_call`); `run_before(items,
   i) -> &[Item]` the thoughts adjacent before index `i`; `next_is_thought
   (items, i)`; `text(run) -> String` (non-empty texts joined by `\n\n`);
   `took(run) -> SignedDuration` (sum of `completed_at − started_at` over
   the finished ones); `run_of(state, id) -> Vec<&Item>` the whole run an
   item belongs to. Pure; unit tests for each, including a tool call that
   breaks a run and an ACP call that does.
2. **`transcript`**: the run's block hangs on its **last** item. A thought
   with a thought after it draws nothing (its lines are the next one's);
   the last draws the heading over `thought_rows(thoughts::text(run),
   fold, width)`. `took` gains minutes. `item_block` learns the item's
   place — `previous`, whether the next is a thought, the run before it —
   in one small struct rather than three loose arguments.
3. **`blocks`**: `sync` hands each item its place; `Revision` carries
   `run: usize` (thoughts before it in its run) and `last: bool` (no
   thought after it), so a block that was a run's last is drawn again the
   frame a new thought starts and a run's block is drawn again as the run
   grows. Tests: a second thought joins the first's block (one block, one
   gap); a tool call between them keeps two; the run's block's id is the
   last thought's.
4. **`fold`, `pointer`, `input`, `pager`**: a click on the run's rows lands
   on the last item (`Blocks::at`), `cycled(last, …)` answers for the run
   (`shuts` reads the last item — the run is over when it is); `ctrl+o`'s
   `latest` finds the last item as it does; the pager's `lines` for a
   thought renders `thoughts::text(run_of(state, id))`, its title stays
   `Thinking`. Tests: a click cycles one fold for the whole run; the sheet
   shows every thought of the run.
5. **Screens** (`screens/thinking.rs`): a run of three finished thoughts
   (`reasoning_run`, both sizes: one heading with the summed time over two
   rows); the same run with the third still being had
   (`reasoning_run_streaming`). Every other snapshot byte-identical.
6. **Docs**: design §4 thinking row and §6 (a run is one block), a dated
   §10 entry; this plan.
7. **Hands-on** in a harness-owned tmux (`-L fable-m79`, never the user's
   terminal): the fake script with three `reasoning` steps separated by
   `delay` steps, then `text`; captures every 100 ms; the transcript shows
   one `✻` block for the three, its heading summing their times.

## Files

`crates/bingo-surface-tui/src/{thoughts.rs (new), lib.rs, transcript.rs,
blocks.rs, fold.rs, pointer.rs, input.rs, pager.rs, screens/thinking.rs}`,
four new snapshots, `docs/design/tui.md`.

## Exit criteria

- [ ] Drawn tests: two adjacent finished thoughts of 2 s and 3 s draw
      `✻ Thought for 5s` over the last two rows of their joined text and
      nothing else; a running third makes it `✻ Thinking…` over the tail;
      a tool call between two thoughts gives two blocks; a run of 70 s and
      131 s reads `3m 21s`.
- [ ] A click on the run's rows opens the whole run in place; `ctrl+o`
      twice opens the sheet with every thought's text.
- [ ] `reasoning_run*` snapshots at both sizes; no other snapshot changed.
- [ ] fmt, check, clippy `-D warnings`, `cargo test -p bingo-surface-tui`,
      discipline, budget in the worktree; the full workspace test and
      `tui-smoke.sh` at the merge; the hands-on captures pasted.

## Non-goals

- No merging in the kernel, the journal, `--print` or the RPC stream: the
  items stay ten, each with its own replayable metadata.
- Thoughts with anything drawn between them stay apart — a call that
  draws no row (a task call, M74) is still an item between them, and this
  plan does not look through it.
- No change to what a single thought draws (M77, M78).

## Risks

- The block's id moves to the newest thought as a run grows, so a fold a
  person set on the run while it was still growing starts over at the
  peek — the newest thought is the one arriving, and the peek is what it
  shows anyway.
- `Revision` equality is what stops a terminal block from being redrawn;
  `last` in it is what makes a former last draw again as nothing. A test
  pins that the old block goes and no gap is left.

## Verified

2026-09-06, dev `72a22fc3` (opus-xhigh worker in `.claude/worktrees/m79`,
merged fast-forward as `4a446d2f` + `39b3221b`; the 0.5.4 bump follows).
Exit criterion 1 ran red before the change (two thoughts drew two blocks
with a gap, a running third drew three, `70s` stood where `3m 21s` was
expected) and green after; the click, `ctrl+o` and sheet tests, the
`blocks` test that a joined thought gives its slot back with no stray
gap, and seven `thoughts` unit tests pass. Snapshots: `reasoning_run_*`
and `reasoning_run_streaming_*` new at both sizes, nothing else changed.
Beyond the plan: the thought's rows moved to `transcript/thinking.rs`
(`transcript.rs` would have passed the 1000-line fail line), `deepen`
walks past a run still being had, and `pager::lines` takes the state.
Gates on dev after the merge and the bump:

```text
== fmt / check / clippy (-D warnings)   exit 0
== test       86 suites, 4241 passed, 0 failed, 2 ignored (--no-fail-fast)
== discipline discipline ok (pre-existing warns only)
== budget     budget ok — dependencies unchanged (334)
== deny       advisories ok, bans ok, licenses ok, sources ok
== smoke      tui-smoke ok
```

Hands-on, harness-owned `tmux -L fable-m79` at 80×24, three `reasoning`
steps with 1.5 s delays then the answer, 90 captures at 100 ms: every
capture with a thought on it has exactly one `✻ Thought`/`✻ Thinking`
row, two rows of the joined text under it, through the run and after
the answer. The worker's own drive read the same. The summed heading is
pinned by the unit tests and the `reasoning_run` snapshots (`1m 21s`);
the fake closes each block within a millisecond, so a live drive reads
`<1s`. Not exercised live: the endpoint that produced the ten items.
