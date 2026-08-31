# M20 — Peers, the serial room, the catalog (ADR-0024, 0025, 0026)

## Goal

An agent can write to the teammate beside it; one delivery that always
arrives (`SendMessage` wakes, `FollowupTask` dies); a room that refuses a
post written behind its head and hands back what was missed; and eyes for
the model landscape — `ListModels` shows providers, models and their
facts, so a spawn chooses instead of guessing.

## Bricks, in build order

**Worker H — peers and the serial room (`bingo-agents`):**

1. **Sibling resolution** — `names::resolve` child-first-then-sibling for
   agent names (parent's other model-driven children, caller excluded;
   own child shadows). `WaitAgent` rides the same resolution; `ListAgents`
   and the unknown-name hint list siblings, marked as such.
2. **One delivery** — `SendMessage` delivers `Wake`; `FollowupTask`
   deleted everywhere (`rg FollowupTask` first: registration, manifest,
   `Kind`, note, docs, every test); descriptions rewritten for the wider
   address space and the DM-owes-nothing rule (ADR-0024 §4).
3. **The serial brick** — pure first: a verdict (land / behind-by-N with
   the missed posts) over (room snapshot, caller snapshot, cut item) per
   ADR-0025 §2–3, nudges (`principal: None`) never counted, quoted
   bounces counted as seen; then the room arm of `SendMessage` bounces
   with the quote. Verify the kernel's absorption order against the cut
   definition before building on it, and pin it with a test.
4. **Black-box** (`tests/cli/`, fake script) — a sibling DM review
   round-trip with the parent's transcript untouched; the idle-answer
   regression (child asks, parent's answer wakes it); a stale post
   bounces once, quotes the missed post, lands on retry; a bounced post
   opens no debt and answers none; fan-out exactly-once pinned.

**Worker I — the catalog to the model:**

5. **Meta enrichment** — `host/catalog.rs::models` meta gains `context`,
   `output`, `reasoning`, `images` from `ModelCatalog::lookup`; a fixture
   test pins the keys (wire shape, ADR-0026 §1).
6. **`ListModels`** — new read-only tool in `bingo-agents` rendering
   Providers (auth) and models (facts); manifest line; `SpawnAgent`'s
   `model`/`provider` field docs point at it.
7. **Black-box** — a fake-provider run lists `fake/…` with facts and auth;
   a spawn with an explicit `provider`/`model` still lands.

## Files

H: `crates/bingo-agents/src/{names,message,list,note,lib}.rs` + a serial
module, crate tests, `tests/cli/agents.rs`. I:
`crates/bingo-core/src/host/catalog.rs`,
`crates/bingo-agents/src/models.rs` (new), `spawn.rs` doc lines, `lib.rs`
manifest line, `tests/cli/models.rs` (new, own mod line). Both touch
`bingo-agents/src/lib.rs` (H: message/names lines; I: ListModels lines) —
the union merge is the integrator's. No new dependencies; budget 302.

## Exit criteria

- [x] a sibling resolves (own child shadows); roster and hints name siblings
- [x] `SendMessage` wakes an idle target; `FollowupTask` gone from every file
- [x] a stale post bounces with the missed posts quoted and lands on retry;
      a bounce unlocks even a lost fan-out; exactly-once fan-out pinned
- [x] a bounced post neither opens nor answers a mention debt
- [x] `catalog(Models)` meta carries context/output/reasoning/images (fixture)
- [x] `ListModels` lists providers with auth and models with facts;
      `SpawnAgent` docs point at it
- [x] black-box scenarios of bricks 4 and 7 green
- [x] every gate green (fmt, check, clippy, test, discipline, budget
      unchanged, deny)

## Non-goals

Room modes or per-room config; `ack_timeout` or any DM obligation; read
receipts; an `image_gen` flag; live `Provider::models()` in the tool;
AgentControl-style stop/delete tools; teams changes; kernel event or
session semantics changes — the core change is the catalog read-out only.

## Risks

R-order — the cut definition leans on where absorption lands in the
journal relative to assistant items: worker H verifies the kernel's actual
order first and pins it; if it differs, the cut moves, not the discipline.
R-sweep — `FollowupTask` references outside `bingo-agents` (surfaces,
permission fixtures): `rg` before deleting, sweep all. R-merge — shared
`lib.rs`/`spawn.rs` doc edits between H and I; small, unioned at merge.
R-drift — models.dev meta keys are wire-visible; the fixture is the lock.

## Verified (2026-09-01)

- Worker I merged `1b77d48` (catalog meta + `ListModels` + the rpc wire
  fixture); worker H merged `39a5ec6` (siblings, one Wake delivery with
  `FollowupTask` deleted everywhere, the serial room in `serial.rs`, the
  kernel barrier-order test). The union reconciled `lib.rs` to five tools
  and added `ListModels` to the discipline tool regex.
- H's amendment to ADR-0025 §2, accepted on review: the room's ledger
  starts at the caller's `created_at` — nobody is behind on a post fanned
  out before its session existed; the holder posts blind and is bounced
  once, which is §3's repair working, not an exception to it.
- Gates ran integrated with M21/M22 on `0dda5a7`: fmt / check / clippy /
  discipline / budget (302/302) / deny all exit 0; the workspace suite's
  one red was M22's relay black-box under full-parallel load — 3/3 green
  solo and the cli suite 121/121 green on a quiet machine. The evidence
  table sits in M22's plan.

## Carried

- The SeatHook seating race H diagnosed and did not chase: under parallel
  load a declared role occasionally never seats, `seat()`'s error
  swallowed into a tracing warn nobody sees. Worth chasing when it bites.
- `quoted()` parses the bounce ledger out of any tool result's text; a
  result that echoes a bounce verbatim could inflate "seen". Accepted at
  this scale, recorded here.
