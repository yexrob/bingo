# M26 — The bridge learns context and compaction (ADR-0030)

## Goal

An external plugin process can contribute prompt context and a
compaction strategy: the sdk grows late sources for both kinds, the
bridge grows two proxies and two wire methods, every crossing has a
deadline, and `schema/plugin.json` stays the external author's only
required reading.

## Bricks, in build order

1. **Wire contract first** (`wire.rs`, `schema.rs`) — methods
   `context/contribute { query } -> { pieces }` and
   `compactor/compact { context, reason } -> { compaction }`; the
   query is `ContextQuery`'s serializable projection (session summary,
   turn, round, items, usage, capabilities, cwd — no host reach), one
   struct with its own fixture. Fixtures pin both shapes before any
   proxy exists; `PROTOCOL` bumped; the ADR-0015 "four methods" pin
   becomes schema-derived.
2. **Late sources in the sdk** (`contributor.rs`, `compactor.rs`,
   `plugin.rs`) — `ContextSource` and `CompactorSource` mirror
   `ToolSource` exactly (id + async resolve); `Contribution` grows the
   two variants, the core registry its arms, and the loop resolves
   them at the one point tool sources are resolved today. Table-test
   the arms; no second resolution point.
3. **The handshake declares** (`manifest.rs`, `bridge.rs`,
   `manager.rs`) — a plugin declares contributors (id + placement) and
   compactors (id) at initialize; `provides` accepts `context:<id>`
   and `compactor:<id>`. Placement is handshake data, asked once,
   never per call.
4. **The proxies** (new `contributor.rs`, `compactor.rs` in
   plugin-rpc) — `RemoteContributor` and `RemoteCompactor` implement
   the sdk traits; bodies are wire calls with a deadline each: a
   contributor past it contributes nothing that round and a notice
   says whose deadline was missed; a compactor past it fails the call
   with the error the trait speaks. Deadline constants in one module.
5. **Black-box** — a scripted child process (the crate's existing test
   pattern) provides one contributor and one compactor; a session
   shows the contributed piece landing as a user item with origin
   `contributor:<id>`; the deadline is proven on a paused clock
   in-crate, not by a wall wait.

## Files

`crates/bingo-sdk/src/{plugin,contributor,compactor}.rs`,
`crates/bingo-core/src/host/registry.rs` and the one resolution point,
`crates/bingo-plugin-rpc/src/{wire,schema,manifest,bridge,manager,source}.rs`,
new `crates/bingo-plugin-rpc/src/{contributor,compactor}.rs`, the
generated `schema/plugin.json`, crate tests. No new dependencies;
budget unchanged.

## Exit criteria

- [x] wire fixtures pin `context/contribute` and `compactor/compact`;
      schema regenerated; `PROTOCOL` bumped; the method-count pin
      derives from the schema
- [x] `ContextSource`/`CompactorSource` registered and resolved at the
      tool sources' one point; registry arms table-tested
- [x] handshake declares contributors (with placement) and compactors;
      an undeclared kind is refused in words
- [x] proxies work end to end with a scripted child: the piece lands
      with origin `contributor:<id>`; compaction runs remote
- [x] deadlines: a late contributor drops with a notice on a paused
      clock; a late compactor fails the call
- [x] every gate green (fmt, check, clippy, test, discipline, budget
      unchanged, deny)

## Non-goals

Provider over the wire (M27); services (ADR-0031, M28); Store;
Hook/Policy; any new trait hierarchy; changes to how in-process
contributors register or run.

## Risks

R-hotpath — `contribute` runs per round: the deadline and
drop-with-notice are the whole protection; the turn must never block
on a dead process. R-query — the wire query is a projection with its
own fixture, not a second `ContextQuery`; host reach is ADR-0031's
lane, not this one's. R-one-point — resolving the new sources anywhere
but the tool sources' point is the ADR-0011 debt. R-io — registration
stays synchronous and I/O-free; only `start` touches processes.

## Verified

2026-09-01, load average 11.9 (a busy machine; no test here waits on a
wall clock — the two deadline tests run on a paused one).

```
$ cargo fmt --all -- --check
clean
$ cargo check --workspace --all-targets --locked
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 6.03s
$ cargo clippy --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.50s
$ cargo test --workspace --locked
69 × test result: ok; 0 failed
$ scripts/check_discipline.sh
dependency direction ok / kernel names no tool / cohesion ok
discipline ok
$ scripts/budget.sh
dependencies (unique, normal): 302 (max  302)
budget ok
$ cargo deny check
advisories ok, bans ok, licenses ok, sources ok
```

What each criterion rests on:

- the wire: `wire::tests::the_round_crosses_without_the_host_the_query_carries`,
  `a_compaction_is_asked_for_with_the_reason_the_trait_speaks`,
  `a_piece_crosses_as_the_sdk_writes_it`;
  `schema::tests::the_committed_schema_names_every_method_and_every_notification`
  reads the count off `schema/plugin.json`, where ADR-0015's literal was.
- one resolution point: `turn::late::Late::gather`, the only caller of
  the three sets' `gather`/`resolve`;
  `turn::tests::a_source_contributor_speaks_when_the_turn_starts_with_its_own_origin`
  (origin `contributor:notes`),
  `a_source_strategy_compacts_when_nothing_in_process_holds_the_slot`,
  `host::registry::tests::a_late_source_of_every_kind_is_kept_where_the_turn_reads_it`.
- the handshake: `tests/plugin.rs::a_plugin_s_contributor_speaks_at_the_placement_it_declared`,
  `a_plugin_s_compaction_strategy_answers_a_compaction`,
  `a_declaration_this_host_cannot_read_refuses_the_handshake_in_words`.
- deadlines, on `tokio::test(start_paused)`:
  `contributor::tests::a_contributor_past_its_deadline_contributes_nothing_and_says_whose`,
  `compactor::tests::a_compactor_past_its_deadline_fails_the_call`.

Two decisions the plan left open, taken here: the compactor slot keeps
its first-wins rule, so a source's strategy runs only where nothing
in-process holds the slot (a `tracing::debug!` says when one is unused,
as the registry already does for a shadowed command); and a remote
contributor's kernel-visible id is `<plugin>:<declared>`, so two plugins
may both declare `notes` and the transcript's origin still says which
one wrote.

Integrated on main at `1f5c4d3` (2026-09-01, load 11–30, after the
worker-R and worker-Q merges): every gate green, workspace 0 failures
(plugin-rpc 181 + 67 + 13, cli 130, rooms 140); ADR-0015 §6 amended
with the supersession note.

## Carried

- **The compactor slot is first-wins, and the shipped composition
  always fills it**: `bingo-context` registers `SummaryCompactor`, so
  an external compaction strategy is inert in the default binary. The
  reversal is one line in `CompactorSet::resolve`, but choosing the
  active strategy is a product decision — the clean shape when demand
  arrives is a settings key naming the compactor, not a registration
  order. Decide when the first external strategy exists.
- `crates/bingo-core/src/turn/tests.rs` reached 777 lines (700 warn,
  1000 fail); split it on its next growth.
