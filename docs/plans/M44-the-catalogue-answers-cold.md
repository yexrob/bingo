# M44 — The catalogue answers cold

## Goal

`/models refresh` lists an ACP agent's models without a prior turn:
when the instance has no live link, `models()` performs one probe
handshake — spawn the adapter, `initialize`, `session/new`, harvest
the declared models and options, drop the child. Verified by hand
against real codex-acp 1.8.0: the `session/new` answer carries the
full list (`models.availableModels`, `model[effort]` ids) at zero
model cost. Amends ADR-0037's consequence line ("before one, `agent`
alone is served") — the probe replaces the shrug.

## Bricks

1. The probe in `bingo-provider-acp`: reuse the existing child/
   connection/handshake machinery (session.rs's open path) — no
   second spawn code; a bounded wait (the machine rule: no pinned
   clocks, a deadline generous for CI); the probe session is never
   journaled, never mapped, and the child is dropped after harvest.
   Failure (adapter not installed, no login) degrades to `agent`
   alone with one notice — refresh must never error the catalogue.
2. Cache the harvest per process beside the live-link derivation
   (same `Declared` shape, one representation — the probe fills the
   same store a session-opening fills); a live link's later, fresher
   declaration wins. Probe at most once per `models()` unless the
   host's refresh asks anew (read how ADR-0026's `refresh::ask`
   signals freshness — ride it, do not invent a flag).
3. Check whether `ServedModels` persists across runs (the ADR-0026
   path); if it does, the probe's result persists for free — say so
   in the plan's Verified; if it does not, note it and stop (durable
   persistence is its own decision, not this slice's).
4. Tests: scripted fake agent — a cold `models()` probes once and
   serves the declared list; a second `models()` serves the cache; a
   live session's declaration supersedes; a probe against a missing
   binary degrades to `agent` + notice. Black-box: `/models refresh`
   cold in a fresh run lists the fake agent's models.

## Files

`bingo-provider-acp/src/{provider.rs,session.rs,knobs.rs}` as the
cache placement demands; fake agent harness; `bingo/tests/cli/acp/
knobs.rs` or sibling; `docs/adr/0037-the-knobs-cross.md` (one
consequence line).

## Exit criteria

- [ ] Cold `/models refresh` lists the agent's declared models
  (black-box, fake agent).
- [ ] One probe per refresh, cached; a live session supersedes.
- [ ] A failing probe degrades to `agent` with one notice.
- [ ] Every AGENTS.md gate; no new dependency; Windows cross-check
  (`-p bingo-provider-acp`) — the probe spawns a process.

## Non-goals

Durable on-disk persistence beyond what ADR-0026 already does.
Probing at boot for every configured adapter (refresh-driven only —
if `refresh::ask` turns out to run at boot for every provider, the
probe still bounds itself and reports; do not block boot).

## Risks

- The probe leaves an orphan agent-side session (codex keeps its own
  state); harmless and unavoidable — recorded, not cured.
- Adapter startup slowness makes refresh feel slow; the wait is
  bounded and the degrade is honest.
- Conflicts with worker X's in-flight row-options slice in the same
  files — this milestone starts only after X lands.
