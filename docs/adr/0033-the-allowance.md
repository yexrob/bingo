# ADR-0033 — The allowance: a host capability, lent for one crossing

Status: accepted · 2026-09-01 · Plan: M30 · amended 2026-09-01
(scope: the table and the `complete` door are deferred to demand — no
external strategy exists to spend through them; `ask` and `notice`,
which mint nothing, land first as M30)

## Context

M26 deliberately projected the provider out of a remote compactor's
context — a remote strategy summarises by its own means or cuts by
none — which leaves external strategies second-class against the
in-process one that calls the session's model. The user asked for host
components as services. The blanket version would dissolve the Hub's
promise (nothing else in the host is reachable from a process), and an
ambient model service would spend the user's tokens with no session to
bill, no authorization, and no visibility. The taxonomy that survived
the discussion: what is visible and attributable (spawn, post) crosses
as a plugin-owned service (ADR-0031); what only observes rides
ADR-0032's observe lane; what **spends** — money, the person's
attention, or their private data — crosses as an allowance: minted for
one crossing, metered, and dead when the crossing ends. The family's
test is irreversible outflow of something the person owns: what the
permission gate already checks per action, and what the journal
already attributes, need no allowance.

## Decision

1. **One reserved service, `bingo.host`, registered by the bridge.**
   The transport is ADR-0031's own lane unchanged: a process reaches
   it with `service/call`, the Hub routes it by key, and no new wire
   method exists. The bridge constructs the one in-process
   `WireService` behind it and enters it through `open_service`; its
   methods are the doors this ADR and its successors open.
2. **An allowance is minted where a crossing begins and dies where it
   ends.** The bridge's proxy already holds the live context — the
   compact call's provider, the tool call's asking host — so minting
   records `{id → grant}` in the bridge's one table, the id travels in
   the crossing's params, the door validates against the table, and
   the reply (or the crossing's deadline) removes the entry. A grant
   never outlives its crossing; there is no ambient spend.
3. **v1 opens two doors.** `complete {allowance, request} →
   {response}`: minted for `compactor/compact`, routed to the provider
   the session already chose; **usage is measured by the host** and
   folded into the crossing's accounting, so the person sees the spend
   on the stream and a claim is never the ledger. `ask {call,
   question} → {answer}`: **nothing is minted** — the bridge already
   tracks running calls for progress and cancel, and that liveness is
   the grant: the call id the plugin holds names the crossing, and an
   ended call or another connection's is refused in words. The
   question rides the live call's own asking machinery, exactly the
   way an in-process tool's does; no second question path exists.
   (Amended 2026-09-01 in review: the first cut minted an Ask
   allowance — a second id for a fact the running-call map already
   carries is the ADR-0011 debt.)
4. **`notice {level, message}` takes no allowance.** A plugin may tell
   the person something at any time, under its own name, on the
   bridge's existing notice path. It is the one unscoped method; it
   spends nothing but a line.
5. **The socket for successors** (user-directed): a future capability
   — sessions, deliver, a store read — enters as one more method on
   `bingo.host` plus one scoping fact, each behind its own ADR
   paragraph naming its scope and its accounting. **A door mints only
   where its scoping fact exists nowhere else**; one already carried —
   a live call, a live crossing — is itself the grant, reused, never
   re-issued. The key, the table, the validation and the routing are
   this ADR's and do not change; what is not a method does not exist
   across the line, so the Hub's promise stands sentence for
   sentence.

## Consequences

- An external compactor is first-class: an extractive strategy stays
  free, a summarising one calls the session's own model rung, and the
  spend is on the stream. The compactor slot's activation key (the
  M26 Carried) ships with this door, so an external strategy can
  actually run in the shipped composition.
- Rings and abuse are bounded as M28 bounds them: one deadline per
  call, one grant per crossing, nothing ambient, nothing renewed.
- An allowance grants a spend, never a permission: the verdict plane
  (ADR-0032 §4) is untouched, and no grant is Allow-shaped.
- Contributors mint nothing in v1 — the hot path has no customer yet;
  when one appears, it is a minting site away, not a redesign.

Refs: ADR-0026 §4, ADR-0030, ADR-0031, ADR-0032.
Non-goals: ambient model access; doors for sessions, deliver or store
(successor ADRs through §5's socket); allowances on the contributor
path; any permission-shaped grant.
