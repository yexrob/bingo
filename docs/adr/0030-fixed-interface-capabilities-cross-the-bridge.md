# ADR-0030 — Fixed-interface capabilities cross the bridge

Status: accepted · 2026-09-01 · Plans: M26, M27

## Context

ADR-0015 opened the process boundary for tools and commands and fixed
the wire at four methods and two notifications. The ecosystem asks for
more: a model backend, a context contributor, a compaction strategy,
written in any language. The user drew the line: an external process
supplies implementations and data, never verdicts — the authority
plane (hooks, policies) stays in-process. What remains splits by
interface shape: seven capabilities share one sdk trait each, and
Service is an open interface per service, crossing differently
(ADR-0031).

The bridge already embodies the right pattern: `PluginTool` implements
the sdk's own `Tool` trait and its `run` is a wire call. A parallel
"remote trait" hierarchy would be a second representation of the one
contract — the debt ADR-0011 forbids.

## Decision

1. **Proxy structs, zero new traits.** Per opened capability the
   bridge owns one struct implementing the sdk trait
   (`RemoteContributor`, `RemoteCompactor`, `RemoteProvider`); its
   method bodies are wire calls. The kernel keeps seeing
   `Arc<dyn Trait>` and never learns which are remote. N remote
   plugins of one kind are N instances of one type — they differ by
   handshake data, never by type.
2. **Late sources, the ADR-0009 shape.** Registration is synchronous
   and does no I/O, so the sdk grows source variants mirroring
   `Tools`/`Commands` for each newly opened kind, resolved where tool
   sources are resolved today — one resolution point.
3. **This ADR opens Context and Compactor (M26), then Provider
   (M27).** Provider brings the wire's one new shape: a streaming lane
   (`provider/delta` notifications, cancellation passed through). An
   optional trait method stays optional — `count_tokens` unimplemented
   falls back to the sdk default.
4. **Every crossing has a deadline.** A contributor past it is dropped
   from that round with a notice — the turn is never blocked; a
   compactor or provider past one fails that call with the error the
   trait already speaks. The constants live in one place.
5. **Nothing a process says about itself is believed** (ADR-0015 §4
   unchanged): capabilities come from the manifest declaration, the
   unknown answers `false`, and a query crosses as its serializable
   projection — an external contributor reads the query, not the host.
6. **The schema is the count.** Methods land in `wire`, the generated
   `schema/plugin.json` is regenerated, `PROTOCOL` is bumped; the
   ADR-0015 §3 pin "four methods" becomes a pin derived from the
   schema, not a literal.

## Consequences

- An external author writes JSON handlers in any language and no Rust,
  for every capability this ADR opens; the one-time Rust per
  capability is the bridge's proxy, ours.
- Store is deliberately not opened: every frame's append is the
  hottest write path, and lock semantics would have to survive process
  death. A remote store is its own ADR if demand shows, opt-in per
  profile, with the in-process store staying the default.
- Surface is not opened here because its door already exists: an
  external client speaks JSON-RPC or ACP to a surface; that lane is
  not the plugin wire's to duplicate.
- A slow external contributor costs its own pieces, never the turn;
  the notice says whose deadline was missed.

Refs: ADR-0009 §1, ADR-0011 §1, ADR-0015, ADR-0031.
Non-goals: Hook or Policy over the wire; Store; a parallel remote
trait hierarchy; any change to in-process registration.
