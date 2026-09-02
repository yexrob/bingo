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

## Verified

2026-09-02, worker F, worktree branch `worktree-agent-aa41b5e66954af54a`.

- [x] With a provider declaring `["deepseek-v4-pro","glm-5"]`,
  `catalog(Models)` lists both with `source: endpoint` after the first
  refresh, and only the snapshot's shelf before it —
  `host::tests::models::what_the_endpoint_answers_replaces_the_shelf_it_was_filed_under`,
  and the background one lands on its own in
  `…::the_list_arrives_on_its_own_after_the_host_is_up`.
- [x] A cache file written by one process is what the next answers with
  before any fetch —
  `…::a_list_one_process_cached_is_what_the_next_one_answers_with`.
- [x] A failing `models()` keeps the old cache and the failure is named —
  `…::an_endpoint_that_cannot_be_asked_keeps_the_list_it_gave`
  (`tracing::warn` at `host/refresh.rs`).
- [x] The facts of a proxied or dated id are found under the family, then
  the id — `models::catalog::tests::an_instance_reads_its_family_s_shelf_then_its_own_name_s`
  and `host::tests::models::a_named_instance_reads_its_family_s_shelf_then_the_id_s_own`,
  which pins `reasoning` true for `deepseek-v4-pro` behind an
  `openai`-family instance. `/think`'s own wording is M34-B's.
- [x] `/models refresh` answers with a count per provider and the
  catalogue then says `from the endpoint` —
  `cli::models::the_models_command_lists_the_catalogue_and_refreshes_on_demand`;
  a `CatalogChanged` reaches a client
  (`host::tests::models::a_changed_list_is_announced_and_an_unchanged_one_is_not`)
  and the TUI's `/model` completions follow it
  (`run::tests::the_completions_follow_a_catalogue_the_host_rebuilt`).
- [x] Every gate. No new dependency.

```
$ cargo fmt --all -- --check
fmt clean

$ cargo clippy --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.76s   (exit 0)

$ cargo test -p bingo-core --locked
test result: ok. 228 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test -p bingo-surface-tui --locked
test result: ok. 489 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test -p bingo --locked
tests/cli/main.rs   ok. 137 passed; 0 failed
tests/rpc.rs        ok.  13 passed; 0 failed
tests/pty.rs        ok.   2 passed; 0 failed
(channels 7, instances 1, login 1, plugin_rpc 4, rooms 2, views 3, unit 57 — all ok)

$ scripts/check_discipline.sh
dependency direction ok · kernel names no tool · cohesion ok · discipline ok

$ scripts/budget.sh
dependencies (unique, normal): 302 (max 302)
warm cargo check -p bingo-core: 0s (max 20s)
relink isolation: touching the TUI recompiled 0 crates for core (must be 0)
budget ok

$ cargo check -p bingo-core --all-targets --locked --target x86_64-pc-windows-msvc
$ cargo check -p bingo-surface-tui --all-targets --locked --target x86_64-pc-windows-msvc
    Finished `dev` profile   (exit 0, both: the cache writes through std fs alone)
```

Where the plan did not survive contact:

- **One file, not one per provider.** `data_dir/served-models.json` holds a
  map keyed by provider id, as `learned-windows.json` does. A provider id is
  a name out of a settings file, and `data_dir/models/<id>.json` is a path a
  settings file could steer.
- **The announcement is the gateway's, not a session's.**
  `GatewayEvent::CatalogChanged{Models}`, which nothing published before and
  which `docs/design/gateway-and-surfaces.md` already names, rather than
  `Event::CatalogChanged` on every open session: a catalogue is the host's
  fact, the session event is durable and would land in every transcript's
  journal, and a refresh that finishes before the first session opens would
  otherwise reach nobody. The TUI reads `gateway_events()` beside its frame
  stream.
- **`meta.source` has three values**, not two: `endpoint`, `catalogue`, and
  `configured` for an id only the settings name. Two would have made the
  third a lie.
- **`ModelCatalog::lookup` already fell back across providers** (ADR-0004 §3,
  exact id anywhere), so brick 2's miss was elsewhere: `resolve_model` looked
  under the *instance name* rather than the family, so a named instance got
  no facts for its own family's dated ids. The ladder now lives in
  `ModelCatalog::facts_for`.
- **A black-box run no longer inherits `ANTHROPIC_API_KEY`/`OPENAI_API_KEY`**
  (`crates/bingo/tests/cli/main.rs`): every `Host::build` now asks the
  endpoints it can sign in to, and a suite must not reach the network because
  whoever ran it has a key exported.
