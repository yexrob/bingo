# M10 — OAuth and the Codex provider

## Goal

`bingo login codex` signs a person into their ChatGPT subscription from a terminal — the browser opens, or a device code is shown — and from then on `bingo --provider codex --model gpt-5.4 "…"` runs a coding turn over the subscription endpoint with no key in any file but `auth.json`. Inside a session `/login codex` does the same through a dialog, and `/logout codex` undoes it. Tokens refresh before they expire and once more on a 401; a dead refresh token reads as `Expired` with the way back. The flows live in a library crate that a second provider can use (ADR-0012).

## Bricks, in build order (owner)

1. **ADR-0012 + sdk** (kernel) — `LoginMethod::{Browser, Device, Paste}`; `Provider::login(&self, prompter, method: Option<LoginMethod>)`; `Provider::logout(&self)` (default `Unsupported`); `CancelReason::CommandEnded`. `scripts/check_discipline.sh` reads `package.metadata.bingo.tier`: a library depends on the sdk only, a plugin on the sdk and libraries. `schema/rpc.json` regenerated.
2. **`bingo-auth-oauth`** (worker A, new library crate) — pure bricks first: `pkce::{verifier, challenge}` (S256 over `aws-lc-rs`), `jwt::{claims, account_id}` (moved from `provider-openai::variant`), `callback::parse(request_head) -> Result<(code, state)>`, `tokens::Tokens::from_response(json, now)` with `is_fresh(now)` at a 300 s lead, `error::permanent(body)`. Then `Issuer` (client id, base, the six paths, scope, extra authorize params), `CredentialStore` (`<data_dir>/auth.json`, entries per ADR-0012 §2), `loopback::Callback` (bind 1455…1475, one request, `state` check, a "you can close this tab" page), `device::{start, poll}`, `browser::open(url)` (best-effort `open`/`xdg-open`/`cmd /c start`, `BINGO_NO_BROWSER` skips), and `TokenSource` (§3): `status()` sync, `access_token()`, `refreshed()` (forced), `login(prompter, method, open_browser)`, `logout()`. Tests: every brick pure; the store's 0600, atomicity, corrupt file; each flow against a wiremock issuer (`usercode` → 403 pending → granted → exchange; loopback callback hit by reqwest with the right and a wrong `state`; refresh rotation; a permanent failure clearing the entry; eight concurrent `access_token()` calls making one refresh; `Cancel` aborting a device poll).
3. **`codex` in `bingo-provider-openai`** (worker A, after 2) — `Credential::{Key(String), Tokens(Arc<TokenSource>)}` replaces `api_key`; `bearer()` async; a 401 with `Tokens` refreshes once and retries once; `auth()`: `Ready` when signed in, `Missing{hint: "Run `bingo login codex`, or `/login codex` in a session."}`, `Expired{hint}`; `login`/`logout` delegate; `models()` for `Variant::Codex` reads `GET {base}/codex/models?client_version=0.146.0` (`models[].slug`, skip `visibility: "hide"`, `priority` ascending; `display_name` as `display`) and falls back to the nine-model static list on any failure; `OpenAiPlugin` registers `openai` and `codex` (`provides: ["provider:openai", "provider:codex"]`), settings claim gains `("codex", Replace)` with `CodexConfig { base_url, issuer }`; `Env.data_dir` names the store. Tests: the header table (bearer from the store, `ChatGPT-Account-Id` from its claim), the 401-refresh-retry against wiremock, the models parse on a recorded body and the fallback, the plugin registering both providers.
4. **Kernel** (kernel) — `commands/{login,logout}.rs`: `/login <provider> [browser|device|paste]` (not instant; `ArgSpec::Catalog{providers}`) finds the provider, runs `provider.login(prompter, method)` with the session's own prompter, receipts `Record{Action{login, provider, "Signed in to codex as …"}}`; `/logout <provider>`. `open_interaction` accepts `turn: None` while a holding command runs; `command_finished` cancels the pendings `CommandEnded`. `Host::provider(id)` and `Host::prompter(session)` become `pub(crate)`/`pub` as the two need.
5. **bin** (kernel) — `bingo login <provider> [--device|--paste]`, `bingo logout <provider>`: build the host, run the provider's method with a `TerminalPrompter` (stderr shows the URL and code; `Paste` reads one line from stdin; `Cancel` on ctrl-c), print the receipt, exit 0/1. The kernel's `AUTH_REQUIRED` refusal names the command through the provider's hint.
6. **TUI** (kernel) — the `Login` dialog offers `Cancel` as a row and Esc; a `Paste` flow opens the words row and sends `Answer::Text`; a `TestBackend` snapshot per flow.
7. **Black-box** (kernel) — `crates/bingo/tests/cli/login.rs` with a wiremock issuer and a wiremock Codex endpoint reached through `--settings`: `bingo login codex --device` completes and `auth.json` is 0600 with an `oauth` entry; `--provider codex` then runs a text turn with `Authorization: Bearer` and `ChatGPT-Account-Id` on the wire; `--provider codex` before any login is one `AUTH_REQUIRED` line naming `bingo login codex`; over RPC `/login codex paste` opens a `Login{Paste}` interaction whose `Text` answer stores the key and acks a receipt; `bingo logout codex` hits `/oauth/revoke` and empties the entry; a 401 mid-turn is followed by one refresh and the turn completes.

## Files

`docs/adr/0012-oauth-credentials.md`, `crates/bingo-sdk/src/{provider,event}.rs`, `schema/rpc.json`, `scripts/check_discipline.sh`, `crates/bingo-auth-oauth/**`, `crates/bingo-provider-openai/src/{lib,variant,models,credential}.rs` + `fixtures/codex_models.json`, `crates/bingo-core/src/commands/{mod,login,logout}.rs`, `crates/bingo-core/src/{host,session}.rs`, `session/{interactions,inputs}.rs`, `crates/bingo/src/{main,login}.rs`, `crates/bingo-surface-tui/src/dialog.rs`, `crates/bingo/tests/cli/login.rs`, `Cargo.toml`, `scripts/budget.toml`, `ARCHITECTURE.md`.

## Dependencies

`aws-lc-rs` (SHA-256, `SystemRandom`) — already resolved under `rustls`; a direct edge, no new crate. One workspace crate: `budget.toml` 267 → 268.

## Exit criteria

- [x] `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `scripts/check_discipline.sh`, `scripts/budget.sh`, `cargo deny check`, `scripts/tui-smoke.sh`
- [x] Library: every brick in 2 has its test; the loopback flow rejects a wrong `state`; the device poll stops at the cap and on `Cancel`; a permanent refresh failure clears the entry and `status()` reads `Expired`; eight concurrent callers refresh once; `auth.json` is 0600 after a write and after a rewrite
- [x] Provider: `codex_request_params_isolation` (M2) still passes; the 401 → refresh → retry path; `models()` dynamic and fallback; both providers registered from one plugin
- [x] Kernel: a holding command may open an interaction with `turn: None`; it is cancelled `CommandEnded` when the command finishes; `/login` on an unknown provider is `PROVIDER_UNAVAILABLE`; `/login` on a provider that takes a key is `INVALID_INPUT` and says `login` (the sdk maps `Unsupported` there)
- [x] Surfaces: the TUI shows the URL and code and Esc cancels; `bingo login codex --device` on a terminal prints them on stderr and nothing on stdout; the RPC `Login{Paste}` round trip
- [x] Black-box: every scenario in 7
- [x] sdk changed once; ADR-0012 lists what it touched; `check_discipline.sh` accepts `provider-openai → auth-oauth` and would reject `auth-oauth → provider-openai`

## Non-goals

A keyring backend. Reading opencode's or codex's own `auth.json`. Anthropic subscription login (the library is ready for it; the issuer is not known). Moving API keys from settings into `auth.json` (`{"type":"api"}` is read if present, never written by this milestone). The catalogue listing a provider's dynamic models. Per-account selection when one issuer holds several.

## Risks touched

R1 sdk churn — one change, three additions, made first. R4 provider quirks — the Codex endpoint has never been exercised live in this project; the fake issuer proves the flows, the user's own subscription proves the endpoint (a live smoke is the last exit criterion and needs the user). Security — the callback binds loopback only, `state` is random per attempt, the verifier never leaves the process, the store is 0600; nothing is logged. `aws-lc-rs` as a direct dependency — if reqwest's TLS backend moves, the edge is a one-line swap to `sha2`.

## Verified (2026-08-30, commit c972901; live subscription smoke confirmed by the user)

```
$ cargo fmt --all -- --check                                        exit 0
$ cargo check --workspace --all-targets --locked                    exit 0
$ cargo clippy --workspace --all-targets --locked -- -D warnings    exit 0
$ cargo test --workspace --locked                                   exit 0 — 1598 passed, 0 failed
  new: bingo-auth-oauth 52 · provider-openai 84 + codex_subscription 5 (responses_api 15 unchanged)
  core +5 (host/tests/login.rs 4, an idle session refuses to ask) · tui +3 snapshots · bin cli +6 · bin login (rpc) 1
$ scripts/check_discipline.sh                                       exit 0 (size warnings: core/session.rs 779, core/host.rs 832,
                                                                    core/host/tests.rs 937, core/turn.rs 775, tests/rpc.rs 793, tui/test_support.rs 743)
  a library importing a plugin, injected by hand:                   exit 1 — "bingo-auth-oauth -> bingo-tool-web (a library depends on bingo-sdk only)"
$ scripts/budget.sh                                                 dependencies 268 (max 268, was 267: one workspace crate; aws-lc-rs added none)
$ cargo deny check                                                  advisories ok, bans ok, licenses ok, sources ok
$ scripts/tui-smoke.sh                                              tui-smoke ok
$ ~/.claude/jobs/8d3a7fd6/tmp/live-m10.sh (tmux, the real binary)   /login codex paste → the dialog, the words row, `login codex ⎿ Signed in to codex.`
                                                                    in the transcript, auth.json 0600 {"type":"api"}; /login codex device against a dead
                                                                    issuer → "transport: error sending request for url (…/deviceauth/usercode)";
                                                                    /logout codex → `Signed out of codex.`, the entry gone
```

Exit criteria, item by item: the library's bricks are each tested alone (RFC 7636 B vector, the JWT claim order, the callback parser, `is_fresh` at the 300 s lead, `permanent`, the authorize URL literal) and each flow end to end against wiremock (device with a pending poll, browser with a real loopback socket hit by reqwest — right state 200, wrong state 400, refresh rotation, a permanent failure clearing the entry and `status()` reading `Expired`, eight concurrent callers → `expect(1)` refresh, `Cancel` stopping a poll, `Paste` storing an `Api` entry, 0600 after write and rewrite, a corrupt file an error). The provider: M2's isolation test unchanged, the 401 → refresh → retry against wiremock, the dynamic catalogue on the fixture and the fallback on a 404, one plugin registering `openai` and `codex`, `login` on `openai` `Unsupported`. The kernel: the four host tests in `host/tests/login.rs` plus the idle-session refusal. Surfaces: three `TestBackend` snapshots, the tmux drive above, `bingo login codex --device` with the code and the address on stderr and one line on stdout, the RPC `Login{Paste}` round trip followed by `/model codex/gpt-5.4` in the same process. Black-box: every scenario in brick 7. The sdk changed once (`aec4a72`); ADR-0012 lists what it touched.

Found while integrating:

- The TUI dialog's 400 ms keystroke guard dropped a `1` my tmux script sent the instant the dialog appeared — the guard working as ADR-0002 meant it; a scripted drive waits half a second before answering a dialog.
- `pkce::verifier()`/`state()` are fallible: `aws_lc_rs` `SecureRandom::fill` can fail and `expect` is a lint error; the failure is `AuthError::Invalid`, never a weaker random.
- Percent-encoding and the form body are four hand-written lines (`percent.rs`): `url` is not a dependency of the library and reqwest's `form` feature would have pulled `serde_urlencoded` against the budget.
- `Credential::Key(Option<String>)`, so `status()` can say `Missing` for an unconfigured key with the provider's own hint; `Credential::status` takes the hint as a closure because only the provider knows the settings file to name.
- The 401 retry keys on `ProviderError::Auth`, which `classify` produces for 401 and 403 alike; both mean the credential.
- `Tokens` and `TokenSource` have redacting `Debug` impls, with a test that no secret appears; `OpenAiProvider`'s derived `Debug` prints them.
- The dropdown's argument completion was hard-wired to `models`; it now reads any catalogue a command's `ArgSpec::Catalog{source}` names, so `/login <tab>` lists providers.
- Three crates now carry their own `Prompter` test double (`Signing` in core, `Person` in auth-oauth, `NoPrompter` in provider-openai).

Open, carried forward:

- [x] **Live smoke with a ChatGPT subscription** — the user ran `bingo login codex` and a `--provider codex` turn on their own account and reported it working (2026-08-30; output not pasted here). Recorded 2026-08-31 (`gpt-5.6-luna`, headless): a plain turn, a plugin tool round trip and an experience commit + fresh-session recall, output pasted in `M14-experience-bridge.md` §Live — token use and streaming are no longer just reported. Still only a documented shape rather than a recording: `fixtures/codex_models.json`; record a live `/codex/models` body when one is at hand. The refresh body stays JSON until a live refresh says otherwise.
- A `Prompter` double in `bingo_sdk::testing` (`ScriptedPrompter`), replacing the three local ones.
- The catalogue's `Models` for a provider whose list is dynamic (`/model codex/<tab>` completes nothing; typing the id works).
- Anthropic subscription login: the library is ready, the issuer is not known.
- API keys into `auth.json`: `{"type":"api"}` is read and written by `paste`; nothing moves a settings key there.
- `browser::open` runs only on macOS here; the Linux and Windows arms are compile-checked.
- `core/host/tests.rs` is 937 lines against the 1000 fail: split by subject at the next kernel milestone.
