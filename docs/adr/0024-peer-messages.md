# ADR-0024 — Peer messages: sibling addresses, one delivery

Status: accepted · 2026-08-31 · Plan: M20

## Context

Addressing is vertical: `names::resolve` reaches the caller's children,
`parent`, and `#room`s — never the teammate beside it. The old tree's
collaboration survey shows what that forecloses: peer review (a builder
writing its reviewer directly) and "DM for production, room for decisions"
both need a sibling's name to work, or the parent must relay every message.
Rooms already resolve child-then-sibling — a member and its room are
children of the same session; agent names don't.

And there are two message tools with a footgun between them. `SendMessage`
is `Delivery::Hold`: a message to an *idle* agent lies in its queue until
something else opens a turn. The common case — a child asks its parent and
ends its turn; the parent answers — silently strands the answer unless the
parent remembers to use `FollowupTask` (Wake) instead. Wake and Hold behave
identically on a busy target (queue, read at the barrier or the turn's
end); they differ only on an idle one, where Hold's behaviour is almost
never the wanted one.

## Decision

1. **Agent names resolve child-first, then sibling.** The rule rooms
   already use, applied to agents: the caller's children, then its parent's
   other model-driven children, the caller itself excluded. A caller's own
   child shadows a sibling of the same name — deterministic, said in the
   tool description. `parent` and `#name` are unchanged; the kernel's
   depth limit (one) keeps the address space flat.
2. **One delivery.** `SendMessage` delivers `Delivery::Wake`: an idle
   target starts a turn on it, a busy one reads it mid-run or at the turn's
   end. `FollowupTask` is deleted — registration, manifest line, prompt,
   and every reference swept.
3. **The roster tells the truth.** `ListAgents` and the unknown-name hint
   list siblings too, marked as such; `WaitAgent` resolves names the same
   way `SendMessage` does.
4. **A DM carries no obligation.** No ack timers, no debts on direct
   messages. Needing an answer from someone is a room `@name` (ADR-0022):
   a DM speaks, a mention owes.

## Consequences

- A sub-agent — which is never offered `SpawnAgent` — finally has someone
  to write to besides its parent and the rooms: its teammates. Peer review
  runs without the parent as a switchboard.
- One tool fewer, and the delivery question disappears from the model's
  choices; the stranded-answer footgun cannot be expressed.
- A sibling gains the power to reach a busy sibling's barrier — the same
  power `FollowupTask` already gave the parent, now symmetric among the
  children of one session. The recipient's own gate still rules whatever
  it then does.
- A cheap FYI to an idle agent now costs one short turn ("read it, done")
  where Hold cost none — accepted: an unread message that might never be
  read was the worse economy.
