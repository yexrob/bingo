# M30 — The allowance and the host's one service (ADR-0033)

## Goal

A host capability can be lent to a plugin for exactly one crossing:
the bridge registers the reserved `bingo.host` service, `complete`
lets an external compactor call the session's own model with the
usage measured and billed, `ask` lets a bridge tool put a mid-run
question to the person, `notice` lets a plugin say one line any time
— and the compactor activation key makes an external strategy
actually runnable.

## Bricks, in build order

1. **The allowance brick, pure first** (new `allowance.rs` in
   plugin-rpc) — the table: `mint(grant) -> id`, `claim(id) ->
   Option<Grant>`, `expire(id)`; `Grant` holds what a crossing lends
   (v1: `Complete { provider, model, cancel }` only — a door mints
   only where its scoping fact exists nowhere else, ADR-0033 §5 as
   amended). Ids are unguessable enough to not collide, entries die
   with their crossing, and the table is the bridge's ONE.
   Table-tested pure: mint/claim/expire, a dead id refused in words,
   expiry idempotent.
2. **`bingo.host`, the door service** (new `doors.rs` or in
   `service.rs`) — an in-process `WireService` the bridge registers
   through M28's `open_service` at start; methods `complete`, `ask`,
   `notice`; an unknown method answers with the spoken set (the M28
   rule, same words). Wire fixtures for the three methods' params and
   results land first; the handshake changes NOT AT ALL — this is a
   service like any other, discovered by calling it.
3. **The `complete` door** — `RemoteCompactor::compact` mints a
   `Complete` grant from the `CompactContext` it already holds, sends
   the allowance id in the params (wire struct grows the field,
   fixture updated), and expires it when the reply or the deadline
   lands. The door drains the provider stream to one response, meters
   usage per allowance, and `RemoteCompactor` folds the measured
   usage into the returned `Compaction.usage` — the claim is never
   the ledger; pin that with a lying stub.
4. **The `ask` door** — takes `{call, question}`; **nothing is
   minted**: the door validates the call against the bridge's
   existing running-call map and the calling connection (the call's
   liveness is the grant — ADR-0033 §3 as amended; a second id for a
   fact that map already carries is the ADR-0011 debt). It routes to
   the live call's own asking machinery, so the question rides the
   interaction system every tool's questions ride — no second path.
   An ended call, or another connection's, is refused in words.
5. **`notice`** — no allowance; rides the bridge's existing notice
   path under the plugin's name; level clamped to the sane set.
   M29 carried the path's defect here: `Notices` drains inside
   `PluginTool::call`, so in a session whose plugin serves no tool a
   queued notice is logged but unsaid. Give the channel a drain that
   does not wait for a tool call (the bridge holds the host at
   start); a hook's `HOOK_UNANSWERED` and this door's notices ride
   the same fixed drain — one channel, one drain, said when it
   happens.
6. **The activation key** (the M26 Carried) — a settings key naming
   the active compaction strategy; the slot resolves to the named id
   wherever it lives (in-process or a source's), else today's
   first-wins rule; refused in words when the name matches nothing.
   Find the key's natural home in the settings texture; do not mint
   a new claim if the kernel's own slice is the fit.
7. **Black-box** (`stub_plugin.rs`, `tests/plugin/allowance.rs`) —
   an external summarising compactor runs end to end: activation key
   set, compact call carries the allowance, the stub calls
   `complete`, the measured usage lands in the compaction; a stub
   tool asks mid-call and acts on the answer; `notice` surfaces; a
   spent or foreign allowance is refused in words; deadlines on a
   paused clock.

## Files

New `crates/bingo-plugin-rpc/src/{allowance,doors}.rs`,
`crates/bingo-plugin-rpc/src/{wire,schema,compactor,tool,bridge,
manager,notice,service,lib}.rs`, the compactor-slot key where the
settings texture puts it (`bingo-core` and/or `bingo-context`),
`crates/bingo-plugin-rpc/examples/stub_plugin.rs`, new
`crates/bingo-plugin-rpc/tests/plugin/allowance.rs`, the generated
`schema/plugin.json` if any wire struct changed. No new dependencies;
budget unchanged.

## Exit criteria

- [ ] the allowance table: mint/claim/expire pure-tested; a grant
      never outlives its crossing; a dead or foreign id refused in
      words
- [ ] `bingo.host` registered through `open_service`; unknown methods
      answer with the spoken set; no handshake change
- [ ] `complete`: an external compactor calls the session's model;
      usage measured by the host and folded into `Compaction.usage`
      (a lying claim does not win); allowance expires with the reply
- [ ] `ask`: a bridge tool's mid-run question rides the one
      interaction path and returns the person's answer; an ended or
      foreign call refused in words; nothing minted for it
- [ ] `notice` surfaces under the plugin's name with no allowance
- [ ] the activation key selects a source's strategy in the shipped
      composition; a name matching nothing refused in words
- [ ] every gate green (fmt, check, clippy, test, discipline, budget
      unchanged, deny)

## Non-goals

Doors for sessions, deliver or store (ADR-0033 §5's socket takes them
later); allowances on the contributor path; streaming through
`complete` (one request, one drained response); any change to the
permission plane; renewing or transferring a grant.

## Risks

R-ambient — the whole point: no door answers without a live grant,
and no grant survives its crossing; the table is the single source.
R-ledger — usage is what the host measured; the plugin's claimed
numbers never overwrite it. R-ask — reuse the interaction machinery
every tool question rides; a parallel question path is the ADR-0011
debt. R-socket — adding a future door must cost one method and one
minting site only; if it costs more, the shape drifted — stop and
report.
