# M80 — Stable request prefixes and deliberate shell waits

## Goal

Keep cached prefixes stable without turning internal context into conversation.
After the Claude Code / DeepSeek comparison, the user requires no internal-context
rows, folded injections or notices in ordinary UI. Use stable knowledge baselines,
necessary runtime updates and shared-prefix compaction. Preserve deliberate shell
waits and the already-tested affinity/usage fixes.

## Bricks

1. Source attribution depends on the item itself, never later items in its turn.
2. Internal context retains explicit provenance in the journal/model projection,
   but is absent from ordinary UI, including expanded transcript mode.
3. Project instructions and memory have stable knowledge baselines with explicit
   refresh boundaries; do not broadcast complete indexes on each file change.
   Runtime updates are only necessary changes, with clear supersession semantics.
4. Normal requests preserve tool-result history. Result elision remains an
   overflow-recovery projection, alongside the existing full compaction safety.
5. OpenAI encodes a stable session-derived `prompt_cache_key` for conversational
   requests and accounts for cache-write tokens without double-counting input.
6. Shell guidance chooses foreground/background by dependency, not duration.
   Foreground remains the default; existing endless-command/promotion rules stay.
7. Summarization reuses the parent request's system/tools/history prefix and adds
   an instruction at the tail, with a bounded fallback for genuine overflow.

## Files

- `bingo-sdk`: source classification, snapshots and request-bearing compaction contract.
- `bingo-core`: attribution, request assembly, compaction safety/accounting and tests.
- `bingo-surface-tui`: omit internal context, including previews and rewind prompts.
- `bingo/tests/cli`: baseline lifecycle and full compaction-to-wire integration.
- `bingo-context`: extension-backed baselines, lifecycle hook and prefix summarization.
- `bingo-tasks`, `bingo-experience`: changed runtime context and tests.
- `bingo-provider-openai`: cache affinity, usage normalization and HTTP fixtures.
- `bingo-provider-acp`, `bingo-plugin-rpc`: fail-closed side request/wire contracts.
- `bingo-tool-bash`: model-facing guidance and foreground/background contract tests.
- `docs/adr/0048-stable-request-prefixes.md`, relevant existing ADR amendments.

## Exit criteria

- [x] Public snapshot-helper tests pin changed/unchanged/resumed behavior before
      implementation, using the existing User contribution wire form.
- [x] Peer arrival never rewrites previously folded messages.
- [x] Production turn assembly keeps System/history unchanged when a snapshot changes;
      unchanged snapshots add nothing, cleared states arrive, and resume/compaction
      derive the latest visible snapshot from journal items, not a private cache.
- [x] Knowledge baseline lifecycle is tested through production assembly; unchanged
      baselines are not reread/repeated, refresh is explicit, runtime state stays fresh.
- [x] Summarization wire preserves parent prefix/header and appends its instruction;
      overflow fallback, manual compact, accounting and output bounds remain covered.
- [x] Normal long turns preserve old results; overflow recovery remains bounded.
- [x] HTTP mock sees identical keys for one session, distinct keys for distinct
      sessions, no invented key for side requests, and caller overrides respected.
- [x] Read/write/fresh token counts sum to upstream input; old fixtures still work.
- [x] Bash foreground default/explicit false wait for exit and return status/output;
      explicit background returns a job. Guidance requires observing prerequisites.
- [ ] fmt, workspace check/clippy/test, discipline, budget and cargo-deny pass.
- [x] Windows cross-check is attempted for changed process/path-facing crates;
      missing target/toolchain or other limitations are reported explicitly.
- [x] Internal context neither names/counts as user speech nor appears in ordinary
      or expanded UI; TestBackend/PTY tests preserve user/tool/agent visibility.
- [x] Independent review reports no blocking regression.

## Non-goals

No production configuration changes, paid cache experiments or pushes.
A local commit requires explicit user authorization.
No new dependencies. Do not automatically enable new OpenAI explicit-breakpoint
fields on unknown compatible endpoints. No speculative TTL/effort protocol change.
No changes to unrelated worktrees or the user's global instructions.

## Risks

- Stable attribution intentionally changes prompt snapshots once on upgrade.
- State snapshots append context; compaction remains necessary and must refresh
  a snapshot when its earlier record leaves visible context.
- Keeping normal tool results uses more context; the existing full-compaction
  threshold and overflow ladder remain, and realized cost is not yet measured.
- Cache affinity improves routing information but does not guarantee backend hits.
- Plugin protocol 6 is incompatible with version-5 peers; bundled examples update,
  external plugins must upgrade. CLI and public surface RPC contracts stay unchanged.
- Agent guidance can be tested as supplied text and tool behavior, not as a promise
  that every model will always make the right scheduling choice.

## Verification budget

One red/green loop per behavior, focused checks during implementation, one full
workspace validation followed by at most two in-scope repair/review rounds.
Stop on an external/toolchain blocker and preserve the work with its evidence.

## Verification — 2026-09-07 (workspace tests blocked)

Production CLI/HTTP tests are distinct from isolated helper tests:

```text
cargo test -p bingo --test cli --locked prefix::
  4 passed: baseline retention/resume, title, valid/rejected summary flows
cargo test -p bingo-surface-tui --locked internal_context_
  2 passed: hidden transcript/expanded view and preview/rewind exclusions
cargo test -p bingo-surface-tui --locked internal_extension_namespaces_never_become_panels_or_pinned_cards
  1 passed: new and legacy namespaces hidden; raw state and public Board retained
cargo test -p bingo --test cli --locked the_agents_session_id_is_journaled_once_as_an_extension
  1 passed: ACP pointer still exactly once, excluding unrelated baseline records
cargo test -p bingo --test cli --locked an_overflow_after_many_rounds_is_summarised_and_the_turn_goes_on
  1 passed: explicit side-lane summary and normal Recovered response
scripts/tui-smoke.sh (isolated TMUX_TMPDIR, API credentials removed)
  tui-smoke ok, including no internal rows and normal Board picker
```

The CLI tests run real actors, built-in baseline hooks and SummaryCompactor through
mock HTTP. They cover within-turn file edits, reopen, successful manual compaction,
failed tool-call summaries without execution/refresh, and fresh post-cut baselines.
The core replay test uses real Compacted events but a scripted compactor/provider.
The TUI transcript check uses TestBackend; panel/pin tests inspect projections.
Worker evidence includes native request/usage/cancellation tests and 42 real plugin
process tests, including v5 rejection and exact declared-error usage transport.
Those bridge tests are not a full remote-compactor/core-billing integration test.

Both source-review tracks approved after the boundedness/protocol repairs. Review
also caught panel leakage from baseline extensions; the private namespace contract
and its legacy alias address that without rewriting journals. Source reviews did
not independently execute tests or measure backend cache hits.

Earlier full runs exposed message-count and overly broad extension assertions;
2/4/6 message counts and exactly one ACP pointer remain required. The fake summary
fixture now follows the side-request lane instead of consuming a normal response.
No acceptance assertion was weakened. Stale-looking shared-worktree artifacts led
to separate worker targets and serial final checks. An earlier full run was stopped
at a stalled ACP invalid-token test when the design changed; it is not a green run.

Windows all-target checks are blocked in existing aws-lc-sys C compilation by
missing stdlib.h/windows.h in this Mac's Windows C toolchain; no dependency or
feature was disabled. Live Road cache uplift, actual model scheduling decisions,
and Windows runtime remain unmeasured. No global binary was installed.
The user authorized a local commit with these validation limitations recorded.

Final serial run (`job_9vdsqwc3`): fmt passed; workspace all-target check finished
in 4m12s; strict workspace Clippy finished in 4m13s; test compilation in 4m16s.
The binary unit suite passed 58 tests and acp_asked passed 1. The workspace run
then stalled >60s at acp_bridge::a_proxy_with_a_token_this_run_never_minted_gets_nothing.
It was stopped after 14m rather than left hanging; later suites/checks in that
chain did not run. No test was skipped or weakened, and this is not a green full
workspace result. Proxy investigation is a separate pending scope; edits stay frozen.
