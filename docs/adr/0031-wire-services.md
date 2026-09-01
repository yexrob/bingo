# ADR-0031 — Wire services

Status: accepted · 2026-09-01 · Plan: M28

## Context

`Contribution::Service` passes one live object between plugins, met by
TypeId: owner and consumer import one contract crate and downcast to
the same type — the ADR-0001 plugin-to-plugin lane. TypeId cannot
cross a process, so opening services to external plugins needs a
second rendezvous, and the contracts-first rule names it: a boundary
consumed independently gets its contract in a form its consumers can
read. Across processes that form is a schema, not a type.

## Decision

1. **Rendezvous by layer.** In-process, the TypeId lane is unchanged.
   Across processes a service is met by string key + method name +
   JSON schema — the same trio the tool contract already rides.
2. **One generic face in the sdk**: `WireService::call(method, params)
   -> Value`. It is the only new trait this design mints. Per-service
   typed traits live in their own api crates, never in the sdk — the
   kernel keeps no feature nouns.
3. **Crossing is the owner's choice.** A service reaches processes
   only if its owner registers a wire face beside the typed handle —
   two faces of one live object, the adapter mechanical (typed→wire).
   No wire face, and the service does not exist to a process.
4. **`service/call {key, method, params}` flows both ways.** Host →
   process serves an external provider of a service; process → host
   serves an external consumer — the connection grows reverse
   requests. External ↔ external routes through the host: the registry
   is the router, no process-to-process pipes.
5. **The manifest declares.** A plugin's `services` block names its
   keys and method schemas; an unknown method is answered with the set
   the service speaks; `requires: ["service:<key>"]` keeps its
   fail-soft meaning — missing disables with a notice, never crashes.
6. **api crates are Rust-side sugar.** A typed trait plus two
   mechanical adapters (typed→wire for owners, wire→typed for
   consumers), written by whoever wants types — never required of an
   external author. The ecosystem contract is zero-Rust: key, method,
   schema.

## Consequences

- Two external plugins can pair on a service with no Rust written by
  anyone; a Rust consumer of an external service programs against its
  api crate's trait and cannot tell the implementation moved out of
  process.
- Service calls are code-to-code between user-installed plugins and
  carry no kernel authority; the permission gate is not involved, and
  the schema's words say a service call is the plugin's own act.
- The reverse-request machinery lands once, scoped to `service/call`;
  anything more an external process may ask of the host is a future
  ADR's door, not a side effect of this one.
- The Rust-side sugar keeps one fact in one place: the typed trait is
  the source, the adapters are derived and mechanical, and neither
  side hand-writes the contract twice.

Refs: ADR-0001, ADR-0011 §1, ADR-0015, ADR-0030.
Non-goals: hooks or policies as services; kernel authority over the
wire; per-service types in the sdk; a second service registry.
