# 0004 — Model facts: the catalogue owns the model, the provider owns the endpoint, the server corrects the window

## Context

A turn needs six facts about the model it talks to: context window, output budget, whether it reasons, whether it sees images, whether the endpoint counts tokens, whether it caches prefixes. M1 let each provider answer all six from a hand-written family table — the table the old project grew to 400 lines and still measured every non-Claude model with a Claude ruler. Two providers now need the same numbers, and the numbers are published: models.dev keeps limits and modalities for 200 providers and 7 000 models. Keeping a copy in every provider would be a second representation of one fact. The server itself is the one source that is never out of date about its own window: a 400 overflow names the real limit.

## Decision

1. **Three owners, no overlap.** The model's own facts — window, output budget, reasoning, image input — belong to the kernel catalogue (`bingo_core::models`). The endpoint's facts — forwards images, counts tokens, caches — belong to the provider: `Provider::endpoint(model) -> EndpointCapabilities{images, count_tokens, caching}`. The user's settings override the catalogue; the server's overflow message clamps it.
2. **One resolved type.** `bingo_sdk::ModelCapabilities` is the resolution a turn reads, produced by the pure `models::resolve(declared, learned, catalogue, endpoint)`: `declared > learned clamp > catalogue > default`, with `images = model && endpoint`. The default for an unknown model fails closed on what a wrong guess would 400 on (8k output, no reasoning) and open on what the server corrects (200k window) or the user sees (images).
3. **The catalogue is data, not code.** `crates/bingo-core/models.dev.json` is the raw models.dev `api.json` pruned to the fields read (`limit`, `reasoning`, `modalities.input`) by `scripts/models_dev.sh`; the kernel parses that one shape, so a runtime refresh is a download and nothing else. Lookup: `(provider, model)`, then the longest model id in that provider that prefixes the request (dated snapshots), then any provider with the exact id (proxies), then default.
4. **Declared overrides are a kernel settings key.** `models` (the fifth kernel key after ADR-0003's four): `"<provider>/<model>": {contextWindow, maxOutput, reasoning, images}`, every field optional, objects deep-merged like any other key.
5. **Learned windows are recorded by the turn, applied at open.** On `ProviderError::ContextOverflow` the turn parses the message (`A + B > C`, "maximum context length is N", "resulted in N tokens" at 85 %), records `(provider, model) → window` in the host's in-memory map, and emits a notice. The next session opened on that model resolves with the clamp; the session that learned it is the overflow ladder's job (M4). A learned value below 8 000 or above 10 000 000 is a misparse and is dropped.
6. **`max_tokens` is the kernel's.** `min(declared maxTokens | min(max_output, 32 000), window / 2)`: the effective window is never under half the model's, so no declared window collapses the compaction thresholds.
7. `ModelInfo` from `Provider::models()` carries `id` and `display` only; capabilities of a listed model come from the same resolver.

## Consequences

- The sdk changes once for M2: `Provider::capabilities` → `Provider::endpoint`, `ModelCapabilities` re-documented as resolved, `ModelInfo.capabilities` removed. Touched plugins: `bingo-provider-fake`, `bingo-provider-anthropic` (its `models.rs` table is deleted), `bingo-provider-openai` (new).
- A new model needs no code: regenerate the snapshot, or declare it in settings until then.
- The kernel never contacts models.dev; refresh is a plugin's or a command's job (M3).
- A provider that reasons by default server-side (DeepSeek behind an OpenAI-compatible URL) is declared `reasoning: false` so the wire parameter is not sent; the catalogue cannot know which endpoint fronts which model.

## Supersedes

—
