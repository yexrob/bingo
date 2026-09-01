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

- [ ] wire fixtures pin `context/contribute` and `compactor/compact`;
      schema regenerated; `PROTOCOL` bumped; the method-count pin
      derives from the schema
- [ ] `ContextSource`/`CompactorSource` registered and resolved at the
      tool sources' one point; registry arms table-tested
- [ ] handshake declares contributors (with placement) and compactors;
      an undeclared kind is refused in words
- [ ] proxies work end to end with a scripted child: the piece lands
      with origin `contributor:<id>`; compaction runs remote
- [ ] deadlines: a late contributor drops with a notice on a paused
      clock; a late compactor fails the call
- [ ] every gate green (fmt, check, clippy, test, discipline, budget
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
