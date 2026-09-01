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

- [ ] wire fixtures pin stream open, delta, close and cancel; deltas
      keyed by call; schema regenerated; `PROTOCOL` bumped
- [ ] handshake declares providers; `endpoint()` fail-closed pinned
      (an undeclared model is `false`)
- [ ] `ProviderSource` resolved at the one provider point; the model
      catalog lists a late provider's models through the built-in path
- [ ] a full turn runs on the stub's model, tool round-trip included;
      cancel reaches the process; mid-stream death yields the trait's
      error with kernel retry untouched
- [ ] idle deadline on a paused clock; constants in the one module
- [ ] every gate green (fmt, check, clippy, test, discipline, budget
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
