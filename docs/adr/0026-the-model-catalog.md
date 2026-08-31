# ADR-0026 — The model catalog reaches the model

Status: accepted · 2026-08-31 · Plan: M20

## Context

`SpawnAgent` takes `model` and `provider`, but the calling model cannot see
what exists: which providers are registered and authenticated, what models
they serve, which of those see images, reason, or carry a large window. So
spawns either inherit blindly or guess ids.

The knowledge is already in the process, one representation each: the
embedded models.dev snapshot (`ModelCatalog`, per-model
`ModelFacts { context_window, max_output, reasoning, images }`, refreshed
by `scripts/models_dev.sh`), `catalog(Models)` filing `provider/model` by
family, and `catalog(Providers)` carrying each provider's auth status.
Nothing hands any of it to a model.

## Decision

1. **Enrich, don't duplicate.** `catalog(Models)` entries carry the facts
   the embedded catalogue already holds in their `meta` — `context`,
   `output`, `reasoning`, `images` beside the existing `provider` — via
   `ModelCatalog::lookup`. `CatalogEntry` is a wire shape the RPC surface
   serializes, so the meta keys get a fixture test.
2. **One read-only tool, `ListModels`** (`bingo-agents`, no arguments):
   renders the Providers catalog (id, auth state) and each provider's
   models with their facts — the same catalog every surface reads, as text
   a model can act on.
3. **`SpawnAgent` points at it**: the `model` and `provider` field docs say
   to call `ListModels` when choosing rather than guessing an id.
4. **Refused.** An `image_gen` capability flag: nothing in bingo can
   request image generation, and a flag nothing reads is a brick for an
   imagined future — carried until an image-generating tool exists, at
   which point the snapshot's output modalities are the source. Also
   refused: calling `Provider::models()` live from the tool — a network
   call inside a read-only tool, and a second answer beside the embedded
   snapshot; the live list stays where it is, serving the auth'd flows
   that already use it.

## Consequences

- An orchestrator can staff deliberately: list, pick the vision-capable or
  long-context model, spawn with `provider`/`model` — across providers.
- Zero new state and zero sdk changes: `meta` is already free-form; the
  whole change is a richer read-out of facts the kernel resolved anyway.
- Model facts age with the snapshot and the tool says so; the refresh
  cadence of `scripts/models_dev.sh` is unchanged.
