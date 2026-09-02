# M35 — The endpoint says what it serves

## Goal

`catalog(Models)` lists the embedded models.dev snapshot by a
provider's *family*, so a named instance called `deepseek` (an
OpenAI-shaped proxy, ADR-0017) offers OpenAI's models and never its
own. After this milestone a provider's models are the ids its
endpoint served — `Provider::models()`, which both providers already
implement — kept in a per-instance cache that outlives the process,
refreshed without ever standing in the way of a turn, and enriched
with the snapshot's facts; the `/model` picker follows a refresh live.
Two facts, two owners: the endpoint says what exists here, the
snapshot says what a model can do.

## Bricks, in build order

1. **`models/served.rs` — the cache, pure at its core.** One record
   per provider id: `{ fetched: Timestamp, models: Vec<ModelInfo> }`,
   in `data_dir/models/<provider id>.json`, the `Learned` precedent
   (`models/learned.rs`: a fact the server told us, kept across
   processes, missing or unreadable is an empty start). Pure
   functions: `merge(served, snapshot) -> Vec<CatalogEntry>`; `stale
   (fetched, now) -> bool` at one day. Fixture test for the file
   shape.
2. **Facts by family, then by id.** `ModelCatalog::lookup(family,
   id)` misses for `deepseek-v4-pro` behind an `openai`-shaped proxy.
   A miss falls back to a lookup by id across families — a model id
   belongs to its maker, whoever proxies it. `resolve_model` in
   `host.rs` gains the same fallback, so `/think` stops failing closed
   on a proxied model the snapshot knows. Test both.
3. **The catalogue read.** `host/catalog.rs::models`: for each
   provider, the served list if there is one, each id enriched via
   (2); else the family's snapshot list as today. `meta.source` is
   `"endpoint"` or `"catalogue"`. The configured model stays first.
4. **Refresh, never in the way.** At `Host::build` the cache is
   loaded synchronously; one background task per provider fetches
   `models()` for those whose cache is missing or stale, overwrites on
   success, keeps the old record and logs on failure, and publishes
   `Event::CatalogChanged { kind: "models" }` once per provider that
   changed. A provider whose `auth()` is not usable is not asked. The
   host's shutdown does not wait for a fetch; the task holds a `Weak`.
5. **`/models refresh`** (kernel command beside `/model`, instant):
   fetches every usable provider now, answers with a count per
   provider and the failures by name. Bare `/models` prints the
   catalogue with source and age. ADR-0008 §4 lists it.
6. **The picker follows.** `bingo-surface-tui` reads catalogues "once
   at start" (`commands.rs:101`); on `CatalogChanged { kind:
   "models" }` it re-reads that one catalogue through the spawned-call
   path (`run.rs`'s `replies`), never awaiting the kernel in the loop.
   Test: a `CatalogChanged` frame changes the `/model` completions.
7. **ADR-0026 §4 amended** in two sentences: what was refused is a
   network call inside a read-only tool and a second source of
   *facts*; a cached, background-refreshed list of *ids* is neither.
   `ListModels` (the tool) reads the same catalogue and so sees the
   served ids for free; its text says the source.

## Files

`bingo-core/src/models/{served,catalog,resolve}.rs`, `host.rs`,
`host/catalog.rs`, `commands/models.rs` (new); `bingo-surface-tui/src/
{run,commands}.rs`; `docs/adr/0026-the-model-catalog.md`,
`docs/adr/0008-commands.md`; black-box in `crates/bingo/tests/cli/`.

## Exit criteria

- [ ] With `Fake` declaring `["fake-1","fake-2"]`, `catalog(Models)`
  lists both with `source: endpoint` after the first refresh, and only
  the snapshot's before it.
- [ ] A cache file written by one process is what the next process
  answers with before any fetch.
- [ ] A failing `models()` keeps the old cache; the log says so; the
  catalogue is unchanged.
- [ ] `lookup` finds `deepseek-v4-pro` under an `openai`-family
  instance; `/think high` there no longer warns.
- [ ] `/models refresh` on the fake provider answers with a count;
  `CatalogChanged` follows and the TUI's `/model` completions change.
- [ ] Every gate in AGENTS.md. No new dependency.

## Non-goals

Fetching inside a tool call. Per-provider fetch logic beyond the
existing `Provider::models()`. Writing proxy models into the snapshot.
A settings key for the staleness window (one day, a constant, until
someone needs otherwise). Facts from the endpoint — `/v1/models`
carries none worth trusting.

## Risks

- A proxy that lists hundreds of ids makes the picker long; the
  configured model stays first and ranking is prefix-first already.
- `host.rs` is also touched by M34-B (`/think`); merge M34 first or
  rebase before the merge.
- A provider whose `models()` blocks on a slow endpoint holds only
  its own background task; the timeout is the provider's client's.
