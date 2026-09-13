# M91 — The context an agent holds

## Goal

User, 2026-09-10, looking at the Nilbo `claude-acp` sessions: "bingo不帮acp做
compact，但是需要监听下acp compact的消息，正确显示到tui上面". The kernel measured
an agent's context with the turn's bill (2 367 503 tokens) against the
unknown-model window (191 808), asked for compactions the provider refuses,
tripped the breaker every turn, and showed `2,367,503/191,808 · /compact`.
After this milestone (ADR-0055): an endpoint that holds its context is not
judged by the kernel's ruler; its own reading is what the status line shows,
against the window it names; a compaction it made is a compaction row in
every surface; `/compact` on such a session is refused in words.

Facts the plan rests on (from `claude-agent-acp` 0.73 and Claude Code
2.1.267): the adapter sends `usage_update { used, size }` after every result
message and again right after a `compact_boundary`; `size` is the model's
real window (1 000 000 for a `[1m]` model, from the SDK's `modelUsage`).
Claude Code auto-compacts on its own side at `window − min(max_output,
20 000) − 13 000` — 967 000 of a million. Around a compaction the adapter
sends three `agent_message_chunk`s: `Compacting...`, `\n\nCompacting
completed.`, `\n\nCompacting failed<reason>`.

## Bricks, in build order

1. `bingo-sdk`: `EndpointCapabilities { holds_context: bool, context_window:
   Option<u64> }` (both `serde(default)`), `ModelCapabilities.holds_context`;
   `ModelEvent::Context { used: u64, window: u64 }` and
   `ModelEvent::Compacted { before: u64, after: u64 }`; the doc on
   `ContextUsage.trigger`: "equal to `window` where the kernel draws no
   line". Serde fixtures for the two variants; `schema/rpc.json` regenerated
   if any of these types is in it.
2. `bingo-core/models/resolve.rs`: the window is `endpoint.context_window`,
   else declared, else learned clamp, else catalogue; `holds_context`
   composed in. Property test amended.
3. `bingo-core/context/budget.rs`: `Thresholds::held(window)` — `effective:
   window, warn: 0, trigger: window, keep: 0`; `ruler.rs`: `Ruler::new`
   takes `held`, `Ruler::reading(used, window)` replaces lines and anchor,
   `measure` on a held ruler returns the last reading and adds no estimate.
4. `bingo-core/accumulator.rs`: `Context` folds into `Finished.context:
   Option<(u64, u64)>` (the last reading wins); `Compacted` emits the item
   of ADR-0055 §3 through the accumulator's own item path. `turn.rs`:
   `account` prefers the reading; `assemble` skips `try_compact` for a held
   ruler; `stream.rs`'s overflow ladder is not climbed when held (the error
   fails the turn as any other); `session/mailbox.rs` `compact` refuses a
   held session with `KernelError` "`<provider>` holds this context and
   compacts it itself; bingo does not". `context.rs`: a `Compaction` with an
   empty summary folds to no note (property test: the fold of a journal
   with such an item equals the fold without it).
5. `bingo-provider-acp`: `endpoint(model)` says `holds_context: true` and the
   last `size` read for that model (a `Mutex<BTreeMap<String, u64>>` beside
   `images`); `events.rs` `Mapper`: `UsageUpdate` → `Context`, preceded by
   `Compacted { before, after }` when `used` fell; the two banners dropped
   as whole chunks, matched exactly; the failure banner passes. Fixtures:
   the recorded adapter frames of a compaction, a `codex-acp` shape with no
   banner. The `asking` refusal of a purposed request stays.
6. `bingo-surface-tui`: no rendering change; one `TestBackend` test folds an
   ACP-shaped stream (window 1 000 000, a fall) and asserts the status
   line `…/1,000k` and the `context compacted (… → … tokens)` rule.
7. `bingo-provider-acp/src/guide.md`: the "no compaction" bullet becomes the
   agent's own compaction and the refused `/compact`; owner test names
   `/compact`. Black-box (`bingo/tests/cli/acp*.rs`, the fake agent): a
   `--print --output-format stream-json` run whose fake agent sends
   `usage_update {used: 400000, size: 1000000}` then `{used: 120000, …}`
   shows `TurnUsage.context.window == 1000000` and one compaction item;
   `/compact` in that session answers the refusal, exit 0.
8. Docs: ADR-0055 (written), ADR README line, ADR-0035 first consequence and
   ADR-0006 §2 each gain an "amended by ADR-0055" clause.

## Files

- `bingo-sdk/src/{model,event}.rs`, `schema/rpc.json` if touched.
- `bingo-core/src/{accumulator,turn,context}.rs`,
  `bingo-core/src/turn/{ruler,stream}.rs`, `bingo-core/src/context/budget.rs`,
  `bingo-core/src/models/resolve.rs`, `bingo-core/src/session/mailbox.rs`.
- `bingo-provider-acp/src/{provider,events,events_tests,fixtures,guide}.rs`,
  `guide.md`, `bin/fake_agent`.
- `bingo-provider-anthropic`, `bingo-provider-openai`, `bingo-provider-fake`,
  `bingo-core/src/turn/late.rs`: the two new fields, `false` and `None`.
- `bingo-surface-tui/src/status.rs` or `transcript.rs` tests only.
- `bingo/tests/cli/`, `docs/adr/{README,0006,0035}.md`.

## Exit criteria

- [x] `resolve`: an endpoint window of 1 000 000 wins over a declared
      200 000; `holds_context` carried; the property test holds
- [x] a held ruler never triggers `try_compact` and never warns; `/compact`
      on a held session is refused with the provider's name; a scripted
      `ContextOverflow` on a held session fails the turn without a ladder
- [x] `Context { used: 412000, window: 1000000 }` in a round makes that
      round's `TurnUsage.context` read `412000 / 1000000 / 1000000`,
      whatever the bill said
- [x] `Compacted { before, after }` records one `Compaction` item with an
      empty summary and no `Event::Compacted`; the fold writes no note
- [x] provider-acp: the recorded compaction frames yield `Compacted` then
      `Context`, and neither banner reaches a `TextDelta`; the failure
      banner does; a second turn's `endpoint()` names the size
- [x] black-box stream-json shows the window and the item; `/compact`
      answers the refusal; the `guide-acp` owner test names `/compact`
- [x] every gate green; Windows check for `bingo-provider-acp`, `bingo-core`

## Non-goals

- No forwarding of `/compact` to the agent and no ACP slash-command
  mapping (ADR-0035 §6 stands).
- No change to how the anthropic, openai or fake provider is measured.
- No kernel warning for a held context: the agent warns and compacts on
  its own lines, and the status line's warmth already climbs.
- The fallback transcript is not cut when the agent compacts: the journal
  lost nothing, and a replaced child gets the whole of it.

## Risks

- The two banners are `claude-agent-acp`'s spelling; a version that changes
  them lands as prose again, which is the state today, not a regression.
  The fixture pins the spelling this milestone read.
- A `usage_update` whose `used` is the adapter's `0` fallback (its own
  comment: "directionally correct") reads as a compaction to 0 tokens; the
  next reading corrects the line. Accepted: the row says what was said.
- `endpoint()` is read at turn start: the first turn of a fresh session has
  no size yet and measures against the catalogue's unknown window until
  the first reading arrives mid-turn; the first `TurnUsage` is already
  right.

## Verified (2026-09-10, dev, `c0489785`…`1b951461`)

One `opus-xhigh` worktree cut from `5d819299`; dev did not move under it,
so the worker's gates ran on the merge tree, and clippy, the touched
crates and the CLI black-box were run once more before the ff merge:

```
cargo fmt --all -- --check                                      ok
cargo check --workspace --all-targets --locked                  ok
cargo clippy --workspace --all-targets --locked -- -D warnings  ok
cargo test --workspace --locked --no-fail-fast                  ok (4648 passed, 2 ignored, no flake)
cargo test -p bingo --locked --test cli                         ok (223)
scripts/check_discipline.sh                                     discipline ok
scripts/budget.sh                                               budget ok (335)
scripts/tui-smoke.sh                                            tui-smoke ok
cargo check -p bingo-provider-acp -p bingo-core --all-targets --target x86_64-pc-windows-msvc  ok
BINGO_UPDATE_SCHEMA=1 cargo test -p bingo-surface-rpc / -p bingo-plugin-rpc   schemas regenerated, committed
```

Decided on the way, against the plan's letter: a refused `/compact` exits
1 like every rejected command (`INVALID_INPUT`), not 0; the black-box reads
`--output-format json`, the raw frames, since `stream-json` carries no
`TurnUsage`; `context_window` is `skip_serializing_if` so a recorded
`ProviderSpec` shape does not grow a `null`; a held round that hears no
reading keeps the last reading rather than falling back to the bill; the
session-long reading lives on the ACP `Link`, the per-model window in
`provider-acp/src/windows.rs`; the kernel's new tests are
`turn/tests/held.rs` because `turn/tests.rs` is at the file cap.

Not verified: no live `claude-agent-acp` run — the banner spellings and
the `usage_update` shape are pinned from the adapter's source into
fixtures. A live drive of a `[1m]` session past 967 000 tokens is the
one check left.
