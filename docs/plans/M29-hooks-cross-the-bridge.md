# M29 — Hooks cross the bridge (ADR-0032)

## Goal

An external plugin registers handlers at the kernel's hook points:
the matcher is declared at handshake, decision points cross as
requests carrying the outcome and the possibly-rewritten value,
observation points cross as notifications nobody waits on, and
tighten-only stays the type's own law.

## Bricks, in build order

1. **Wire contract first** (`wire.rs`, `schema.rs`) — the handshake's
   `hooks: [HookSpec {id, points, tool?}]` mirrors `HookMatcher`; one
   request `hook/decide {id, point, payload} -> {outcome, value?}`
   serves the four decision points (`submit`, `beforeTool`,
   `afterTool`, `stop` — the payload tagged by point, `value` the
   rewritten input or call where the trait mutates, absent where it
   does not); one notification `hook/observe {id, point, payload}`
   serves the four observation points. `HookOutcome` crosses as the
   sdk writes it — it has no `Allow`, so the wire cannot say one
   (pin that in the schema test). Serde derives added where missing,
   never bridge copies. Fixtures before implementation; `PROTOCOL`
   bumped; schema regenerated; ADR-0015 §6's note updated.
2. **Late hooks in the sdk and core** — `HookSource` in the ADR-0030
   source shape; `Contribution::Hooks`; find where the kernel reads
   its hooks today and extend that one point (M27's discipline: no
   second list, no second resolution point; where that point sits —
   per turn, per event — is the code's to say, follow it).
3. **`RemoteHook`** (new `hook.rs` in plugin-rpc) — implements the
   sdk `Hook`; matcher from the declaration, asked once. Decision
   points send `hook/decide` under a deadline (hooks-shell's
   precedent: a hook that errors or misses it never gets to decide —
   the host continues with a notice naming it; constant joins the one
   module); the response's `value` is applied to the `&mut` argument
   only at the two points that own one. Observation points send the
   notification and return at once. Bridge hooks compose with
   in-process hooks in registration order — pin with a two-hook test.
4. **Black-box** (`stub_plugin.rs`, new `tests/plugin/hooks.rs`) —
   the stub declares a `before_tool` hook matching one tool name:
   a matched call is rewritten and a scripted deny is refused in the
   hook's own words; an unmatched tool never crosses the pipe (the
   stub proves silence through its kv service); an `on_submit` hook
   appends to the input; an observation point lands as a
   notification; a hook past its deadline yields Continue plus the
   notice, on a paused clock.

## Files

`crates/bingo-sdk/src/{hook,plugin}.rs` (derives + the source trait),
the hook resolution point in `bingo-core`,
`crates/bingo-plugin-rpc/src/{wire,schema,bridge,manager,deadline,
connection,lib}.rs`, new `crates/bingo-plugin-rpc/src/hook.rs`,
`crates/bingo-plugin-rpc/examples/stub_plugin.rs`, new
`crates/bingo-plugin-rpc/tests/plugin/hooks.rs`, the generated
`schema/plugin.json`. No new dependencies; budget unchanged.

## Exit criteria

- [x] wire fixtures pin `hook/decide` and `hook/observe`; the
      declaration mirrors `HookMatcher`; the outcome schema has no
      `Allow` to say; `PROTOCOL` bumped; schema regenerated
- [x] `HookSource` resolved at the kernel's one hook point; bridge
      hooks compose with in-process hooks in registration order
- [x] a matched `before_tool` rewrites and denies from the process;
      an unmatched tool never crosses; `on_submit` appends
- [x] observation points are notifications: nothing awaited, proven
- [x] a hook past its deadline never decides: Continue plus a notice
      naming it, on a paused clock
- [x] every gate green (fmt, check, clippy, test, discipline, budget
      unchanged, deny)

## Non-goals

Policy over the wire; an `Allow` outcome under any spelling; new hook
points; changes to hooks-shell or to in-process hooks; widening the
reverse-request lane beyond M28's `service/call`.

## Risks

R-tighten — the whole safety: `HookOutcome` has no `Allow`, and the
wire adds none; the schema test is the pin. R-latency — `submit` and
`before_tool` sit on hot paths: the handshake matcher keeps unmatched
events at zero cost and the deadline is the whole protection for
matched ones. R-mutate — the rewritten value replaces the mutable
argument at exactly the two points that own one; anywhere else it is
refused by the shape. R-order — composition order with in-process
hooks must match two in-process hooks' order; pin it, do not assume
it.

## Verified

Worker run, all gates green on the first try, 1-minute load 6.6–9.1
(no timing-sensitive family failed; nothing was rerun):

```
$ cargo fmt --all -- --check
$ cargo check --workspace --all-targets --locked
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.31s
$ cargo clippy --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.87s
$ cargo test --workspace --locked
2828 passed; 0 failed; 0 ignored
$ scripts/check_discipline.sh
dependency direction ok / kernel names no tool / cohesion ok
discipline ok
$ scripts/budget.sh
dependencies (unique, normal): 302 (max 302)
budget ok
$ cargo deny check
advisories ok, bans ok, licenses ok, sources ok
```

What each criterion rests on:

- **the wire, fixtures first** (`wire.rs`): `a_declared_hook_carries_the_
  matcher_the_kernel_skips_on` reads `{id, points, tool}` into the sdk's
  own `HookMatcher` — the declaration is the matcher flattened, not a
  copy of it — and `a_hook_that_names_no_point_wants_them_all` keeps the
  empty matcher's meaning. `a_decision_is_asked_for_by_the_point_that_
  owns_the_payload`, `a_stop_carries_the_point_and_no_payload_at_all` and
  `an_after_tool_carries_the_call_and_what_it_answered` pin
  `{id, site, point, payload}`; `an_observation_carries_the_phase_and_
  the_turn_it_watched` and `an_observed_frame_crosses_as_the_journal_
  writes_it` pin the notification. `a_site_crosses_without_the_host_the_
  context_carries` is ADR-0030 §5's projection: session, turn, cwd,
  model, and no host and no provider.
- **no `Allow`, in the type and in the document**: the sdk's
  `hook::tests::no_spelling_of_allow_is_an_outcome`, the wire's
  `no_answer_a_process_can_write_widens_anything`, and the schema pin
  `schema::tests::the_outcome_a_hook_writes_has_no_allow_to_say`, which
  reads the five kinds off `$defs/HookOutcome` and then greps
  `HookOutcome`, `HookDecideResult` and `HookValue` — descriptions
  stripped, so the doc comment saying there is no `Allow` cannot be what
  passes the test — for `allow`, `approve` and `permit`.
- **PROTOCOL 5, schema regenerated**: `the_committed_schema_is_this_
  document` and `the_committed_schema_names_every_method_and_every_
  notification` read nine methods, five notifications and `protocol: 5`
  off `schema/plugin.json`. ADR-0015 §6's supersession note and
  `examples/plugins/wordcount/` (its `PROTOCOL` and its README's wire
  summary) say the same; the wordcount black-box in `crates/bingo` is
  what would have caught a stale one, and did.
- **one resolution point**: `TurnConfig.hooks` is a `HookSet` in
  `turn/late.rs` beside the other late sets, and every reader asks it.
  Five hand-written `iter().filter(hook_applies)` copies went — the
  session's `Session`, `Event` and turn-end reads, the submit, and the
  turn's `hooks(point)`; the two that filter by tool name (the gate's
  `run_hooks`, `after_tool_hooks`) keep their own `hook_applies` call
  over the one set's answer, because the name is per call and not per
  point. `a_source_s_hooks_join_the_registered_ones_in_order`,
  `a_point_asks_only_the_hooks_that_claim_it`, `a_call_a_hook_did_not_
  claim_never_reaches_it` and `a_session_with_no_hook_anywhere_is_empty`.
- **composition order, pinned rather than assumed** (R-order):
  `turn::tests::a_source_s_hook_composes_where_a_second_registered_hook_
  would` runs one gated call twice — two registered hooks, then one
  registered and one from a source — and compares the two orders the
  hooks wrote down. Nothing in it reads `gather`.
- **a hook in another process, over a real child**
  (`tests/plugin/hooks.rs`, four stub hooks):
  `a_matched_call_is_rewritten_by_a_hook_in_another_process` (`ls` comes
  back `ls --safe`), `a_hook_in_another_process_refuses_a_call_in_its_own_
  words` (`Deny { reason: "not that one" }`, the call untouched),
  `a_submit_hook_in_another_process_rewrites_the_line` (the text grows,
  the origin stays the person's), and `an_observation_point_lands_as_a_
  notification_nobody_waits_on` — the stub answers an observe with
  nothing, so a lane that awaited one would never return. Every crossing
  is written into the map the stub's own `kv` service serves and read
  back through the host, so what arrived is the process's own memory.
- **an unmatched event costs nothing**: `what_a_hook_claims_is_answered_
  without_the_pipe` asks `matcher()` and finds the store still empty —
  the declaration is answered locally, so nothing crossed. That the
  kernel then skips on that matcher is `HookSet::at`'s to prove, and
  `a_call_a_hook_did_not_claim_never_reaches_it` does.
- **the deadline, on a paused clock** (hooks-shell's precedent):
  `hook::tests::a_hook_past_its_deadline_never_decides_and_a_notice_
  names_it` and its submit twin against a live-but-silent child, and end
  to end `tests/plugin/hooks.rs::a_hook_past_its_deadline_never_decides_
  and_a_notice_names_it` — `Continue`, the call untouched, one
  `HOOK_UNANSWERED` notice naming `stub:silent` and the 5s it was given,
  and the store showing it was asked. `deadline::HOOK` joins the one
  module and `the_hot_path_has_the_shortest_deadline_of_them_all` keeps
  six ordered: contribute < hook < handshake < service < compact <
  provider idle.
- **the reverse lane is still one method wide**:
  `connection::tests::exactly_one_method_travels_from_a_process_to_the_
  host` is unchanged and still passes — hook traffic is host → process
  only.

Three decisions the plan left open, taken here:

- **The declaration flattens the sdk's `HookMatcher` rather than
  restating it.** `HookSpec { id, #[serde(flatten)] matcher }` gives the
  plan's flat `{id, points, tool?}` on the wire with the matcher written
  once; `HookMatcher` gained the serde and schemars derives instead of
  the bridge gaining a copy.
- **`HookDecision` and `HookObservation` are adjacently tagged**
  (`tag = "point", content = "payload"`) and flattened into the params,
  so the line reads `{id, site, point, payload}` with the point spelled
  once. A separate `point` field beside a self-describing payload would
  have been the same fact twice.
- **The registry's six late lists became one `Sources` struct.** Adding
  `hook_sources` took `Registry` to 17 fields and the cohesion check
  fails above 16. Grouping is the honest fix rather than the cheap one: a
  source is the one contribution the composition never arbitrates — it
  holds no slot, takes no name, and two of a kind are both welcome — so
  what the registry judges stays at the top level and what it only
  carries moved into `registry.sources`.

## Carried

- **A hook's notice waits for the next bridge tool call.** `Notices` is
  the crate's one channel to a person and it is drained inside
  `PluginTool::call` (notice.rs says why: a source read holds no
  session). A hook that misses its deadline in a session whose plugin
  offers no tool leaves its notice in the queue, logged but unsaid. The
  fix is a notice lane that does not need a tool call — a hook holds a
  `HookContext` with the whole host, so the door exists — and it is its
  own decision, not this milestone's.
- **`HookContext.provider` does not cross.** An in-process hook may ask
  the session's model (memory extraction does); an external one cannot,
  because a provider is not serializable and ADR-0030 §5 leaves it
  behind. A remote hook that wants a model asks its own. Worth saying in
  the plugin docs if someone tries.
- **The stub is a directory example now** (`examples/stub_plugin/`), so
  its hooks own a module and neither file passes the 700-line warn. The
  harness finds the same binary path; nothing else changed.
