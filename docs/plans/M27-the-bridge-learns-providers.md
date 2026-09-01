# M27 — The bridge learns providers (ADR-0030)

## Goal

An external plugin process can serve models: the handshake declares
providers with their models, the wire gains its one streaming lane
(deltas as notifications, cancellation passed through), a
`RemoteProvider` implements the sdk trait, and a whole turn runs on a
scripted child's model with the kernel none the wiser.

## Bricks, in build order

1. **Wire contract first** (`wire.rs`, `schema.rs`) — the streaming
   shape: `provider/stream {id, call, request}` opens one stream;
   the process emits `provider/delta {call, event}` notifications
   (each carrying one sdk `ModelEvent`, crossing as the sdk writes
   it — derives added where missing, never bridge copies) and closes
   with the response (finish or the error the trait speaks);
   `provider/cancel {call}` flows host → process like `tool/cancel`.
   Deltas are keyed by `call` so two concurrent streams never
   interleave. Fixtures pin open/delta/close/cancel before any proxy;
   `PROTOCOL` bumped; schema regenerated.
2. **The handshake declares** (`manifest.rs` or `wire.rs`,
   `bridge.rs`) — `providers: [{id, family?, models: [...]}]`;
   `provides` accepts `provider:<id>`. `endpoint()` answers from the
   declaration and only the declaration: an undeclared model is
   `false` (ADR-0015 §4 pinned), `family` defaults to the id as the
   sdk trait does.
3. **Late resolution** (`plugin.rs` in the sdk, the provider
   resolution point in core) — `ProviderSource` mirrors the other
   sources; the registry grows its arm, and providers resolve late at
   the one point they are resolved today (find it; the model catalog
   must list a late provider's models through the same path it lists
   built-ins — no second list). R-one-point applies as in M26.
4. **`RemoteProvider`** (new `provider.rs` in plugin-rpc) —
   implements the sdk `Provider`; `stream()` sends the request and
   returns a `ModelStream` fed from the delta notifications through a
   bounded channel until the close; dropping the stream or firing the
   `CancellationToken` sends `provider/cancel`; a process that
   ignores cancel or goes silent hits the idle deadline (no delta
   within the constant → the stream yields the timeout error the
   kernel already retries on). `count_tokens` stays the sdk default.
   Deadline constants join M26's module.
5. **Black-box** (`stub_plugin.rs`, `tests/plugin.rs`) — the stub
   declares a provider with one model; a session runs a full turn on
   it (text, and one tool round-trip); a cancelled stream stops the
   child and the turn ends as interrupted; a mid-stream death yields
   the error the trait speaks and the kernel's retry/backoff is
   untouched. Paused-clock for the idle deadline; nothing waits wall.

## Files

`crates/bingo-sdk/src/{plugin,provider,model}.rs` (source trait +
derives only), the provider resolution point and catalog arm in
`bingo-core`, `crates/bingo-plugin-rpc/src/{wire,schema,bridge,
manager,source,deadline}.rs`, new
`crates/bingo-plugin-rpc/src/provider.rs`,
`crates/bingo-plugin-rpc/examples/stub_plugin.rs`, the generated
`schema/plugin.json`, crate tests. No new dependencies; budget
unchanged.

## Exit criteria

- [x] wire fixtures pin stream open, delta, close and cancel; deltas
      keyed by call; schema regenerated; `PROTOCOL` bumped
- [x] handshake declares providers; `endpoint()` fail-closed pinned
      (an undeclared model is `false`)
- [x] `ProviderSource` resolved at the one provider point; the model
      catalog lists a late provider's models through the built-in path
- [x] a full turn runs on the stub's model, tool round-trip included;
      cancel reaches the process; mid-stream death yields the trait's
      error with kernel retry untouched
- [x] idle deadline on a paused clock; constants in the one module
- [x] every gate green (fmt, check, clippy, test, discipline, budget
      unchanged, deny)

## Non-goals

Services (ADR-0031, M28); hooks (ADR-0032, M29); Store; auth flows
for remote providers (a process minds its own credentials); provider
options pass-through beyond what `ModelRequest` already carries;
changes to in-process providers or the retry/backoff ladder.

## Risks

R-stream — the one new wire shape: the delta channel is bounded and
keyed by call; two streams interleaving or an unbounded buffer is the
defect to design against. R-cancel — a process may ignore cancel; the
idle deadline is the floor that keeps the turn from hanging, and the
kernel's existing interrupt semantics must see the stream end.
R-catalog — a late provider's models must ride the existing catalog
path; a second list is the ADR-0011 debt. R-serde — `ModelEvent` and
friends cross as the sdk writes them; a bridge-side copy of any model
type is refused in review.

## Verified

Integrated on main at `3688bed` (2026-09-01, load 15–36): every gate
green, workspace 0 failures (plugin-rpc 185 + 81 + 21, cli 130),
budget 302 unchanged, deny ok.

Worker run: 2026-09-01, load average 7–12 (a busy machine; nothing
here waits on a wall clock — the idle deadline is proven on a paused
one).

```
$ cargo fmt --all -- --check
clean
$ cargo check --workspace --all-targets --locked
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.03s
$ cargo clippy --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.65s
$ cargo test --workspace --locked
69 × test result: ok; 0 failed (2770 tests)
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

- the wire: `wire::tests::a_stream_opens_with_the_request_the_sdk_writes`,
  `a_delta_carries_one_sdk_event_and_the_stream_it_belongs_to`,
  `a_finish_crosses_as_the_sdk_writes_it`,
  `a_stream_closes_with_nothing_or_with_the_error_the_trait_speaks`,
  `a_cancel_names_the_stream_it_stops_and_no_other`;
  `schema::tests::the_committed_schema_names_every_method_and_every_notification`
  reads seven methods, four notifications and `protocol: 3` off
  `schema/plugin.json`, and `the_sdk_types_a_plugin_answers_with_are_all_defined`
  now names `ModelRequest`, `ModelEvent`, `ModelInfo`, `ProviderError`
  and `EndpointCapabilities` — the model vocabulary crosses as the sdk
  writes it, with no bridge copy of any of it and no derive to add.
- keyed by call: `connection::tests::a_delta_reaches_the_stream_it_names_and_no_other`,
  and over a real pipe `tests/plugin.rs::two_streams_at_once_never_mix_their_deltas`.
- the declaration: `tests/plugin.rs::a_plugin_s_provider_serves_the_models_it_declared`,
  `provider::tests::a_model_the_declaration_never_named_can_do_nothing`
  (ADR-0015 §4) and `a_provider_that_named_no_family_is_filed_under_its_own_id`.
- one resolution point: `Host::providers`, the only reader of
  `registry.providers` and `provider_sources`, folding both through
  `turn::late::ProviderSet::gather`; `provider`, the catalogue and
  `/model`'s `<provider>/<model>` all read it, and `has_provider` was
  deleted rather than given a second answer.
  `late::tests::a_source_s_providers_join_the_registered_ones_and_never_shadow_one`,
  `host::registry::tests::a_late_source_of_every_kind_is_kept_where_the_turn_reads_it`
  (five kinds now), `host::tests::providers::the_catalogue_lists_a_late_provider_s_models_where_it_lists_every_other`.
- a turn on a late provider:
  `host::tests::providers::a_session_opens_on_a_provider_that_did_not_exist_at_boot`
  — nothing at boot, the source answers, the turn completes on it.
  Against a real child process, `tests/plugin.rs::a_whole_response_crosses_the_pipe_event_by_event`
  and `a_tool_round_trip_crosses_as_two_streams`. Two tests and not one:
  joining them would make the bridge crate depend on the kernel, which
  ADR-0001 forbids.
- cancel: `a_cancelled_stream_ends_and_the_process_is_told` and
  `a_dropped_stream_tells_the_process_to_stop`; the stub reads its own
  `provider/cancel` notifications back out through its tool, so what is
  asserted is what crossed the pipe.
- the trait's error, the ladder untouched:
  `a_process_that_dies_mid_stream_yields_the_error_the_trait_speaks`
  (`Transport`, `retryable()`) and `a_failed_response_crosses_as_the_error_the_trait_speaks`
  (`RateLimited { retryAfterMs }` verbatim, so the kernel waits the
  seconds the plugin named). Nothing in `turn/stream.rs` changed.
- the idle deadline, on `tokio::test(start_paused)`:
  `provider::tests::a_stream_that_hears_nothing_gives_up_at_the_deadline`;
  `deadline::PROVIDER_IDLE` joins M26's module, and
  `the_hot_path_has_the_shortest_deadline_of_them_all` keeps the four
  ordered.

Four decisions the plan left open, taken here: **a remote provider keeps
the id it declared** (a person types it — ADR-0017 §2 — where a remote
contributor's id is prefixed), a registered id winning over a source's
with a `tracing::debug!` for the loser; **`endpoint` is declared once per
provider**, as every built-in answers it, the per-model part being
whether the model was declared at all; **the close carries
`ProviderError`** so the kind survives to the retry ladder, a JSON-RPC
error meaning `Stream` and a dead pipe `Transport`, both retryable
because the kind is unknown; and **`models` answers from the
declaration**, so asking costs no call and ADR-0026 §4's live-list lane
is untouched.

## Carried

- **A late provider's own model ids are not in the catalogue.**
  `catalog(Models)` lists `models_of(family)` from the embedded
  snapshot, so a plugin serving a catalogued shape (`family:
  "anthropic"`) is listed model for model, while one serving ids of its
  own shows only the configured model — as a built-in instance with a
  private endpoint does today. Its declaration still reaches a person
  through `Provider::models` and the default-model choice. Widening the
  catalogue is a catalogue decision (ADR-0026 §4 refused the live call
  for good reasons); take it when a plugin provider with private ids
  exists.
- `crates/bingo-plugin-rpc/tests/plugin.rs` reached 749 lines (700 warn,
  1000 fail); split it on its next growth — M28's services will add to it.
- `bingo-plugin-rpc` now depends on `futures`, as the sdk, the kernel and
  every provider crate do, because a `ModelStream` is a `futures::Stream`.
  No crate enters the workspace tree: the budget is 302, unchanged.
