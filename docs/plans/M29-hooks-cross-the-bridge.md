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

- [ ] wire fixtures pin `hook/decide` and `hook/observe`; the
      declaration mirrors `HookMatcher`; the outcome schema has no
      `Allow` to say; `PROTOCOL` bumped; schema regenerated
- [ ] `HookSource` resolved at the kernel's one hook point; bridge
      hooks compose with in-process hooks in registration order
- [ ] a matched `before_tool` rewrites and denies from the process;
      an unmatched tool never crosses; `on_submit` appends
- [ ] observation points are notifications: nothing awaited, proven
- [ ] a hook past its deadline never decides: Continue plus a notice
      naming it, on a paused clock
- [ ] every gate green (fmt, check, clippy, test, discipline, budget
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
