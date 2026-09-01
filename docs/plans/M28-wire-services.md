# M28 — Wire services (ADR-0031)

## Goal

A service crosses the process line by schema: an owner opts in with a
wire face beside the typed handle, `service/call` flows in both
directions, external ↔ external routes through the host, and the
ecosystem contract stays zero-Rust — key, method, JSON schema.

## Bricks, in build order

0. **Make room** — `crates/bingo-plugin-rpc/tests/plugin.rs` is at
   749 lines (700 warn); split it mechanically (one commit, no test
   changed) before anything grows it.
1. **The generic face** (sdk) — `WireService`: `call(method, params)
   -> Result<Value, _>`, the one new trait ADR-0031 mints. The
   registry keeps ONE entry per key holding both faces of one live
   object (typed `Any` + optional wire); the TypeId lane is untouched
   and its existing tests stay byte-identical. A named concrete
   handle type in the sdk makes an external service reachable by an
   in-process consumer through the one lookup (`host.service::<_>`).
2. **The wire, both directions** (`wire.rs`, `schema.rs`, `codec.rs`,
   `connection.rs`) — `service/call {key, method, params} ->
   {result}`. Host → process serves a service an external plugin
   provides; process → host is the connection's one new machinery:
   the reader's "a plugin asked, which it may not" arm learns to
   serve exactly `service/call` and keeps refusing everything else
   with the same warn. Handshake: `services: {key: {methods:
   {name: schema}}}` in `InitializeResult`; `provides:
   ["service:<key>"]`. Fixtures before implementation; `PROTOCOL`
   bumped; schema regenerated.
3. **The bridge's two directions** (new `service.rs` in plugin-rpc) —
   provider side: a declared service registers under its key as a
   remote handle implementing `WireService` (calls go out as
   `service/call`). Consumer side: an inbound `service/call` resolves
   through the host's wire faces — a service with no wire face is
   refused in words (crossing is the owner's choice, ADR-0031 §3),
   an unknown method is answered with the set the service speaks.
   External ↔ external follows by construction: in one door, out the
   other. Deadline constant joins the one module; errors name the
   plugin.
4. **Black-box** (`stub_plugin.rs`, the split tests) — the stub
   declares a `kv` service (`set`/`get`); an in-process consumer
   reaches it by key through the one lookup; two stub processes pair
   on it through the hub (set from one, get from the other); a call
   to an unwired key and an unknown method are both refused in the
   words the plan names; the deadline is proven on a paused clock.

## Files

`crates/bingo-sdk/src/plugin.rs` (or a small `service.rs`),
`crates/bingo-core/src/host.rs` + `host/registry.rs`,
`crates/bingo-plugin-rpc/src/{wire,schema,codec,connection,bridge,
manager,lib}.rs`, new `crates/bingo-plugin-rpc/src/service.rs`,
`crates/bingo-plugin-rpc/examples/stub_plugin.rs`, the generated
`schema/plugin.json`, crate tests. No new dependencies; budget
unchanged; no feature noun anywhere near the sdk.

## Exit criteria

- [x] `WireService` in the sdk; one registry entry per key holds both
      faces; the TypeId lane's existing behaviour byte-identical
- [x] wire fixtures pin `service/call` in both directions; the
      reverse lane serves exactly `service/call` and still refuses
      every other process request with the warn; `PROTOCOL` bumped;
      schema regenerated
- [x] owner opt-in pinned: no wire face → refused in words; unknown
      method → answered with the spoken set
- [x] the hub proven over real child processes: two stubs pair on
      `kv`; an in-process consumer reaches an external service by key
- [x] deadline on a paused clock; constant in the one module
- [x] `tests/plugin.rs` split landed before growth; every gate green
      (fmt, check, clippy, test, discipline, budget unchanged, deny)

## Non-goals

Hooks (ADR-0032, M29); Store; streaming services (one call, one
reply); cycle detection between services (the deadline is the floor);
api crates for particular services (the channels adapter rides this
lane later, as its own consumer); any kernel authority over the wire.

## Risks

R-reverse — the process→host request server is the one new machinery:
scope it to `service/call` alone, everything else keeps the refusal
warn; widening it is a future ADR's door, not a convenience. R-faces —
one registry entry, two faces of one object; a second list keyed by
the same string is the ADR-0011 debt. R-typeid — the in-process lane
must not move: its tests stay untouched and green. R-loop — external A
calling B calling A hangs until the deadline; bound it with the
constant, build no cycle detector without evidence.

## Verified

```
$ cargo fmt --all -- --check
$ cargo check --workspace --all-targets --locked
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 10.01s
$ cargo clippy --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.98s
$ cargo test --workspace --locked
1267 passed; 0 failed (69 suites)
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

- **the split, first and alone** (commit `test(plugin-rpc): split the
  black-box tests by what crosses`): `tests/plugin.rs` at 749 lines
  became `tests/plugin/{main,harness,tools,context,provider,lifecycle}.rs`
  with no test body edited — the same 21 tests, renamed only by their
  module. The entry is `tests/plugin/main.rs`, which cargo finds without
  a manifest entry: an integration test's crate root is itself, so its
  submodules would otherwise be looked for in `tests/`.
- **the sdk's one trait**: `bingo_sdk::service` holds `WireService`,
  `ServiceError`, `ServiceHandle` and the `Services` map both the kernel
  and the testing host keep, and nothing else — no per-service trait, no
  feature noun. `service::tests::a_service_with_no_wire_face_is_still_a_
  service_in_process`, `an_opened_service_answers_by_type_and_by_wire`,
  `a_key_that_is_taken_stays_its_first_owner_s`.
- **one entry, two faces, the TypeId lane untouched**:
  `host::tests::services::a_service_is_a_typed_value_under_a_key_and_
  crosses_only_if_it_opened_a_face` — `host.service::<u32>(key)` answers,
  a wrong type does not, and `service_wire` is `None`. `Registry.services`
  is the one map; `Contribution::Service` grew `wire: Option<..>` beside
  its value rather than a second variant or a second list.
- **the wire, both ways**: `wire::tests::a_service_call_is_one_shape_
  whichever_way_it_travels`, `a_call_may_take_nothing_and_an_answer_may_
  be_nothing`, `a_declared_service_names_the_methods_it_speaks_and_their_
  schemas`, `a_process_that_serves_no_service_says_nothing_about_services`.
  `schema::tests::the_committed_schema_names_every_method_and_every_
  notification` reads eight methods, four notifications and `protocol: 4`
  off `schema/plugin.json`; ADR-0015 §6's note says the same.
- **the reverse lane is one method wide**:
  `connection::tests::exactly_one_method_travels_from_a_process_to_the_
  host` walks every other name in `wire::name` past `served`, and
  `a_request_the_host_does_not_serve_is_refused_and_the_reader_goes_on`
  keeps the warn arm's behaviour — refused where it arrives, the reader
  carrying on. An answer goes out on a task of its own, so a slow service
  never stops the reader that is also carrying that process's replies.
- **owner opt-in, refused in words**:
  `service::tests::a_service_with_no_wire_face_does_not_exist_to_a_process`
  ("the service kv has no wire face: it does not cross to a plugin") and
  `a_key_nobody_holds_is_refused_in_words`; over a real pipe,
  `tests/plugin/service.rs::a_call_to_a_service_nobody_serves_is_refused_
  in_words`.
- **an unknown method is answered with the spoken set**, before it
  crosses: `service::tests::a_method_the_declaration_never_named_is_
  refused_with_the_set_it_speaks`, `a_service_that_named_no_method_speaks_
  nothing`, and end to end
  `tests/plugin/service.rs::an_unknown_method_is_answered_with_the_set_
  the_service_speaks` ("store: the service kv does not speak drop; it
  speaks get, set").
- **the hub over real child processes**: two copies of `stub_plugin` are
  installed as `store` (declaring `kv`) and `caller` (`--no-service`).
  `two_processes_pair_on_a_service_through_the_host` writes from the
  caller and reads the value out of the store's own map;
  `a_caller_reads_back_what_it_wrote_across_two_processes` does the round
  trip; `an_in_process_consumer_reaches_an_external_service_by_key` calls
  `host.service::<ServiceHandle>("kv")` and cannot tell it is a process;
  `a_second_plugin_claiming_the_same_key_is_reported_and_the_first_keeps_
  it` pins one owner per key with one `SERVICE_TAKEN` notice.
- **the deadline, on a paused clock**:
  `service::tests::a_service_that_says_nothing_gives_up_at_the_deadline_
  and_names_the_plugin` ("stub: the service kv said nothing within 30s").
  `deadline::SERVICE` joins the one module and
  `the_hot_path_has_the_shortest_deadline_of_them_all` keeps the five
  ordered: contribute < handshake < service < compact < provider idle.
  No cycle detector: A→B→A ends here, as the plan says it should.

Three decisions the plan left open, taken here:

- **A service an external process declares reaches the registry through
  a new door, `HostApi::open_service`.** Registration is synchronous and
  discovery is I/O, so a declared key cannot be a `Contribution`; the
  plan's "an in-process consumer reaches an external service by key
  through the one lookup" and ADR-0031 §4's "the registry is the router"
  both need the entry to exist. A `ServiceSource` in the ADR-0009 shape
  would have been a second new trait, which ADR-0031 §2 forbids, so the
  host takes the service instead: `open_service(key, wire)` builds both
  faces from the one object, and `service_wire(key)` reads the second
  one. Both have default bodies — refuse, and `None` — so every existing
  `HostApi` double is untouched and a host that keeps no services fails
  closed.
- **The published handle outlives the process.** `RemoteService` holds a
  `Weak<Bridge>` and asks for the live connection on every call, so a
  plugin that died and respawned serves the same key without a second
  entry; `Bridge` remembers which keys it has offered, so a respawn
  publishes nothing and a refused key is reported once.
- **`Bridge::new` takes a `Setting`** (env, data dir, notices, host)
  rather than an eighth argument, which clippy refuses at 7.

## Carried

- `HostApi` now has two defaulted methods. A default that fails closed
  is right for a double, but a kernel trait growing defaults is a slope:
  if a third arrives, give `HostApi` a services section of its own or
  make the doubles share one base.
- The stub reads one line at a time and may not wait inside one:
  `std::io::Stdin::lock` is not reentrant, so a nested read deadlocks.
  Its outbound service call leaves the tool call open and answers it when
  the response arrives — which is also what a real plugin should do.
- `provides: ["service:<key>"]` is unchanged: an in-process owner still
  declares its key in its static manifest, and `requires` on a service
  only an external plugin declares still disables the requirer at load —
  fail-soft, as ADR-0031 §5 keeps it. Making a process's declaration
  satisfy a `requires` is a resolution decision, not this milestone's.
