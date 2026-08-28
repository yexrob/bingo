# Rust crate landscape, verified 2026-08-29

> Source: subagent report, archived verbatim. Facts were verified on the date in the title; re-verify before depending on a version.

All lookups done. Here is the report.

---

# bingo — Rust dependency research (verified 2026-08-29)

All crates.io figures pulled live from `crates.io/api/v1` on **2026-08-29**; GitHub figures from the GitHub API the same day. `dl/90d` = crates.io `recent_downloads`.

## Summary table

| # | Subsystem | Recommended | Version (date) | Maturity | Port note |
|---|---|---|---|---|---|
| 1 | LLM providers | **none — hand-roll 2 adapters** over `reqwest`+`serde` | — | — | Port **AI SDK `LanguageModelV4`** type shape; port **pi-ai** OAuth + model catalog |
| 2 | SSE | `eventsource-stream` **0.2.3** (2022-02-17) *or* `sse-stream` **0.2.5** (2026-07-22) | | A− / B+ | — |
| 2 | HTTP | `reqwest` **0.13.4** (2026-05-25) + `rustls` 0.23 | | A | — |
| 2 | Retry | `backon` **1.6.0** (2025-10-18); or `reqwest-middleware` 0.5.2 + `reqwest-retry` 0.9.1 | | A | — |
| 3 | JSON-RPC 2.0 | **hand-roll** on `serde_json` + `tokio_util::codec::LinesCodec`; WS via `axum::extract::ws`; reuse `agent-client-protocol`'s rpc for ACP | — | — | — |
| 4 | OAuth PKCE | `oauth2` **5.0.0** `default-features=false` + own loopback (`axum` or `tiny_http` 0.12) | | A− | Port **pi-ai `auth/oauth/*`** (anthropic, openai-codex, device-code, pkce) |
| 4 | Secrets | `keyring` **4.1.6** (2026-08-01) + `secrecy` **0.10.3** | | A | — |
| 5 | Config | `config` **0.15.25** (2026-06-26) or hand-rolled merge; **not `figment`** | | A / — | — |
| 5 | JSONC/JSON5 | `jsonc-parser` **0.33.1** (2026-07-26), `json5` **1.3.1** | | A− | — |
| 5 | Schema / TS | `schemars` **1.2.2** (2026-07-27), `ts-rs` **12.0.1** (2026-01-31) | | A | — |
| 6 | Journal | JSONL by hand (`tokio::fs` + `serde_json`) | — | — | — |
| 6 | Index/DB | `rusqlite` **0.40.2** `bundled` (+FTS5) | | A | — |
| 6 | Lock / ids / hash / dirs | `fs4` **1.1.0**, `uuid` **1.26.0** (v7), `blake3` **1.8.7** + `sha2` **0.11.0**, `etcetera` **0.11.0** | | A | — |
| 7 | BM25 recall | `bm25` **2.3.2** (2025-09-07) — *codex uses this* | | B+ | — |
| 8 | Shell parsing | `shlex` **2.0.1** + `tree-sitter` 0.25/0.26 + `tree-sitter-bash` **0.25.1** | | A | Port **Claude Code permission-rule semantics** (no OSS source); reference `codex-execpolicy` |
| 9 | Process / PTY | `tokio::process` + `process-wrap` **10.0.0** (2026-08-24); `portable-pty` **0.9.0** | | A / B+ | — |
| 9 | Sandbox | `landlock` **0.4.7** (2026-07-27) + `seccompiler` **0.5.0**; macOS `sandbox-exec`; **not `birdcage` (archived)** | | A− | Port `codex-linux-sandbox` / `codex-windows-sandbox` approach |
| 10 | Diff | `similar` **3.2.0** (2026-08-17); `diffy` **0.5.1** for unified patch apply | | A | Port `codex-apply-patch` V4A format |
| 11 | File search | `ignore` **0.4.33** + `globset` **0.4.20** + `grep-searcher` **0.1.17**/`grep-regex` 0.1.14 | | A | — |
| 12 | HTML→MD | `htmd` **0.5.5** (2026-07-27) or `html-to-markdown-rs` **3.11.4** (2026-08-22) | | B+ | Port **Mozilla Readability** (no good Rust port) |
| 12 | Readability | `dom_smoothie` **0.18.0** (2026-06-07) | | B | — |
| 12 | Web search | **hand-roll** Brave/Tavily/Exa clients | — | — | no mature Rust client exists |
| 13 | Skills | hand-rolled `---` frontmatter split + YAML; `minijinja` **2.24.0** if real templating | | A | — |
| 13 | YAML | **`serde-saphyr` 1.1.0** (2026-08-15) or `serde_norway` 0.9.42 — **never `serde_yml`** | | B+ / B | — |
| 14 | Tokenizers | `tiktoken-rs` **0.12.0** (2026-06-02) for OpenAI; Anthropic → `/v1/messages/count_tokens` API | | A− | — |
| 15 | Plugin registration | `inventory` **0.3.24** (in-process, *codex uses it*); `linkme` 0.3.37 alt | | A | — |
| 16 | Observability | `tracing` 0.1.44 + `tracing-subscriber` 0.3.23 + `tracing-appender` 0.2.5; OTel **0.32** + `tracing-opentelemetry` 0.33 | | A | — |
| 17 | CLI / release | `clap` **4.6.6**; `dist` (crate `cargo-dist` **0.32.0**); `cargo-deny` 0.20.2, `cargo-nextest` 0.9.143 | | A | — |
| 18 | Testing | `insta` 1.48.0, `wiremock` 0.6.5, `assert_cmd` 2.2.2 + `predicates` 3.1.4, `tempfile` 3.27.0, `proptest` 1.11.0, `rstest` 0.26.1 | | A | — |
| 19 | Async | `tokio-util` **0.7.19** `CancellationToken`; `async-trait` **0.1.92** for dyn traits; `futures` 0.3.34; `async-stream` 0.3.6 | | A | — |
| 20 | IM | `teloxide` 0.17.0, `slack-morphism` 2.25.0, `serenity` 0.12.5, `matrix-sdk` 0.18.0; Lark → **`openlark` 0.20.0** (not `open-lark`) | | A/B/C | Lark: port from official Go/Python OAPI SDK |
| 21 | Misc | `regex` 1.13.1, `unicode-width` 0.2.2, `unicode-segmentation` 1.13.3, **`jiff` 0.2.35**, `thiserror` 2.0.20 + `anyhow` 1.0.104, `semver` 1.0.28, `base64` 0.23.1 | | A | — |

---

## Reference implementations (the strongest signal in this report)

I read the live workspace manifests of both comparable agents. These are facts, not inference.

**openai/codex** — 119,499★, pushed 2026-08-28. `codex-rs/Cargo.toml`:
- `reqwest = "0.12"` (**not** 0.13) + `rustls` 0.23 `aws_lc_rs` + `rustls-native-certs` 0.8.3
- `eventsource-stream = "0.2.3"` — SSE
- **`bm25 = "2.3.2"`** in `codex-core` — this is the memory-recall crate
- `similar = "2.7.0"` **and** `diffy = "0.4.2"` (both)
- `sqlx = "0.9.0"` `sqlite-bundled` in `codex-thread-store` (+ `zstd`), *plus* a separate JSONL `codex-rollout` crate — **journal is JSONL, index is SQLite**
- `schemars = "0.8.22"` (still 0.8), `ts-rs = "11"`
- `inventory = "0.3.19"` — plugin registration
- `keyring = "3.6"` with per-OS features (`apple-native`, `windows-native`, `linux-native-async-persistent`, `sync-secret-service`, `crypto-rust`)
- **`codex-login` = `tiny_http` + `sha2` + `rand` + `base64`** — PKCE hand-rolled, **no `oauth2` crate**
- `codex-shell-command` = `tree-sitter` + `tree-sitter-bash` + `tree-sitter-powershell` + `shlex` + `which`
- `codex-execpolicy` = `starlark` 0.14.2 + `shlex` + `multimap`
- `codex-file-search` = `ignore` + `nucleo` (git rev) + `crossbeam-channel`
- `codex-skills` = `serde_yaml` + `shlex` + `include_dir` — **no `gray_matter`, no `minijinja`**; `codex-utils-template` has **zero dependencies**
- `codex-hooks` = `tokio` process + `regex` + `schemars` + `async-channel`
- WebSocket: forked `tokio-tungstenite` 0.28 + `tungstenite` 0.27 (`openai-oss-forks`); `crossterm` also forked
- No `jsonrpsee` — `codex-app-server-protocol` + `codex-app-server-transport` are hand-rolled
- No genai/rig — `codex-model-provider` is hand-rolled
- `uuid` with `v4,v5,v7`; `toml` 0.9 + `toml_edit` 0.24; `dunce`, `arc-swap`, `indexmap`, `regex-lite`, `lru` 0.18, `which` 8, `notify` 8.2, `gix` 0.81, `jsonwebtoken` 9.3.1

**block/goose → now `aaif-goose/goose`** — 53,612★, pushed 2026-08-28, v1.48.0, `rust-version = 1.94.1`, Apache-2.0. Note the **org rename** (`block/goose` redirects):
- `rmcp = "3.0.0"`; `agent-client-protocol = "2.0.0"` + **`agent-client-protocol-http = "2.0.0"`** + `agent-client-protocol-schema = "=1.5.0"`, all `[patch]`ed to a git rev
- `reqwest = "0.13.2"` + `rustls` 0.23.31 `aws_lc_rs`
- `schemars = "1.0.2"`, `sha2 = "0.11"`, `base64 = "0.23"`, `etcetera = "0.11"`, `keyring = "3.6.3"` (vendored), `fs2 = "0.4"` for locking
- `serde_yaml = "0.9.32"` (still on the deprecated crate)
- OTel **0.32** stack + `tracing-opentelemetry` 0.33; `axum` 0.8 + `axum-server` 0.8 + `tower-http` 0.7
- `tree-sitter = "0.26"`; `wiremock` 0.6; also no LLM-abstraction crate

**Divergences to decide:** codex is on reqwest 0.12/schemars 0.8; goose is on reqwest 0.13/schemars 1.0. `rmcp` 3.1.4 requires `reqwest ^0.13.2` + `schemars ^1.0` + `oauth2 ^5.0`. **Follow goose: reqwest 0.13 + schemars 1.x.** That is the tree `rmcp` and `agent-client-protocol` already want.

---

## 1. LLM multi-provider abstraction

| Crate | Version (date) | dl total / 90d | ★ / last push | License | Verdict |
|---|---|---|---|---|---|
| `genai` | 0.6.5 stable (2026-06-06); 0.7.0-beta.19 (2026-08-18) | 333k / 119k | 866 / 2026-08-18 | MIT OR Apache-2.0 | Best-in-class *for its category*, but 0.x |
| `rig-core` | 0.42.0 (2026-08-17) | 2.42M / 1.41M | 8,439 / 2026-08-27 | MIT | Agent framework (RAG, vector stores), wrong altitude |
| `llm` | 1.3.8 (2026-04-19) | 116k / 30k | 363 / 2026-06-06 | MIT | Thin, low adoption |
| `anthropic-ai-sdk` | 0.2.27 (2026-01-11) | 44k / 3.9k | 18★ / 2026-01-11 | MIT | Single-author, 7 months idle |
| `misanthropy` | 0.0.8 (2025-06-08) | 12.7k | 34★ / 2025-06-12 | MIT | Dead |
| `clust` | 0.9.0 (2024-06-30) | 14.6k | 42★ / 2025-03-02 | MIT/Apache | Dead |
| `anthropic` | 0.0.8 (2024-09-03) | 29.6k | — | MIT | Dead |
| `async-openai` | 0.41.3 (2026-07-31) | 7.62M / 2.39M | 1,997★ / 2026-08-18 | MIT | Already verified; **reqwest ^0.13** ✓ |

`genai` 0.7-beta genuinely covers a lot (verified from its CHANGELOG): Anthropic `Tool::with_cache_control`, request-level `ChatOptions::with_cache_control` auto-breakpointing the tools+system prefix, `ChatStreamEvent::Heartbeat` for Anthropic SSE pings, incremental `ToolCallChunk`, normalized `cache_write_tokens`, `extra_body` passthrough, `AuthResolver` + `ServiceTargetResolver` for custom auth/base-URL, optional `otel` feature.

**RECOMMENDATION: do not depend on any of them. Write two adapters behind your own `Provider` trait.**

Reasons, in order of weight:
1. Both reference implementations (codex, goose) — and Claude Code — hand-roll. Two independent teams at that scale chose the same thing.
2. Your `Provider` trait *is* the abstraction. Adding genai means two normalization layers stacked; the outer one leaks the inner one's model.
3. A coding agent needs byte-exact fidelity a normalizer will eventually lose: Anthropic `thinking` block **signatures** replayed verbatim across turns, `cache_control` breakpoints at exact positions, `stop_reason` discrimination, `server_tool_use`/`web_search_tool_result` blocks, beta headers (`context-1m`, fine-grained tool streaming), and OAuth bearer + `anthropic-beta: oauth-*` for Pro/Max.
4. genai is 0.x with breaking changes in nearly every release (`Error::HttpError` gained a field, `Tool` gained a public field, `ReasoningEffort::None`→`Zero`, all in 0.7-beta). You'd absorb that churn in your kernel.
5. `async-openai` 0.41.3 is the *one* worth considering — but only for the Responses API, and it pulls its own `Config`/`Client` model. A thin adapter is ~600 lines and you control retry/streaming.

Budget: ~800–1200 LOC for Anthropic Messages + ~800 for OpenAI Responses, plus a shared SSE/retry/error module.

**Best normalization port reference — ranked:**
1. **Vercel `@ai-sdk/provider` v4.0.8** (Apache-2.0, `vercel/ai` 26,469★, pushed 2026-08-28). The spec now lives at `packages/provider/src/language-model/{v2,v3,v4}` — **v4 is current**. Its file split *is* the trait design you want: `language-model-v4-content.ts`, `-stream-part.ts`, `-finish-reason.ts`, `-usage.ts`, `-tool-call.ts`, `-tool-result.ts`, `-tool-approval-request.ts`, `-reasoning.ts`, `-provider-tool.ts`, `-call-options.ts`, `-prompt.ts`, `-response-metadata.ts`. Port the *type algebra*, not the code.
2. **pi-ai** (`earendil-works/pi`, 98,696★, MIT, pushed 2026-08-28, npm `@earendil-works/pi-ai` 0.84.3). Port `packages/ai/src/auth/oauth/{anthropic,openai-codex,github-copilot,device-code,pkce,oauth-page,load}.ts` and `model-catalog.ts` / `models.generated.ts`. This is the only OSS source for the Anthropic Pro/Max and Codex OAuth flows.
3. **litellm** 1.98.0 (MIT). Only useful as a lookup table for provider error-code → canonical-error and param-name mappings. Don't port structure — it's Python-dynamic and enormous.

- https://crates.io/crates/genai · https://github.com/jeremychone/rust-genai
- https://github.com/vercel/ai/tree/main/packages/provider/src/language-model/v4
- https://github.com/earendil-works/pi/tree/main/packages/ai/src/auth/oauth

## 2. SSE + HTTP + retry

**`reqwest` 0.13.4** (2026-05-25, 673M dl, MIT/Apache, MSRV 1.85). Breaking changes in 0.13.0, verified from CHANGELOG:
- `rustls` is now the **default** TLS backend (was `native-tls`); crypto provider defaults to **aws-lc** (was ring)
- feature `rustls-tls` renamed to `rustls`; rustls roots features removed — **`rustls-platform-verifier` used by default**
- **`query` and `form` are now optional features, disabled by default** ← easy to miss
- `native-tls` now includes ALPN

**SSE — the compatibility trap:**
- `reqwest-eventsource` 0.6.0 (2024-03-29) depends on **`reqwest ^0.12`**. On reqwest 0.13 this duplicates the whole HTTP stack. **Reject.**
- **`eventsource-stream` 0.2.3** — deps are only `futures-core`, `nom ^7.1`, `pin-project-lite`. HTTP-client agnostic, so version-proof. 19.9M dl / 7.86M 90d. Repo last push 2024-08-15, 39★ — but it's ~300 lines and codex ships it.
- **`sse-stream` 0.2.5** (2026-07-22, 15.9M dl / 7.98M 90d, Apache-2.0, `4t145/sse-stream`) — deps only `bytes` + `pin-project-lite` (no `nom`). Actively maintained, and it's what **`rmcp` already pulls in**, so it's in your tree regardless.

**RECOMMENDATION:** `reqwest` 0.13 (rustls default) + **`sse-stream` 0.2.5** (zero extra tree — rmcp brings it) with `eventsource-stream` 0.2.3 as the drop-in fallback. Retry: **`backon` 1.6.0** (75.5M dl, Apache-2.0) — it's a combinator over your own future, no middleware layer, works with streaming bodies. Use `reqwest-middleware` 0.5.2 + `reqwest-retry` 0.9.1 (both `reqwest ^0.13.1` ✓) **only** if you also want tracing/auth middleware; note that retrying an SSE stream mid-flight needs your own resume logic either way.

## 3. JSON-RPC 2.0 over NDJSON stdio + WebSocket

- **`jsonrpsee` 0.26.0** (2025-08-11; the 0.24.11 published 2026-05-27 is a backport on the old line). Repo active (853★, pushed 2026-08-22) but **no 0.27 in a year**. Verified feature list: `http-client`, `ws-client`, `wasm-client`, `server`, `client-core`, `server-core` — **no stdio transport**. You'd implement `TransportSenderT`/`TransportReceiverT` yourself, and the server half is hyper-bound. Substrate lineage, heavy.
- `jsonrpc-core` 18.0.0 — **2021-07-20**. Dead.
- `async-jsonrpc-client` 0.3.0 — **2021-02-24**, 28.9k dl. Dead.

**RECOMMENDATION: hand-roll.** JSON-RPC 2.0 is ~400 lines: a `Request`/`Response`/`Notification`/`Error` enum in `serde_json`, an id→oneshot map, and a `tokio_util::codec::FramedRead<_, LinesCodec>` for NDJSON stdio. WebSocket: **`axum::extract::ws`** if you're already serving HTTP (axum 0.8.9 pins `tokio-tungstenite ^0.29`), else `tokio-tungstenite` 0.30.0 directly — **but not both**, or you get two tungstenite versions.

Better: `agent-client-protocol` 2.0.0 **already implements JSON-RPC in-crate** with Stdio/ByteStreams/Lines/Channel transports and is runtime-agnostic. Reuse its rpc/connection machinery for the ACP surface, and if its transport traits are public enough, for the native surface too. Both codex (`codex-app-server-protocol` + `-transport`) and ACP took this route. Also note **`agent-client-protocol-http` 2.0.0** exists (2026-07-23, Apache-2.0, 98k dl) — goose depends on it — if you want ACP over HTTP rather than stdio.

## 4. OAuth PKCE + secrets

- **`oauth2` 5.0.0** (2025-01-21, 47.2M dl / 11.5M, MIT/Apache, `ramosbugs/oauth2-rs` 1,203★, pushed 2026-02-22). **Gotcha: default features are `["reqwest", "rustls-tls"]` and it pins `reqwest ^0.12`.** Use `oauth2 = { version = "5.0", default-features = false }` and implement `AsyncHttpClient` over your reqwest 0.13 client. `rmcp` 3.1.4 already does exactly this (`default_features=false`, `features=[]`), so alignment is free.
- `openidconnect` 4.0.1 (2025-07-06, 12.3M dl). Only if you need real OIDC discovery/ID-token validation. Anthropic/Codex flows don't.
- **`keyring` 4.1.6** (2026-08-01, 22.3M dl / 9.16M, MIT/Apache, MSRV 1.88). v4 restructured: API moved to **`keyring-core` 1.0.0** + separate per-store crates; the `keyring` crate's **`v1` default feature** gives the old v1 API over native stores on macOS/Windows/*nix. Feature list collapsed from 12 (`apple-native`, `windows-native`, `sync-secret-service`, `crypto-rust`, …) to just **`cli`/`default`/`v1`** — much simpler than codex's 3.6 setup. Repo is under the new `open-source-cooperative` org, 764★, pushed 2026-08-25.
- **`secrecy` 0.10.3** (2024-10-09, 151M dl). Stable by design, no churn expected.

**RECOMMENDATION:** `oauth2` 5.0 (`default-features=false`) for the state machine + PKCE, your own loopback listener (**`axum`** if it's already in the tree, else `tiny_http` 0.12 like codex — 60.7M dl but last released 2022-10-06), `keyring` 4.1.6 + `secrecy` 0.10.3 for storage. Device flow: `oauth2` has `DeviceAuthorizationUrl` support; port pi-ai `device-code.ts` for the polling/backoff nuances.

**Port note:** the Anthropic Pro/Max and Codex OAuth flows are undocumented and exist in no Rust crate. `pi-ai`'s `auth/oauth/anthropic.ts` and `openai-codex.ts` are the reference.

## 5. Layered config, JSONC/JSON5, schema, TS types

- **`figment` 0.10.19 — published 2024-05-17, repo last push 2024-09-13.** Two years idle. 909★. **Do not adopt for a new project.**
- **`config` 0.15.25** (2026-06-26, 109M dl / 18.0M, `rust-cli/config-rs` 3,205★, pushed 2026-08-27). Actively maintained, layered `Source` model.
- `jsonc-parser` **0.33.1** (2026-07-26, 9.67M dl, MIT, dprint) — comment-preserving JSONC, the right choice if you want Claude-Code-style `settings.json` with comments and want to *write back*.
- `json5` **1.3.1** (2026-02-07, 74.7M dl) — serde-native JSON5, read-only.
- `serde_jsonc` 1.0.108 (2023-10-30, 99k dl) — a stale fork of serde_json. Reject.
- **`schemars` 1.2.2** (2026-07-27, 424M dl / 151M, MIT). **Use 1.x, not 0.8.** `rmcp` 3.1.4 requires `schemars ^1.0`; goose is on 1.0.2. Codex's 0.8.22 is legacy.
- **`ts-rs` 12.0.1** (2026-01-31, 13.3M dl, 1,865★, pushed 2026-08-11) — derive-based, zero runtime, codex uses v11.
- `specta` — **`2.0.0-rc.25`, max stable is 1.0.5 from 2023-07-17**. Perpetual RC. Only worth it if you're in the Tauri ecosystem.

**RECOMMENDATION:** hand-roll the layer merge (managed → user → project → local → env → flags) over `serde_json::Value` — it's ~150 lines and you need exact Claude-Code precedence semantics that no crate gives you. Use `jsonc-parser` for reading (comments survive) + `serde_json` for the typed layer, `schemars` 1.2 for `--print-schema`, `ts-rs` 12 for the TS surface types.

## 6. Storage

| Option | Version (date) | dl 90d | Note |
|---|---|---|---|
| `rusqlite` | 0.40.2 (2026-08-08) | 31.2M | `bundled` feature; FTS5 available; sync API |
| `sqlx` | 0.9.0 (2026-05-21) | 34.0M | codex uses it (`sqlite-bundled`); async, MSRV **1.94** |
| `redb` | 4.2.0 (2026-08-17) | 4.40M | 4,755★, pure-Rust embedded KV, MSRV 1.90 |
| `fjall` | 3.1.9 (2026-08-15) | 447k | 2,293★, LSM, MSRV 1.90 |

**RECOMMENDATION — copy codex's split:** **append-only JSONL as the source of truth** (`tokio::fs` + `serde_json`, one event per line, `fsync` policy yours), **plus `rusqlite` 0.40.2 `bundled` as a derived index** (session list, resume, FTS5 recall). This satisfies 守一 — the journal is the only authority; SQLite is a rebuildable projection you can delete. `redb`/`fjall` add a second storage format with no query layer; skip. `sqlx` over `rusqlite` only if you want compile-time-checked async queries and can take MSRV 1.94.

- Locking: **`fs4` 1.1.0** (2026-04-28, 62.0M dl, features `sync`/`tokio`/`async-std`/`smol`). Prefer over `fd-lock` 4.0.4 (2025-03-10, no async) and `fs2` 0.4 (what goose uses; long unmaintained).
- IDs: **`uuid` 1.26.0 v7** over `ulid` 3.0.0. Both are sortable; uuid is already in every tree and codex enables `v4,v5,v7`. `ulid` 3.0.0 (2026-07-16, 31.6M dl) is fine if you want the 26-char Crockford text form.
- Hashing: **`blake3` 1.8.7** (2026-08-20, 171M dl) for content addressing of blobs — much faster; `sha2` 0.11.0 (2026-03-25) only where an interop spec demands SHA-256 (e.g. PKCE `S256`).
- Dirs: **`etcetera` 0.11.0** (2025-10-28, 99.6M dl / 30.2M) — goose uses it, and it lets you pick XDG-vs-native strategy explicitly. `directories` 6.0.0 (2025-01-12) is fine too; codex/goose also carry `dirs` 6.

## 7. BM25 / full-text

- **`bm25` 2.3.2** (2025-09-07, 3.90M dl / 2.06M, MIT, `Michael-JB/bm25` 68★, pushed 2026-08-27). API: `Embedder` (sparse vectors), `Scorer`, `SearchEngine` with `upsert()`/`remove()`. `DefaultTokenizer` does unicode normalization, stopwords, stemming; EN + DE built in, optional `language_detection`. Features: `parallelism` (rayon), `stemming`, `stop_words`. **In-memory only, no persistence.** Caveat from its own docs: mutating the corpus shifts avgdl, degrading scores — plan periodic reindex.
- `tantivy` 0.26.1 (2026-04-21, 17.3M dl, 16,002★). Real Lucene-class engine with mmap segments and persistence. Overkill for session memory; it's a second storage system to operate.
- SQLite **FTS5** via `rusqlite` — free if you already have the index DB; BM25 ranking built in (`bm25()` function); persists automatically.
- `probly-search` 2.0.1 (2024-07-03, 57k dl). Stale, tiny. Reject.

**RECOMMENDATION:** **`bm25` 2.3.2** for in-process memory recall — the decisive evidence is that `codex-core` depends on exactly `bm25 = "2.3.2"`. Use **FTS5** for the on-disk session/transcript search where you already have SQLite. Only reach for `tantivy` if corpus size crosses ~100 MB.

## 8. Shell parsing for permission rules

| Crate | Version (date) | dl 90d | License | Fit |
|---|---|---|---|---|
| `shlex` | 2.0.1 (2026-05-17) | 224M | MIT/Apache | POSIX word splitting; **what codex uses** |
| `shell-words` | 1.1.1 (2025-12-10) | 35.6M | MIT/Apache | Same job; goose uses it |
| `tree-sitter-bash` | 0.25.1 (2025-12-02) | 5.59M | MIT | Real AST: pipelines, `&&`/`;`, subshells, redirects |
| `brush-parser` | 0.4.0 (2026-05-03) | 365k | MIT | Full POSIX/bash parser, 2,188★, MSRV 1.88 |
| `yash-syntax` | 0.24.0 (2026-07-31) | 13.2k | **GPL-3.0-or-later** | ⚠️ copyleft — reject |
| `conch-parser` | 0.1.1 (**2019-05-15**) | 1.5k | MIT/Apache | Dead |

**RECOMMENDATION — two layers, exactly as codex does it:**
1. **`tree-sitter` + `tree-sitter-bash` 0.25.1** to *decompose* a command string into its constituent simple-commands across `&&`, `||`, `;`, `|`, `$(...)`, and to detect redirections/expansions that make a rule undecidable. This is the security-critical layer: a rule like `Bash(git status:*)` must not be satisfied by `git status && rm -rf /`.
2. **`shlex` 2.0.1** to word-split each leaf command for prefix matching.

`brush-parser` gives a richer AST and is genuinely maintained, but tree-sitter-bash is what codex ships in `codex-shell-command` alongside `tree-sitter-powershell` — proven at scale, and error-tolerant by design (important: you must fail *closed* on unparseable input, and tree-sitter gives you explicit ERROR nodes to detect that).

**Port note:** Claude Code's permission-rule semantics (`Tool(specifier)`, `:*` prefix matching, deny-over-allow precedence, the command-splitting rules) have no OSS reference implementation. `codex-execpolicy` (`starlark` 0.14.2 + `shlex` + `multimap`) is a different, more programmable design worth studying but not copying wholesale — Starlark in the permission path is a large attack surface.

## 9. Process management + sandbox

- **`tokio::process`** for the base. Add **`process-wrap` 10.0.0** (2026-08-24, 12.0M dl / 5.24M, Apache/MIT, MSRV 1.87) — process groups, job objects on Windows, kill-on-drop, composable wrappers. It **supersedes `command-group` 5.0.1** (2023-11-18, same author, watchexec) — use process-wrap for new code.
- `nix` 0.31.3 (2026-05-11, 754M dl) for signals/pgid where you need raw syscalls.
- **`portable-pty` 0.9.0** (2025-02-11, 13.0M dl, wezterm) — codex uses it; last release 18 months ago but wezterm is alive.
- **Sandbox — Linux:** `landlock` **0.4.7** (2026-07-27, MIT/Apache) ⚠️ *this supersedes the 0.4.4 in your already-verified list* + `seccompiler` 0.5.0 (2025-03-07, Apache/BSD, rust-vmm). Both are what codex ships.
- **`birdcage` 0.8.1 — repo `phylum-dev/birdcage` is ARCHIVED (last push 2026-07-06), last release 2024-04-19, GPL-3.0-or-later.** ⚠️ **Reject on both counts.**
- **macOS:** no crate. Use `sandbox-exec` with a generated SBPL profile (deprecated by Apple but functional), or Seatbelt via `libc`. Codex has `codex-sandboxing`; there is no published crate.
- **Windows:** codex has `codex-windows-sandbox` (job objects + restricted tokens); nothing on crates.io.

**What goose does:** goose's workspace manifest has **no sandbox crate at all** — no landlock, no seccompiler, no birdcage. Its isolation story is elsewhere (container/permission-prompt based). So codex is your only Rust reference for in-process sandboxing.

## 10. Diff / patch

- **`similar` 3.2.0** (2026-08-17, 185M dl / 47.2M, Apache-2.0, MSRV 1.85, 1,314★). **3.0 was a breaking release**: Rust 2024 edition, `old_slices`/`new_slices` removed in favor of `old_len`/`new_len` + slice iterators, `iter_changes` now panics on invalid ranges, `get_diff_ratio`→`diff_ratio`. New in 3.x: **`Algorithm::Histogram`** (git-style) and `Algorithm::Hunt`, `InlineChangeOptions`/`InlineChangeMode`, `CachedLookup`, owned inputs, **`TextMerge`** (3-way merge), `WhitespaceMode`, `no_std + alloc`.
- `imara-diff` 0.2.0 (2025-06-14, 29.4M dl, 224★) — fastest Myers/Histogram, used by gitoxide. Lower-level, no rendering.
- **`diffy` 0.5.1** (2026-07-19, 14.6M dl, MIT/Apache) — unified-diff **parse + apply + 3-way merge**. This is the patch-application half.

**RECOMMENDATION:** **`similar` 3.2** for computing and rendering diffs (use `Algorithm::Histogram` — it produces the hunk boundaries humans and git expect) + **`diffy` 0.5.1** for applying unified patches. Codex ships both (`similar` 2.7.0, `diffy` 0.4.2); take the newer majors.

**Apply-patch formats:** for an `Edit` tool, exact-string-replace (Claude Code's model) needs no diff library at all — only `str::find` + uniqueness check. Add unified-diff apply via `diffy` for a `Patch` tool. Codex's V4A format (`*** Begin Patch` / `*** Update File:` / `@@` context) lives in `codex-rs/apply-patch` — worth porting if you want fuzzy context matching, since it tolerates line-number drift.

## 11. File search

All BurntSushi, `Unlicense OR MIT`, all released 2026:
- **`ignore` 0.4.33** (2026-08-04, 164M dl / 35.8M) — parallel walk with `.gitignore`/`.ignore`/global-excludes semantics. MSRV 1.88.
- **`globset` 0.4.20** (2026-08-04, 225M dl) — compiles many globs into one automaton. This is your `Glob` tool.
- **`grep-searcher` 0.1.17** (2026-07-15) + **`grep-regex` 0.1.14** (2025-10-16) + `grep-matcher` 0.1.9 — line-oriented search with mmap, multiline, context lines, binary detection. This is your `Grep` tool.
- `walkdir` 2.5.0 (2024-03-01, 588M dl) — only for the simple non-gitignore case; `ignore` supersedes it.

**RECOMMENDATION:** `ignore` + `globset` + `grep-searcher`/`grep-regex`. Do **not** shell out to `rg` — you lose cancellation, structured results, and cross-platform install guarantees. Codex pairs `ignore` with **`nucleo`** (fuzzy match, MPL-2.0, crates.io 0.5.0 from 2024-04-02 — codex pins a git rev instead) for the file picker; `nucleo-matcher` 0.3.1 is the library half. Note **MPL-2.0** is file-level copyleft — fine for linking, but flag it in `cargo-deny`.

## 12. HTML→markdown / readability / web search

- **`htmd` 0.5.5** (2026-07-27, 3.37M dl / 2.62M, **Apache-2.0**, `letmutex/htmd` 452★, pushed 2026-08-17). turndown.js-inspired; `html5ever ^0.38` + `markup5ever_rcdom`; `HtmlToMarkdownBuilder`, custom `element_handler`s. Conversion only — **no content extraction**.
- **`html-to-markdown-rs` 3.11.4** (2026-08-22, 1.11M dl / 670k, **MIT**, `xberg-io/html-to-markdown` 859★, pushed 2026-08-25). Rust core with Python/Node/Go/Java/C#/WASM bindings; robust against malformed HTML; GFM tables with alignment; **metadata extraction** (OpenGraph/Twitter/JSON-LD/microdata); optional Djot output; visitor API. Newer and more featureful, but a younger ecosystem and a vendor-driven README.
- `html2md` 0.2.17 — **GPL-3.0+**. ⚠️ Reject.
- `html2text` 0.17.1 (2026-04-19, 5.67M dl, MIT) — renders to *plain text* with layout; different job.
- **`dom_smoothie` 0.18.0** (2026-06-07, 384k dl / 312k, MIT, 217★, pushed 2026-08-03) — actively maintained Readability port. Best available.
- `readability` 0.3.0 (2023-12-20) and `readability-rs` 0.5.0 (2024-12-18, 5.9k dl) — stale. `readability-rust` 0.1.0 (2025-07-22, 20★) — too new/small.

**RECOMMENDATION for `WebFetch`:** `dom_smoothie` 0.18 to extract the article, then **`htmd` 0.5.5** to convert. Pick `htmd` over `html-to-markdown-rs` for the smaller surface and the `html5ever` foundation you can reason about; revisit if you need the metadata extraction.

**Web search — there is no mature Rust client for Brave, Tavily, or Exa.** I searched crates.io: `tavily` 2.1.0 (2026-01-27, 56k dl) is one author's thin wrapper; everything else (`brave-cli`, `agent-search`, `websearch`, various `*-agents-*`) is sub-1k-download hobby code. **Hand-roll**: each is one POST with an API key header and a JSON response — ~80 lines per provider behind one `SearchProvider` trait.

## 13. Skills (SKILL.md + arg templating)

- **YAML — the critical finding: `serde_yml` is unsound and archived.** `sebastienrousseau/serde_yml` is **`archived: true`**, its GitHub description reads *"[DEPRECATED] Final release is a thin compatibility shim. RUSTSEC-2025-0068 structurally fixed in 0.0.13. Migrate to a maintained alternative."* **RUSTSEC-2025-0068** (2025-09-11, `GHSA-hhw4-xg65-fp2x`, informational=unsound, `patched = []`): *"Using `serde_yml::ser::Serializer.emitter` can cause a segmentation fault."* Despite 22.4M downloads. **Do not use.**
- `serde_yaml` 0.9.34**+deprecated** (2024-03-25) — dtolnay archived it; both codex and goose still ship it. Works, but is a dead end.
- **`serde-saphyr` 1.1.0** (2026-08-15, 5.29M dl / **4.12M in 90 days**, MIT/Apache, `bourumir-wyngs/serde-saphyr` 218★, pushed 2026-08-28). Built on `granit-parser` 1.1.0. `#![forbid(unsafe_code)]`, Miri + fuzz CI, panic-free-on-malformed-input claim, **no tag-driven object instantiation** (structurally immune to the classic YAML RCE class), configurable `Budget` limits for resource exhaustion, snippet error rendering, optional `!include`, comment capture, serializer with anchors, and a 1.0-API-compat CI job. Deserializes directly into your types with no intermediate Value tree.
- `serde_yaml_ng` 0.10.0 (2024-05-26, 9.8M dl, 113★, pushed 2025-09-14) — a serde_yaml fork on the **unmaintained** `unsafe-libyaml`. RUSTSEC lists it as an alternative but it inherits C-lib risk.
- `serde_norway` 0.9.42 (2024-12-21, 10.2M dl) — the other RUSTSEC-recommended fork, on `unsafe-libyaml-norway`.
- `saphyr` 0.0.12 / `saphyr-parser` 0.0.12 (2026-08-18, 336★) — YAML 1.2-compliant, but 0.0.x and no license field on the repo.

**RECOMMENDATION: `serde-saphyr` 1.1.0.** It's the only actively maintained, pure-Rust, semver-1.0, security-designed option, and its 90-day download share (4.12M of 5.29M lifetime) shows the ecosystem is moving there right now. Fall back to `serde_norway` if you hit a YAML feature gap.

- **Frontmatter:** `gray_matter` 0.3.2 (2025-07-10, 734k dl, `yuchanns/gray-matter-rs` 57★, last push 2025-07-11). Small and idle. **Just split it yourself**: the `---\n…\n---\n` split is ~15 lines, and codex's `codex-skills` does exactly that (its deps are only `serde_yaml` + `shlex` + `include_dir`). Hand-rolling also lets you keep byte offsets for error reporting.
- **Templating:** `minijinja` **2.24.0** stable (2026-08-12, 31.3M dl, Apache-2.0, mitsuhiko, 2,746★); `3.0.0-alpha.0` also published 2026-08-12 — stay on 2.24. But note `codex-utils-template` has **zero dependencies** — for `$1`/`$ARGUMENTS`/`$@` substitution you need no template engine. **Take minijinja only if skills need conditionals/loops/filters**; otherwise a 100-line substituter is the 无为 answer.

## 14. Tokenizers

- **`tiktoken-rs` 0.12.0** (2026-06-02, 15.0M dl / 6.62M, MIT, `zurawiki/tiktoken-rs` 405★, pushed 2026-07-01). Encodings: `o200k_base` (GPT-5/o-series/4o/4.1), `o200k_harmony` (gpt-oss), `cl100k_base`, `p50k_base`, `p50k_edit`, `r50k_base`. Model-name→encoding helpers, chat-completion token counting, optional `async-openai` feature. Its docs explicitly scope it to OpenAI and point elsewhere for other models.
- `tokenizers` (HuggingFace) 0.23.1 (2026-04-27, 29.1M dl, Apache-2.0). Heavy (pulls `onig`/rayon/etc.) — only if you need a real HF tokenizer.json.

**RECOMMENDATION:** `tiktoken-rs` for OpenAI-side budgeting. **For Anthropic, there is no public tokenizer** — use the `POST /v1/messages/count_tokens` endpoint for exact counts, and a cheap `chars/3.5` heuristic for the fast path (budgeting/compaction triggers), reconciling against the `usage` block returned by every response. Do not pretend cl100k approximates Claude.

## 15. Plugin registration

- **`inventory` 0.3.24** (2026-03-30, 120M dl / 28.4M, dtolnay, 1,341★, pushed 2026-08-22) — distributed slice via life-before-main. **Codex uses `inventory = "0.3.19"`** for its plugin system. Works across crates without a central registry file.
- `linkme` 0.3.37 (2026-07-18, 29.9M dl, dtolnay) — linker-section slices, no ctor. Slightly faster, slightly more platform-fragile.
- `extism` 1.30.0 (2026-06-04, 654k dl, BSD-3-Clause, 5,742★, pushed 2026-08-25) — WASM plugin runtime. Only relevant for *untrusted third-party* plugins, not for in-process Rust crates behind traits.
- `libloading` 0.9.0 (2025-11-05, 495M dl, ISC) — dylib loading. ABI-unstable across rustc versions; avoid for a Rust plugin API.

**RECOMMENDATION:** for **in-process plugin crates behind stable traits**, you need no registration crate at all — a `register(&mut Registry)` call per plugin crate in `main.rs` is explicit, ordered, and debuggable (道法自然 / 损). Reach for **`inventory`** only when plugin crates must self-register without the binary naming them. Keep `extism` in the back pocket for a future untrusted-plugin tier.

## 16. Observability

- `tracing` **0.1.44** (2025-12-18, 803M dl), `tracing-subscriber` **0.3.23** (2026-03-13, 573M dl), `tracing-appender` **0.2.5** (2026-04-17, 106M dl). All MIT, tokio-rs.
- OpenTelemetry **0.32.0** / `opentelemetry_sdk` **0.32.1** (2026-05-26) / `opentelemetry-otlp` **0.32.0**, with **`tracing-opentelemetry` 0.33.0** (2026-05-18). Apache-2.0.

**RECOMMENDATION:** `tracing` + `tracing-subscriber` (`EnvFilter` + JSON layer) + `tracing-appender` for rotating file logs. **Pin the OTel stack at 0.32/0.33** — that's goose's current set; codex is one minor behind at 0.31/0.32. The OTel Rust crates bump all six crates in lockstep and break between minors, so version them as one workspace unit and gate the whole thing behind an `otel` cargo feature so the default build doesn't pay for it.

## 17. CLI / release / quality

- **`clap` 4.6.6** (2026-08-06, 1.08B dl, MSRV 1.85). Uncontested.
- **`dist`** — the tool formerly known as `cargo-dist`; crate name is still **`cargo-dist` 0.32.0** (2026-05-22). `axodotdev/cargo-dist` 2,106★, **pushed 2026-08-28**, Apache-2.0 — actively maintained (331 open issues). README confirms the rename.
- `self_update` **0.44.0** stable (2026-04-05) / `1.0.0-rc.6` (2026-07-16), 11.1M dl, `jaemk/self_update` 958★, pushed 2026-08-27. Use 0.44 stable; watch for 1.0.
- `cargo-deny` **0.20.2** (2026-07-09, 5.31M dl, 2,409★) — licenses, advisories, duplicate versions, sources. **Non-optional for this project** given the GPL/MPL/unsound crates enumerated above.
- `cargo-nextest` **0.9.143** (2026-08-04, MSRV 1.91) — per-test process isolation matters when tests touch cwd/env/keyring.
- `cargo-hakari` 0.9.38 (2026-05-21) — workspace-hack; adopt only when the workspace crosses ~20 crates and CI feature-unification churn hurts.
- `cargo-udeps` 0.1.61 (2026-04-29, nightly-only) vs **`cargo-machete` 0.9.2** (2026-04-15, stable, 2.75M dl) — prefer machete for CI; note codex uses `cargo-shear`.

## 18. Testing

`insta` **1.48.0** (2026-06-11, 93.3M dl, 2,951★) — snapshot-test every JSONL journal event and every rendered prompt; this is the highest-leverage test tool for an agent. `wiremock` **0.6.5** (2025-08-24, 72.5M dl, 798★, repo last push 2025-08-24 — a year idle but API-complete) for provider HTTP fakes, including SSE bodies. `assert_cmd` **2.2.2** + `predicates` **3.1.4** for the CLI surface. `tempfile` **3.27.0**. `proptest` **1.11.0** (2026-03-24, 2,223★) — aim it at the permission-rule matcher and the shell splitter, where adversarial inputs are the threat model. `rstest` **0.26.1** (2025-07-27). `tokio-test` **0.4.5**.

## 19. Async utilities

- `tokio` **1.53.1** (2026-07-20); `tokio-util` **0.7.19** (2026-07-21) — `CancellationToken` for turn abort, `codec::{LinesCodec, FramedRead}` for NDJSON, `task::TaskTracker` for graceful shutdown. Nothing else needed.
- **`async-trait` 0.1.92** (2026-08-08, 626M dl). AFIT is stable in the language, but your `Provider`/`Tool`/`Surface` traits must be **`dyn`-compatible** for a plugin host, and native AFIT still isn't. `async-trait` is the pragmatic answer.
- `trait-variant` 0.1.3 (2026-07-22, rust-lang) — adds `Send` bounds to AFIT traits; useful for the *static* half of an API.
- `dynosaur` 0.3.1 (2026-07-03, 1.58M dl, 464k/90d) — generates a `dyn`-compatible wrapper for AFIT traits. The intended future replacement for `async-trait`, but still 0.3 and low adoption. **Not yet.**
- `futures` 0.3.34 (2026-08-11); `async-stream` 0.3.6 (2024-10-01, 308M dl — stable, not stale).

**RECOMMENDATION:** `async-trait` on the plugin traits now; revisit `dynosaur` when it reaches 0.5+/1.0. Note codex uses `async-channel` 2.3.1 rather than tokio mpsc in its hooks/event paths — worth considering for the journal fan-out since it's MPMC and runtime-agnostic.

## 20. IM surfaces (later)

| Platform | Crate | Version (date) | ★ / push | License | Grade |
|---|---|---|---|---|---|
| Telegram | `teloxide` | 0.17.0 (2025-07-11) | 4,221 / 2026-08-08 | MIT | A− (repo active, release 13mo old) |
| Slack | `slack-morphism` | 2.25.0 (2026-08-20) | 227 / 2026-08-20 | Apache-2.0 | A− (Web/Events/Socket Mode + Block Kit) |
| Discord | `serenity` | 0.12.5 (2025-12-20) | 5,594 / 2026-08-27 | ISC | A |
| Matrix | `matrix-sdk` | 0.18.0 (2026-06-02) | 2,263 / 2026-08-28 | Apache-2.0 | B+ (MSRV **1.93**, heavy, E2EE) |
| Lark/Feishu | **`openlark` 0.20.0** | 2026-07-27 | 105 / 2026-08-24 | Apache-2.0 | **C+** |

⚠️ **Lark correction:** `open-lark` 0.14.0 is **stale (2025-09-30)**. The project was renamed — the repo is now `foxzool/openlark` and it publishes a **modular crate family**: `openlark`, `openlark-core`, `openlark-client`, `openlark-auth`, `openlark-docs`, `openlark-meeting`, `openlark-webhook`, `openlark-application`, all **0.20.0 (2026-07-27)**. Adoption is tiny (6.4k lifetime downloads on `openlark`, 105★). Also live: `larksuite-oapi-sdk-rs` 0.3.10 (2026-08-28, 2★, MSRV 1.95 — brand new), `lark-channel` 0.6.0, `feishu-sdk` 0.1.2, and `lark-websocket-protobuf` 0.1.2 (23k dl — the WS long-connection protobufs, useful on its own).

**RECOMMENDATION:** Telegram/Slack/Discord are well served. For Lark, **do not depend on any of these for production** — write a thin client for the handful of endpoints a chat surface needs (tenant access token, `im/v1/messages`, card callbacks, WS or webhook events), porting from the **official `larksuite-oapi-sdk-go`/`-python`**. Lark's OAPI is huge and every Rust SDK covering "all of it" is one person's generated code.

## 21. Misc

- `regex` **1.13.1** (2026-07-15, 1.10B dl); `regex-lite` 0.1.9 where you only need the API and want fast compiles (codex uses both).
- `unicode-width` **0.2.2** (2025-10-06); `unicode-segmentation` **1.13.3** (2026-06-01, MSRV 1.85).
- **`jiff` 0.2.35** (2026-07-25, 172M dl / **58.0M in 90 days**, Unlicense/MIT, BurntSushi, 2,902★) vs `chrono` 0.4.45 (2026-06-04, 765M dl / 164M, 3,901★). jiff has correct tz-aware arithmetic, a bundled tzdb, and a saner API; chrono has the ecosystem (`sqlx`, `schemars` `chrono04` feature — which `rmcp` enables). **Use `jiff` for your own logic, and accept `chrono` transitively.** Serialize timestamps in the journal as RFC-3339 strings so the choice stays reversible (守一: the journal format is the contract, not the crate).
- **`thiserror` 2.0.20** for library/kernel errors + **`anyhow` 1.0.104** at the binary edge. `miette` 7.6.0 (2025-04-27, 69.8M dl) only if you want source-span diagnostics rendered for config/skill parse errors — it's a real UX win there and nowhere else.
- `semver` **1.0.28**; `base64` **0.23.1** (2026-08-04) — note codex is on 0.22.1, goose on 0.23; take 0.23.

---

## Port candidates (ranked by value)

1. **Vercel AI SDK — `@ai-sdk/provider` v4** (Apache-2.0, 26.5k★). *Port:* the `LanguageModelV4` type algebra — content parts, stream parts, finish reasons, usage, tool call/result/**approval**, reasoning, provider-tools, call options. This is your `Provider` trait's shape, already argued to death by people who normalized 30 providers. Port types, not code. ~1 week.
2. **pi-ai OAuth + model catalog** (MIT, 98.7k★). *Port:* `packages/ai/src/auth/oauth/{anthropic,openai-codex,device-code,pkce,oauth-page,load}.ts` and `model-catalog.ts`/`models.generated.ts`. **Highest-value item in the report** — the Anthropic Pro/Max and Codex OAuth flows exist in no Rust crate and no spec; this is the only readable reference. ~1 week.
3. **Mozilla Readability** (Apache-2.0). *Port:* scoring heuristics, `_grabArticle` candidate selection, `unlikelyCandidates` regexes. `dom_smoothie` covers most of it but is one maintainer at 217★ — vendoring the algorithm de-risks `WebFetch`. ~3 days if you start from `dom_smoothie`.
4. **codex `apply-patch` (V4A) + `shell-command` splitting** (Apache-2.0, vendorable from GitHub). *Port:* the V4A envelope parser with fuzzy context matching, and the tree-sitter-bash decomposition into leaf commands. The second one is security-critical and easy to get subtly wrong. ~1 week.
5. **`html-to-markdown` (xberg-io) metadata layer** (MIT) — if you want OpenGraph/JSON-LD extraction for `WebFetch` results; or just depend on the crate.
6. **litellm provider tables** (MIT). *Port:* error-code→canonical-error and param-alias tables only. Data, not architecture. ~1 day.
7. **larksuite-oapi-sdk-go** — only the auth/token-cache + WS event-loop pieces, when the Lark surface lands.

## Rust ecosystem gaps

1. **No official Anthropic SDK, and no viable community one.** All four candidates (`anthropic` 2024-09, `clust` 2024-06, `misanthropy` 2025-06, `anthropic-ai-sdk` 2026-01/18★) are single-author and idle. Confirmed: hand-roll.
2. **No agent-provider OAuth crate.** `oauth2` gives the generic machinery; the Anthropic Pro/Max, Codex, and Copilot flows (custom scopes, headers, token-exchange quirks, loopback page HTML) exist only in TS. Gap #2 in the port list.
3. **No JSON-RPC-over-stdio crate.** `jsonrpsee` has no stdio transport and no release in 12 months; everything else is dead. Every agent hand-rolls it (codex, ACP, rmcp).
4. **No maintained YAML crate with an unambiguous track record.** `serde_yaml` deprecated, `serde_yml` unsound+archived (RUSTSEC-2025-0068), the libyaml forks sit on unmaintained C. `serde-saphyr` is the most credible answer and is only 2 weeks past its 1.1.0.
5. **No macOS or Windows sandbox crate.** Linux is well served (`landlock`, `seccompiler`); the other two platforms have zero published crates and `birdcage` — the one cross-platform attempt — is archived and GPL.
6. **No web-search API clients worth depending on** (Brave/Tavily/Exa). All sub-1k-download hobby wrappers.
7. **No Claude-Code-style permission-policy crate.** Rule syntax, shell-splitting semantics, deny/allow/ask precedence — no OSS implementation in any language. This is genuinely novel work in bingo.
8. **`figment` is effectively abandoned** (2 years) despite 35.8M downloads — a trap for anyone recalling 2024-era advice.
9. **Frontmatter parsing has no maintained crate** worth the dependency (`gray_matter` 57★, idle).
10. **Anthropic has no offline tokenizer.** Exact counts require an API round-trip; plan the compaction trigger around approximate counts reconciled against response `usage`.

## Corrections to the already-verified list

- **`landlock` is 0.4.7 (2026-07-27)**, not 0.4.4. Codex still pins 0.4.4.
- **goose has moved to `aaif-goose/goose`** (`block/goose` redirects), is at **v1.48.0** with **`rmcp = "3.0.0"`**, **`agent-client-protocol = "2.0.0"`**, plus **`agent-client-protocol-http = "2.0.0"`** and **`agent-client-protocol-schema = "=1.5.0"`** — all three `[patch]`ed to git rev `c97a520`. `agent-client-protocol-schema` is now at **1.7.0** (2026-08-20) on crates.io, ahead of goose's pin.
- **`rmcp` 3.1.4 requires `reqwest ^0.13.2`, `schemars ^1.0` (with `chrono04`), `oauth2 ^5.0` (default-features off), and `sse-stream ^0.2.4`** — that fixes several of your version choices for free.
- **codex is on `reqwest 0.12` and `schemars 0.8.22`**, i.e. *behind* rmcp's requirements; it pins `rmcp = "=3.1.3"` with `default-features = false`.
