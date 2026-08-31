# 0017 — Named provider instances and paste login for keys

## Context

The settings hold one endpoint per wire shape — `openai`, `anthropic`, `codex`, each `Merge::Replace` — so a person with two OpenAI-compatible proxies, or two ChatGPT subscriptions, can hold one at a time. And a key-based provider answers `/login` with `Unsupported`, so the only way to hand one a key is to edit a settings file or export an environment variable, while the interactive machinery (the `Login{Paste}` interaction, the 0600 `auth.json` with its `Entry::Api{key}` shape) already exists for the subscription flow. The kernel needs nothing: `/login <id>` routes by registered id, `/model <id>/<model>` splits against the registry, `--provider <id>` resolves the same way, and the credential store keys entries by arbitrary string.

## Decision

1. **Instances live inside each plugin's own settings key.** `openai`, `anthropic` and `codex` each gain `instances: { <name>: {…} }` — the same fields as their parent minus what an instance cannot have (`codex` instances carry only `baseUrl?`/`issuer?`; `openai`/`anthropic` instances carry `baseUrl?`, `apiKey?`, `images?`). No new top-level key, no cross-plugin claim; the maps ride the existing `Merge::Replace` slices.
2. **An instance registers under its own name.** Each entry becomes one more `registrar.provider(...)` whose `id()` is the instance name — so `--provider work`, `/model work/gpt-5.4`, `/login work` and the catalogue all work with zero kernel changes. A name that collides with a built-in id (`openai`, `anthropic`, `codex`, `fake`) or with another instance is a `PluginError::Config`, refused at boot, never half-registered.
3. **Credentials are keyed by the instance's id.** A codex instance's `TokenSource` uses the instance name as its store id, so `bingo login work` and `bingo login personal` hold two subscriptions side by side in one `auth.json`. A key instance resolves its key as: the `auth.json` `Api` entry under its name, else its own `apiKey` setting. **Environment variables bind to the default instances only** (`OPENAI_API_KEY` feeds `openai`, never `work`): one ambient variable must not silently feed every proxy.
4. **Key providers accept `LoginMethod::Paste`.** `/login <id>` (or `bingo login <id>`) on a key-based provider opens the existing paste interaction and stores `Entry::Api{key}` under the provider's id; `/logout <id>` deletes it. Browser and device stay `Unsupported` for keys — there is no issuer to talk to. The auth status names where the key came from (auth.json, setting, or the env var for a default instance).
5. **A codex instance logs in exactly as `codex` does** — same issuer and flows, its own store entry. Nothing about OAuth changes but the key under which the tokens rest.
6. **`bingo provider add` writes the settings entry a person would otherwise edit in.** At the binary edge (the `bingo login` precedent: a terminal prompter, `anyhow` allowed), it asks for a name, a shape (`openai` | `anthropic`), a base url and — optionally, hidden — a key; the instance lands in the **user** settings layer, the key in `auth.json`, and the closing line says `bingo --provider <name>`. Registration stays a boot-time fact: the command runs before a kernel exists, so nothing is hot-added and no provider source is invented. The settings file is round-tripped as JSON (`preserve_order`); a file that does not parse is left untouched and the command says so.

## Consequences

- Crates touched: `bingo-provider-openai`, `bingo-provider-anthropic` (instance loops, paste login, resolution order), black-box tests in `crates/bingo/tests/cli/login.rs` territory. `bingo-auth-oauth`, `bingo-core`, `bingo-sdk`, the bin: untouched.
- No new dependencies; the budget stands.
- A settings layer that replaces `openai` replaces its instances with it — the documented cost of `Merge::Replace`, unchanged.
- The `providers` catalogue grows one row per instance; `/login`'s completion follows for free.
- Secrets keep their one rule: a key a person types lands in `auth.json` (0600), never in a settings file; the settings `apiKey` field stays for the environments that already manage it.

## Supersedes

—
