# M30 — The host's doors: ask and notice (ADR-0033, first slice)

## Goal

The bridge registers the reserved `bingo.host` service and opens the
two doors that have customers today: `ask` lets a bridge tool put a
mid-run question to the person on the one interaction path, and
`notice` lets a plugin say one line any time — fixing the drain M29
carried. The allowance table and the `complete` door wait for demand
(see Deferred).

## Bricks, in build order

1. **`bingo.host`, the door service** (new `doors.rs` or in
   `service.rs`) — an in-process `WireService` the bridge registers
   through M28's `open_service` at start; methods `ask` and `notice`;
   an unknown method answers with the spoken set (the M28 rule, same
   words). Wire fixtures for both methods' params and results land
   first; the handshake changes NOT AT ALL — this is a service like
   any other, discovered by calling it.
2. **The `ask` door** — takes `{call, question}`; **nothing is
   minted**: the door validates the call against the bridge's
   existing running-call map and the calling connection (the call's
   liveness is the grant — ADR-0033 §3 as amended; a second id for a
   fact that map already carries is the ADR-0011 debt). It routes to
   the live call's own asking machinery, so the question rides the
   interaction system every tool's questions ride — no second path.
   An ended call, or another connection's, is refused in words.
3. **`notice` and the drain** — no allowance; the plugin's name;
   level clamped to the sane set. M29 carried the path's defect here:
   `Notices` drains inside `PluginTool::call`, so in a session whose
   plugin serves no tool a queued notice is logged but unsaid. Give
   the channel one tool-independent drain (the bridge holds the host
   at start); a hook's `HOOK_UNANSWERED` and this door's notices ride
   the same fixed drain — one channel, one drain, said when it
   happens.
4. **Black-box** (`stub_plugin`, a doors test module beside the
   others) — a stub tool asks mid-call and acts on the answer; an
   ended or foreign call is refused in words; a notice surfaces with
   **no tool call in flight** (the drain proven); an unknown method
   answers the spoken set. Paused clocks throughout; nothing waits
   wall.

## Deferred (user scope call, 2026-09-01)

The allowance table, the `complete` door and the compactor activation
key wait for a real external strategy to exist. ADR-0033 keeps the
design on record — the family test, the socket rule — and when the
customer appears the cost is the table plus one minting site (§5),
not a redesign. Until then an external compactor stays
extractive-only and inert in the shipped composition; accepted.
M26's compactor crossing itself stays: merged, tested, inert,
harmless.

## Files

`crates/bingo-plugin-rpc/src/{wire,doors or service,notice,tool,
bridge,manager,lib}.rs`, `crates/bingo-plugin-rpc/examples/
stub_plugin/`, a new doors module under
`crates/bingo-plugin-rpc/tests/plugin/`, `schema/plugin.json` only if
a wire struct changed. No new dependencies; budget unchanged; no sdk
or kernel change expected (`open_service` already exists).

## Exit criteria

- [ ] `bingo.host` registered through `open_service`; unknown methods
      answer with the spoken set; no handshake change
- [ ] `ask`: a bridge tool's mid-run question rides the one
      interaction path and returns the person's answer; an ended or
      foreign call refused in words; nothing minted for it
- [ ] `notice` surfaces under the plugin's name with **no tool call
      in flight** — the drain no longer waits for one, and
      `HOOK_UNANSWERED` rides the same drain
- [ ] every gate green (fmt, check, clippy, test, discipline, budget
      unchanged, deny)

## Non-goals

The allowance table, `complete` and the activation key (Deferred);
doors for sessions, deliver or store; streaming; renewing or
transferring anything; any change to the permission plane; any
"while we're at it" table machinery — deferred means absent.

## Risks

R-drain — one channel, one drain: a second surfacing path is the
ADR-0011 debt; the fix must serve the hook's notices and this door's
alike. R-ask — reuse the interaction machinery every tool question
rides; a parallel question path is the same debt. R-scope — nothing
here anticipates the deferred work; if a brick seems to want the
table, the scope drifted — stop and report.
