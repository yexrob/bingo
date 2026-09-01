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

- [ ] `WireService` in the sdk; one registry entry per key holds both
      faces; the TypeId lane's existing behaviour byte-identical
- [ ] wire fixtures pin `service/call` in both directions; the
      reverse lane serves exactly `service/call` and still refuses
      every other process request with the warn; `PROTOCOL` bumped;
      schema regenerated
- [ ] owner opt-in pinned: no wire face → refused in words; unknown
      method → answered with the spoken set
- [ ] the hub proven over real child processes: two stubs pair on
      `kv`; an in-process consumer reaches an external service by key
- [ ] deadline on a paused clock; constant in the one module
- [ ] `tests/plugin.rs` split landed before growth; every gate green
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
