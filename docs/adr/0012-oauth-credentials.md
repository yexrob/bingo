# 0012 — OAuth credentials: a library tier, one store, login as an interaction

## Context

The Codex endpoint (M2's `Variant::Codex`) takes a ChatGPT-subscription bearer that only an OAuth flow yields — PKCE over a loopback callback, or a device code — and that expires and must be refreshed. The other providers take API keys from settings or the environment. The sdk anticipated this since M0: `Provider::{auth, login(prompter)}`, `InteractionKind::Login{provider, flow}`, `LoginFlow::{Browser, Device, Paste}`; nothing calls `login` yet and no crate holds a token. The flows are pure over an issuer, and a second provider will want them (Claude's own subscription login), yet ADR-0001 knows only two tiers below the bin — the kernel and plugins — and forbids a plugin importing a plugin; the service registry passes runtime objects, not code. M2 met the same gap ("`sse.rs`, the idle guard, `retry-after` are written twice… the third consumer is when they move to one crate"). A token must never land in settings, whose project layer is committed. A login takes minutes and needs a person, yet a command runs outside a turn and today an interaction needs a running turn.

## Decision

1. **A library tier.** A workspace crate declaring `[package.metadata.bingo] tier = "library"` registers nothing, depends on `bingo-sdk` and external crates only, and any plugin may depend on it. `bingo-auth-oauth` is the first; `scripts/check_discipline.sh` asserts the tier's edges.
2. **One credential store.** `<data_dir>/auth.json`, mode 0600, written whole through a temp file and a rename under an in-process lock; one entry per provider id, `{"type":"oauth","access","refresh","expires","accountId"?}` or `{"type":"api","key"}` — opencode's shape, so a login carries over. A missing file reads empty; a corrupt one is an error, never silently emptied.
3. **Tokens are read lazily and refreshed single-flight.** A `TokenSource` per provider instance caches the entry, refreshes 300 s before expiry or on a 401 — once, then the request is retried once — under one lock, so concurrent turns make one refresh. `refresh_token_expired|reused|invalidated` in the issuer's reply is permanent: the entry is removed and `auth()` reads `Expired`. `auth()` is synchronous over the cache, so the kernel's refusal at session open and a `/login` in the same process agree.
4. **Login is an interaction.** `Provider::login(prompter, method: Option<LoginMethod>)` opens `InteractionKind::Login` with the flow's `url` and `code` and `answers: [Cancel]` (`[Text, Cancel]` for `Paste`), and races the flow against the answer: the flow completing drops the ask, `Cancel` aborts the flow. The browser flow binds `127.0.0.1:1455` (up to twenty ports higher on conflict), opens the browser unless `BINGO_NO_BROWSER` is set, checks `state` and exchanges the code with PKCE S256; the device flow polls every `interval` seconds for at most 15 minutes. `Provider::logout()` revokes best-effort and removes the entry.
5. **`/login <provider> [browser|device|paste]` and `/logout <provider>` are kernel built-ins**: the kernel owns providers and is what refuses a session without credentials. They hold the queue; an interaction opened while a holding command runs has `turn: None` and is cancelled `CommandEnded` when the command finishes. `bingo login|logout <provider> [--device|--paste]` run the same two without a session, answered on the terminal. The receipt is `Record{Action{name: "login", args: provider, result}}`.
6. **`codex` is the openai plugin's second provider**: `Variant::Codex`, OAuth against `https://auth.openai.com` with codex's client id, settings key `codex: {baseUrl?, issuer?}` for proxies and tests, models from `GET {base}/codex/models?client_version=…` (`slug`, `visibility != "hide"`, in `priority` order), M2's static list when that fails.
7. SHA-256 and random bytes come from `aws-lc-rs`, already in the tree under rustls; no crate is added.

## Consequences

- sdk touched once: `LoginMethod`; `Provider::login` gains `method`; `Provider::logout`; `CancelReason::CommandEnded`. Plugin touched: `bingo-provider-openai`, the one implementer. The wire gains the cancel reason and nothing else — `Interaction` carried `Login` already.
- `variant::account_id` moves to the library's `jwt` module, the one JWT reader.
- The catalogue's `Models` for `codex` is empty (models.dev has no such provider); the resolver still finds `gpt-5.x` by exact id across providers. Carried: a provider's own list reaching the catalogue.
- opencode's `~/.local/share/opencode/auth.json` is not read: the shape is compatible, the path is ours.
- A person on a machine without a browser uses `device`; a CI job uses `paste` with a token minted elsewhere. Neither is chosen for them.

## Supersedes

ADR-0001's two-tier map gains the library tier; ADR-0008's "`/provider` does not exist" stands — login names the provider, `/model` names the model.
