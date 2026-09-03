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

- [x] Cold `/models refresh` lists the agent's declared models
  (black-box, fake agent).
- [x] One probe per refresh, cached; a live session supersedes.
- [x] A failing probe degrades to `agent` with one notice.
- [x] Every AGENTS.md gate; no new dependency; Windows cross-check
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

## Verified

`bingo-provider-acp/src/probe.rs`: the ask, the per-run cache and the
notice. It writes no spawn and no handshake of its own — it calls
`Sessions::{inbox, spawn, fresh}` and `session::handshake`, the same four
`open` climbs through, widened to `pub(crate)`. It skips what an opening
*for a person* needs: no bridge, no ladder, no journal, no map entry, and
no `preset` — a row's options move a knob's value, not the list of
values, so the harvest is the same without them. `session.rs` grew 48
lines (709 → 757, warn only).

**Freshness.** There is no signal on the door. `Provider::models()` takes
no argument, and both callers — `refresh::in_background` (stale or never
asked) and `refresh::now` (`/models refresh`, everybody) — reach the same
method. So the gate is the host's and stays there: the plugin asks at most
once per run per adapter, and how often a *process* asks is decided
upstream by `ServedModels::stale` and by a person typing refresh. Nothing
was invented to tell the two apart.

**`ServedModels` persists** — `models/served.rs`, one
`data_dir/served-models.json` written atomically by `record` and read by
`load`. So the harvest outlives the run for free; the black-box scenario
lists the models from a *second* process that asks nothing.

**Found beyond the plan.** The boot top-up asks every stale provider, so
on a machine that has never asked, the cold ask happens at boot as well
as on refresh — bounded, in the background, once a day per adapter. Two
consequences: the notice is kept until a session is open to hear it (the
kernel drops one said before there is), and every black-box scenario that
is *not* about the catalogue now starts on a machine that was asked
before (`Scripted::asked_before`), or a second child would arrive in its
agent's log at a moment nothing can pin down. `Host::model` also calls
`models()` when no model is named, which pays for one ask to be told
`agent`; harmless, and the same one ask.

```
$ cargo test -p bingo-provider-acp --locked --test catalogue
running 7 tests ... test result: ok. 7 passed; 0 failed; finished in 0.04s

$ cargo test -p bingo --locked --test cli acp::
running 32 tests ... test result: ok. 32 passed; 0 failed; finished in 2.27s
  (acp::catalogue::a_cold_refresh_lists_the_agents_own_models,
   a_live_sessions_declaration_supersedes_the_cold_one,
   a_probe_that_cannot_start_the_adapter_serves_the_label_and_says_so)

$ cargo fmt --all -- --check                       # clean
$ cargo clippy --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile ... in 1m 03s           # exit 0
$ cargo test --workspace --locked                  # 1454 tests, 0 failed
$ scripts/check_discipline.sh                      # discipline ok
$ scripts/budget.sh                                # 310 (max 310), budget ok
$ cargo deny check                                 # advisories/bans/licenses/sources ok
$ cargo check -p bingo-provider-acp --all-targets \
      --target x86_64-pc-windows-msvc
    Finished `dev` profile ... in 18.94s
```
