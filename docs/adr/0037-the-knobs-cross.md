# ADR-0037 — The knobs cross

Status: accepted · 2026-09-03 · Plan: M40

## Context

ADR-0035 §6 left `Effort` and the model label uncrossed: the agent
picks its own. The ecosystem has since converged on a wire for exactly
these knobs: `session/new` answers with `configOptions`, and the
client changes one with `session/set_config_option` — mid-session, no
restart. codex-acp serves reasoning effort and model as such options
(plus a legacy `session/set_model` whose model id spells
`model[effort]`); claude-agent-acp serves an option whose id is
`effort`; Zed's pickers are drawn from the same list. The schema this
workspace pins (1.5.0) already carries both types. Meanwhile bingo
drops `ModelRequest.reasoning` on the floor for an ACP session, and
`/model` can only say `agent`.

## Decision

1. **Effort crosses.** `ModelRequest.reasoning` is at the instance's
   hand every turn. The instance keeps the last value it applied per
   session; a difference is sent, before the prompt, as
   `session/set_config_option` on the effort-shaped option the agent
   declared, clamped to the values the option itself lists. An agent
   without the option keeps its own default — one notice says so,
   once.
2. **The model crosses as the agent's own list.** The models the agent
   declares are served as this instance's catalogue through the door
   every provider's served models already ride (ADR-0026); `agent`
   stays the label for "the agent's default". A chosen model is
   applied the same way — the model-shaped option, or the agent's
   legacy `session/set_model` where that is what it has.
3. **Nothing new is invented for the asking.** `/model`, `/thinking`,
   `SpawnAgent`'s `model` field and every path that reconfigures a
   session already end in the next request's `model` and `reasoning`.
   The instance reads the two fields it always received and applies
   what changed. No kernel word moves, no new tool, no new door.
4. **Applied between turns, never inside one** — the moment those
   knobs take effect for every provider already.

Amends ADR-0035 §6: `Effort` and the model now cross; `system`,
caching and token counting still do not.

## Consequences

- A value the agent does not offer is clamped to the nearest it does,
  in the option's own words; a knob the agent lacks is a notice once,
  never an error — the knob is the agent's, bingo only turns it.
- An ACP instance's catalogue is as fresh as its last `session/new`; with
  no conversation to read one from, the refresh opens a session of its own
  to ask and drops it (M44), and `agent` alone is what an adapter that
  would not answer serves.
- No schema bump, no new dependency, no kernel change; the scripted
  fake agent grows a `configOptions` capability to pin the contract.

Refs: ADR-0035 §6, ADR-0026; Plan: M40
