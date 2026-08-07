# Provider protocol layer + OAuth access — design draft

> Status: draft (for team alignment on #provider-oauth, pending main's confirmation)
> Branch: `feat/provider-oauth` · Related decision records: D1 (Messages API client), D6 (count_tokens), D9 (config layering), D10 (prompt caching), D29/D31 (subagents, teams)
> Contract-first: per AGENTS.md, every independently consumed boundary (settings schema, cross-process protocol, persistence format) gets a serde contract first; all implementations check against the same tables.
> §6 filled by devex research pass (codex 0.146 source + opencode source + local installs, 2026-08).

## 1. Goal

bingo today speaks only the Anthropic Messages protocol. We want:

1. A **protocol abstraction layer**: `anthropic` (Messages, existing) and `openai` (Responses API) as pluggable implementations, selected per named provider.
2. **OAuth-based providers** so bingo can use subscriptions without an API key: Codex (ChatGPT account OAuth) and opencode go.
3. Zero disruption to existing users: current `providers` entries and top-level `apiKey`/`apiBaseUrl` keep working unchanged.

## 2. Current state (as-is)

### 2.1 Configuration (`src/settings.rs`)

- `Settings.api_key` / `Settings.api_base_url` (top level) + env fallbacks `ANTHROPIC_API_KEY` / `DEEPSEEK_API_KEY`, `ANTHROPIC_BASE_URL`. Top level = implicit provider `"default"`.
- `Settings.providers: HashMap<String, ProviderConfig>` where `ProviderConfig { apiKey: String, apiBaseUrl: String, supportsImages: Option<bool> }`. **Anthropic protocol assumed for every entry.**
- `Settings.provider` (current provider name, persisted by `/provider` and the `/model` menu; invalid name falls back to `"default"` with a warning).
- `Settings.send_images` (default provider image support; named providers use their own `supportsImages`).
- Layering: user → project → local shallow merge (`load_settings`); `/provider`/`/model` persist via `upsert_project_settings` into `.bingo/settings.json`.
- **Capabilities today**: only `supportsImages` (plus `cacheControl` and `thinkingLevel` which are global switches, not per-provider).

### 2.2 API client (`src/api/`)

- `api/types.rs`: Anthropic-wire serde types (`Request`, `Message`, `Role`, `ContentBlock`, `SystemBlock`, `StreamEvent`, thinking/effort param builders). `API_BASE = https://api.anthropic.com`, `API_VERSION = 2023-06-01`.
- `api/client.rs`: `Client` struct = reqwest + an `Arc<RwLock<Endpoint>>` (`{api_key, base_url, supports_images}`) + a named-provider table. Methods:
  - `stream(Request) -> Stream<StreamEvent>`: POST `{base}/v1/messages`, SSE parse, retries (429/5xx, retry-after, backoff 500ms→32s, MAX_RETRIES=5), 120s connect timeout, 60s stream idle timeout, 400 context-overflow max_tokens recompute.
  - `complete_text(Request) -> String` (non-streaming, 15s short-write timeout): used by compact/memory extraction.
  - `list_models() -> Vec<String>`: GET `{base}/v1/models`, 10s short-read timeout (used by `/model` menu).
  - `count_tokens(model, system, messages) -> u64`: POST `{base}/v1/messages/count_tokens` (D6; budget display, compact threshold).
  - `headers()`: `x-api-key` + `anthropic-version` + content-type.
  - Provider switch: `set_provider(name)`, `with_provider(name)` (subagent fork), `provider_names()`, `provider_endpoint(name)`, `supports_images()`, `current_endpoint()`.
- `api/sse.rs`: incremental SSE frame parser (8MB buffer ceiling, boundary overlap rescan).
- `api/image.rs`: decode → downscale to 2000px / ~3.75MB raw → base64 PNG/JPEG.

### 2.3 Consumers

- `query.rs::one_turn`: builds `Request` (model from `Runtime.model`, thinking/effort from `Runtime.thinking`), streams, feeds `AssistantAccumulator` (text/thinking/tool_use block accumulation), then executes tools and re-requests with `tool_result` backfill.
- `compact.rs` / `memory.rs`: `complete_text` for summaries; `count_tokens` for the token gate.
- `budget.rs`: **hardcodes `CONTEXT_WINDOW = 200_000` and `DEFAULT_MAX_TOKENS = 64_000`** (Claude window). OpenAI models have different windows (gpt-5 family ~400k); the compact/auto-compact thresholds and the `/context` bar derive from this constant. → `Capabilities` must carry `context_window`/`max_output_tokens` and the budget module must resolve them per active provider (fallback to today's constants for anthropic).
- `tui/chat.rs`: `/provider` (list with masked key + URL, switch + persist), `/model` two-level menu (providers → `list_models()`), footer model badge `{provider} · {model} · think {level}`.
- `tool/agent.rs`: subagents — `with_provider(name)` forks the endpoint; **cross-provider rule**: forking to a provider different from the parent's current one requires an explicit `model` (early failure), `thinking` defaults to off; same provider inherits model/thinking.
- Error mapping: `ClientError` → stable codes (`AUTH_REQUIRED`, `PERMISSION_DENIED`, `RATE_LIMITED`, `SERVER_ERROR`, `OFFLINE`, `TIMEOUT`), drift-guarded in `error.rs` tests.

### 2.4 Key constraints from decision records

- D1: hand-rolled client, no SDK. Retry policy, SSE, prompt caching GA, adaptive thinking (Claude 5 family: `budget_tokens` rejected; `{"type":"adaptive"}` + `output_config.effort`).
- D6: token counting via official count_tokens API; local estimate never authoritative.
- D9: config layers + feature flags; new capabilities default off.
- D10: system prompt segments + `cache_control: ephemeral` tail breakpoints; **this is Anthropic-specific** — OpenAI has its own caching (`prompt_cache_breakpoint`, `prompt_cache_options.ttl`), so caching strategy becomes per-protocol.
- AGENTS.md: contract first for independently consumed boundaries; subtract by default; no unwrap/expect; no unneeded deps.

## 3. Design: protocol abstraction layer

### 3.1 Layering

```text
queryLoop / compact / memory / TUI menu        (protocol-agnostic consumers)
        │
        ▼
   ProviderClient trait                         (neutral request/event types)
        │
        ├── AnthropicAdapter  (moves current api/client.rs code)
        ├── OpenAIAdapter     (Responses API, new)
        └── (future: others — additive)
        │
        ▼
   Auth  (ApiKey | Bearer | OAuth token provider w/ refresh)
        │
        ▼
   reqwest + SSE/JSON
```

Boundaries that get a serde contract (per AGENTS.md):

1. **settings schema** (consumed by users and persisted by `/provider`, `/model`):
   `ProviderConfig` v2 (additive fields only — see §5).
2. **neutral request/event types** (`api::contract`): the cross-protocol exchange format between consumers and adapters.
3. **auth.json persistence format** (OAuth tokens; cross-session, possibly shared with other tools — see §4.3).

### 3.2 Neutral contract (`src/api/contract.rs`, new)

Mirror of today's `api/types.rs` shapes, but protocol-free:

```rust
/// Provider capabilities — negotiated at config time, consumed by the UI
/// (image attach button, /think menu), by message building, and by budget.rs
/// (context window / output budget per provider).
#[derive(Debug, Clone, Default)]
pub struct Capabilities {
    pub supports_images: bool,
    pub supports_thinking: bool,     // effort/reasoning levels
    pub supports_tools: bool,        // always true in practice
    pub supports_count_tokens: bool, // OpenAI has no count_tokens endpoint
    pub supports_prompt_caching: bool,
    pub context_window: u64,         // anthropic 200_000; openai per model family (default 400_000)
    pub max_output_tokens: u32,      // DEFAULT_MAX_TOKENS today; per-provider override
}

/// Neutral streaming request. All fields Option/skippable — each adapter
/// maps to its own wire format.
pub struct NeutralRequest {
    pub model: String,
    pub max_tokens: u32,
    pub system: Vec<String>,           // cache breakpoints decided by adapter
    pub messages: Vec<NeutralMessage>, // role: user/assistant (+ tool blocks)
    pub tools: Vec<serde_json::Value>, // schemars JSON schema
    pub thinking: Option<ThinkingLevel>, // off = None (no param sent)
    pub stream: bool,
}

pub enum NeutralMessage {
    UserText { text: String, images: Vec<ImageAttachment> },
    AssistantText { text: String, thinking: Option<String> }, // thinking for replay
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { id: String, content: serde_json::Value, is_error: bool },
}

/// Neutral stream event — what `api/types.rs::StreamEvent` already is; reused
/// verbatim (rename to contract::StreamEvent). queryLoop/accumulator keep
/// consuming it unchanged.
```

Notes:

- `thinking` as an enum level (`off|low|medium|high|xhigh|max`) instead of the raw Anthropic JSON: Anthropic maps level → `{"type":"adaptive"}` + `output_config.effort`; OpenAI maps level → `reasoning.effort` (and `reasoning.summary`). The consumer never sees wire JSON.
- Tool schemas stay `serde_json::Value` (already protocol-neutral JSON Schema via schemars).
- `max_tokens` → OpenAI: `max_output_tokens`; the 400-context-overflow recompute stays in the Anthropic adapter (OpenAI reports context via its own error body; handle in phase 2 with a similar heuristic or accept retry guidance).

### 3.3 ProviderClient trait (`src/api/provider.rs`, new)

```rust
#[async_trait]
pub trait ProviderClient: Send + Sync {
    fn capabilities(&self) -> Capabilities;
    async fn stream(&self, req: &NeutralRequest)
        -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent, ClientError>> + Send>>, ClientError>;
    async fn complete_text(&self, req: &NeutralRequest) -> Result<String, ClientError>;
    async fn list_models(&self) -> Result<Vec<String>, ClientError>;
    async fn count_tokens(&self, model: &str, system: &[String], messages: &[NeutralMessage])
        -> Result<u64, ClientError>; // adapters without the endpoint return Err(Unsupported)
    fn auth_status(&self) -> AuthStatus; // for /provider listing & /login UX
}
```

- `ClientError` gains an `Unsupported(String)` variant mapped to `SERVER_ERROR` (or a new `CONFIG_INVALID` presentation when a capability is required but absent).
- `Client` keeps its public surface (provider switching, `with_provider`, endpoint info) but internally holds `Arc<dyn ProviderClient>` + the provider table of `Arc<dyn ProviderClient>`; `Endpoint`-specific fields move into the adapter state.

### 3.4 Provider registry (`src/api/registry.rs`, new)

```rust
pub enum Protocol { Anthropic, OpenAI }

pub fn build_provider(name: &str, cfg: &ProviderConfig, auth: AuthSource, home: &Path)
    -> Result<Arc<dyn ProviderClient>, ConfigError>;
```

- Unknown `protocol` value → `CONFIG_INVALID` with a message listing valid values (config error at startup, not mid-request).
- Top-level `apiKey`/`apiBaseUrl` build the implicit `"default"` provider with `Protocol::Anthropic` (unchanged behavior).

## 4. Adapters

### 4.1 Anthropic adapter — absorb, don't rewrite

- Move `api/client.rs` internals into `api/providers/anthropic.rs` (adapter) + `api/types.rs` stays as its wire module (rename `api/providers/anthropic/wire.rs` if cleaner).
- The `Endpoint { api_key, base_url, supports_images }` struct becomes the adapter's state; `supports_images` comes from config, the rest of `Capabilities` is hardcoded (`thinking=true`, `count_tokens=true`, `caching=true` when `cacheControl` enabled).
- **Behavior must be byte-identical**: same retry/backoff/timeouts, same 400-overflow recompute, same SSE parser, same error mapping. The refactor is covered by existing tests (client.rs tests + query.rs mock-server tests) running green before anything new lands.
- `count_tokens` stays Anthropic-only; `complete_text` maps `NeutralRequest → Request { stream:false }`.

### 4.2 OpenAI adapter — Responses API (`src/api/providers/openai.rs`, new)

Endpoint: `POST {base}/v1/responses` (default base `https://api.openai.com`), auth `Authorization: Bearer <token>`.

Mapping table (contract → wire):

| NeutralRequest | Responses API |
|---|---|
| `system` | `instructions` (string; join segments) |
| `messages` | `input`: array of items — user text → `{type:"message", role:"user", content:[{type:"input_text", text}]}`; images → `{type:"input_image", image_url:"data:{mime};base64,{data}"}`; assistant text → `{type:"message", role:"assistant", content:[{type:"output_text", text}]}`; tool_use → `{type:"function_call", call_id, name, arguments: json_string}`; tool_result → `{type:"function_call_output", call_id, output: json_string}` |
| `tools` | `tools: [{type:"function", name, description, parameters, strict:false}]` |
| `thinking` | `reasoning: {effort: level}` for levels; `off` → omit (o-series may still reason; acceptable, matches Claude `off` semantics loosely) |
| `max_tokens` | `max_output_tokens` |
| `stream` | `stream: true` |

SSE event mapping (verified against the Responses API reference):

| Responses SSE | contract::StreamEvent |
|---|---|
| `response.created` | `MessageStart { id: response.id, model }` |
| `response.output_item.added` (item type `message`, content `output_text`) | `TextStart { index }` |
| `response.output_text.delta` | `TextDelta { index, text }` |
| `response.output_item.added` (item type `function_call`) | `ToolUseStart { index, id: call_id, name }` |
| `response.function_call_arguments.delta` | `InputJsonDelta { index, partial_json }` |
| `response.output_item.done` (function_call, full `arguments`) | `BlockStop { index }` (accumulator parses the final JSON; delta-only accumulation needs care — see note) |
| `response.output_item.done` (message) | `BlockStop { index }` |
| `response.completed` / `response.failed` | `StopReason { stop_reason, output_tokens }` / `ApiError`; then `Done` |
| `response.reasoning_summary_text.delta` | `ThinkingDelta`-equivalent (map into a `ThinkingStart/Delta` sequence) |

Accumulator notes:

- Anthropic streams `input_json_delta` fragments; Responses streams `function_call_arguments.delta` fragments the same way → feed both into `InputJsonDelta` accumulation; on `output_item.done` the full item carries `arguments` — treat it as the final delta if the accumulated JSON fails to parse (defensive).
- Thinking: Responses gives `reasoning` items (encrypted/summarized by default; `response.reasoning_summary_text.delta` provides the visible summary). Map summary deltas to `ThinkingDelta` so the UI keeps its thinking affordance; never send thinking back to the model (it's not replayable like Anthropic's signature) — on replay, drop `thinking` from `AssistantText` for OpenAI.
- `stop_reason` mapping: `completed` → `end_turn`; `incomplete` → `max_tokens` (when `incomplete_details.reason == "max_output_tokens"`); `failed` → error.
- **Tool-result error flag**: Anthropic `tool_result` carries `is_error: bool`; Responses `function_call_output.output` is a plain string. Encode the error flag into the output string — serialize `{"is_error":true,"content":…}` (or a documented error-prefix convention) on the wire, decode on the way back into `NeutralMessage::ToolResult { is_error }`. Exact convention chosen in P1 with a round-trip test.
- **Index bookkeeping (two-level → one)**: Responses carries two index planes — `output_index` (output items: message/function_call) and `content_index` (content parts within a message item; `response.output_text.delta` carries item_id + output_index + content_index). The adapter must flatten both into the single block index that `StreamEvent`/the accumulator use (Anthropic's content_block index semantics). Rule: assign one StreamEvent index per output item; content-part deltas within a message item all target that item's index. Covered by a mapping test with a multi-item, multi-part fixture.
- **`stop_reason` semantics**: `completed` → `end_turn`; `incomplete` → `max_tokens` (when `incomplete_details.reason == "max_output_tokens"`) — query.rs's max_tokens continuation logic depends on this; `failed` → `ApiError` with the error detail, never a silent `Done`.

`list_models`: GET `{base}/v1/models` (same `data[].id` shape — shared). `count_tokens`: **no public endpoint** → return `Unsupported`; the token gate falls back to local estimation (D6 already keeps a local estimator; make it the fallback path instead of a hard error). `complete_text`: `stream:false` + parse `output[].content[].text`.

### 4.3 OAuth auth source

Two auth kinds on `ProviderConfig` (contract, §5): `apiKey` (existing) and `oauth` (new). The auth layer is a small `TokenProvider` behind the adapter:

```rust
pub enum AuthSource {
    ApiKey(String),
    OAuth(Arc<dyn TokenProvider>), // get access token (lazy refresh), persist on change
}
```

`TokenProvider::token() -> Result<String, AuthError>` does: read cached token → if expired (or 401 on use), refresh via the provider's refresh endpoint → persist → return. Single-flight refresh (one `tokio::sync::Mutex` per provider) so concurrent streams don't stampede the token endpoint. Eager refresh 5 min before expiry; permanent refresh failures clear auth and prompt re-login (§6.3).

## 5. Settings contract v2 (serde, additive)

```jsonc
// ProviderConfig v2 — old fields keep their meaning; new fields optional.
{
  "apiKey": "sk-...",              // v1: static key (protocol: anthropic)
  "apiBaseUrl": "https://...",     // v1
  "supportsImages": true,          // v1
  // v2 additions:
  "protocol": "anthropic" | "openai",  // optional; default "anthropic" (backward compat)
  "oauth": {
    "kind": "codex",             // v1: "codex" only; "opencode" reserved (go 订阅走 apiKey，§6.0)
    "account": "user@example.com"  // optional: pick a stored account
  },
  "capabilities": {                // optional overrides; default = protocol defaults
    "supportsImages": false,
    "supportsThinking": true
  }
}
```

Rules (main's rulings applied):

- `protocol` missing → `anthropic` (every existing config parses unchanged).
- `apiKey` missing + `oauth` present → OAuth provider; both missing → config error at startup with clear copy.
- Both `apiKey` and `oauth` present → **`apiKey` wins + a startup warning** (main's ruling; explicit static key overrides OAuth, useful for debugging).
- `capabilities` overrides the protocol defaults only for the listed keys (missing keys = protocol defaults).
- Top-level `sendImages`/`cacheControl`/`thinkingLevel` remain global defaults for `"default"`; named providers keep using their own `supportsImages`/`capabilities` (unchanged semantics).
- `CacheControl` becomes Anthropic-only behavior: OpenAI adapter maps system → `instructions` with `prompt_cache_breakpoint`/`prompt_cache_options.ttl` only when a future `cacheControl`-equivalent is enabled; for now `cacheControl:true` on an OpenAI provider logs a warning and is ignored (capability `supports_prompt_caching=false`).

## 6. OAuth flows (filled from the devex research pass — verified against codex 0.146.0 source, opencode source, and local installs)

### 6.0 Correction to the premise: opencode-go is NOT OAuth

Research result: **opencode-go (like OpenCode Zen) is an API-key subscription, not an OAuth flow.** Per opencode docs: `/connect` → select OpenCode Go → opencode.ai/auth → sign in → **copy API key** → paste. No device flow, no tokens. So "opencode go 订阅" lands in bingo as a **named provider with `protocol: "openai"` (or whatever it exposes) + `apiKey`** — plain API-key provider, zero OAuth code. The OAuth work is driven by **Codex (ChatGPT account)**; opencode-go's protocol/base URL to be verified at implementation time (its docs point to opencode.ai/auth; models are "popular open coding models").

The `oauth.kind` enum in §5 therefore starts with `"codex"`; `"opencode"` is not needed for the go subscription (main confirmed: opencode-go = `protocol: "openai"` + `apiKey`, zero OAuth code; kept as a future extension point if opencode ever ships an OAuth flow).

### 6.1 Codex / ChatGPT auth flows (the reference implementation)

Verified from `openai/codex` source (`codex-rs/login/`), opencode's `openai.ts` plugin, and `notes/research-oauth-cli.md`. **Codex has two login flows** (opencode implements both too):

```
A. Loopback PKCE (codex default; opencode "ChatGPT Pro/Plus (browser)"):
   GET {issuer}/oauth/authorize?response_type=code&client_id=app_EMoamEEZ73f0CkXaXp7hrann
      &redirect_uri=http://localhost:{port}/auth/callback&scope=openid profile email offline_access
      &code_challenge=…&code_challenge_method=S256&state=…
   → local HTTP server on port 1455 (fallback port on conflict) receives ?code=
   → POST {issuer}/oauth/token (form) grant_type=authorization_code + code_verifier
   → {id_token, access_token, refresh_token}
   issuer = https://auth.openai.com · client_id app_EMoamEEZ73f0CkXaXp7hrann (env-overridable)

B. Custom device flow (codex --device-auth; opencode "headless"; NOT RFC 8628):
   POST {issuer}/api/accounts/deviceauth/usercode {client_id} → {device_auth_id, user_code, interval}
   print "Open {issuer}/codex/device, enter code {user_code} (expires in 15 min)"
   poll POST {issuer}/api/accounts/deviceauth/token {device_auth_id, user_code}   (interval s, 15 min cap)
      → granted: {authorization_code, code_challenge, code_verifier} (server-generated)
      → exchange at the same token endpoint with redirect_uri={issuer}/deviceauth/callback
```

Refresh: `POST {issuer}/oauth/token` (grant_type refresh_token, may rotate). Codex distinguishes permanent refresh failures in user-facing copy: `refresh_token_expired` / `_reused` / `_invalidated` / account-mismatch → "log out and sign in again". 401 recovery = refresh first (single-flight), only re-login when refresh fails. Logout: `POST {issuer}/oauth/revoke` + delete local auth.

### 6.1b P2 RISK — which endpoint does the subscription speak? (spike at P2 start)

Two observed paths, must be verified with a real ChatGPT subscription before committing P2's protocol work:

- **Path 1 (cheap, prefer first): api.openai.com/v1/responses with the OAuth access token.** opencode sends ChatGPT subscription tokens through the standard `@ai-sdk/openai` Responses SDK (baseURL `https://api.openai.com/v1`); its catalog hides only chat-completions-only models. If the public Responses endpoint accepts subscription bearer tokens (possibly with `ChatGPT-Account-ID` / `OAI-Product-Sku` headers), **P2 reuses the P1 openai adapter wholesale** — OAuth is purely additive.
- **Path 2 (expensive): private chatgpt.com/backend-api "codex responses" protocol.** codex CLI itself talks to `{chatgpt_base_url}` (default `https://chatgpt.com/backend-api/`) with `OAI-PRODUCT-SKU: codex` + account headers and a **private SSE format** (response_item.done etc.). If Path 1 fails, P2 needs a third protocol adapter ("codex") against this private endpoint — brittle, reverse-engineered, follow codex's own client as the reference.

**P2 acceptance must include a 0.5-day spike**: login with a real account → hit Path 1 with the openai adapter → record the outcome. Design consequence: `protocol: "openai"` (public) and the future `protocol: "codex"` (subscription, if needed) are separate adapters; the OAuth `TokenProvider` is shared by both.

### 6.2 Token storage (both tools agree; adopt as-is)

| | location | perms | shape |
|---|---|---|---|
| codex | `~/.codex/auth.json` (`$CODEX_HOME/auth.json`) | 0600-ish | `{auth_mode, OPENAI_API_KEY?, tokens:{access_token, refresh_token, id_token{email, chatgpt_plan_type, ...}, account_id}, last_refresh, agent_identity?, personal_access_token?}`; optional OS-keyring backend (feature flag) |
| opencode | `~/.local/share/opencode/auth.json` | **0600** (writeJson mode) | `{provider: {type:"oauth", access, refresh, expires(unix ts), accountId?} \| {type:"api", key}}` |

**bingo: `~/.local/share/bingo/auth.json`, 0600, opencode-compatible shape** — `{provider: {type:"oauth", access, refresh, expires, accountId?} | {type:"api", key}}`. Rationale: (a) both reference tools put tokens in the **user data dir**, never in config; (b) settings.json's project layer is committed — today's `apiKey`-in-settings is exactly the leak OAuth must not repeat; (c) opencode-compatible shape keeps interop open. Keyring backend: **out of scope for v1** (codex makes it a feature flag; opencode doesn't have one; avoid the `keyring` dep per AGENTS.md "no unneeded dependencies") — document the trade-off.

### 6.3 TokenProvider contract (per-provider flows)

`oauth.kind: "codex"` registers both flow implementations (main's ruling: OAuth hard requirement = Codex; opencode-go is API-key):

```rust
pub enum Flow {
    LoopbackPkce,   // default when a local terminal: port 1455 (fallback on conflict), opens browser
    Device,         // headless/SSH: print URL + code, poll (interval, 15 min cap); Esc cancels
}
pub trait OAuthFlow: Send + Sync {
    async fn start(&self) -> Result<FlowPrompt, AuthError>;       // {url | verification_url + user_code, interval}
    async fn poll(&self) -> Result<PollState, AuthError>;         // Pending | Granted(Tokens) | Denied | Expired
}
pub struct Tokens { pub id_token: Option<String>, pub access: String, pub refresh: String, pub expires: i64, pub account_id: Option<String> }
```

`TokenProvider::token()` = cached access → if expired (or 401 on use) → single-flight refresh (`tokio::sync::Mutex`) → persist (0600 atomic write, same convention as the transcript file lock) → return. Refresh failure classes (expired/reused/revoked/mismatch) map to distinct user copy (codex wording), permanent failures clear auth and prompt re-login. **Eager refresh 5 min before expiry** (opencode behavior) plus 401-triggered refresh. Poll UX in the TUI: print URL + code once, spinner while polling (reuse ActivityIndicator), Esc cancels (fires `deny`).

### 6.4 Auth status model (for /provider listing & /login)

Per provider, three sources of truth merge into one status: `NotConfigured` (absent from settings) · `ApiKey` (static key) · `OAuth(LoggedOut | LoggedIn{account, expires_at})` · `OAuth(Expired)` (refresh failed → prompt re-login). `/provider` lists one line per provider: `● codex ✓ ChatGPT Plus (user@x) · [openai]` / `○ codex 未登录（/provider login codex）· [openai]`.

Command surface (added to the existing `/provider`):
- `/provider login <name>` — default loopback PKCE (opens browser, local callback); `--device-auth` for headless/SSH (print URL + code, poll); `--manual` accepts a pasted token (CI fallback)
- `/provider logout <name>` — clear auth.json entry (+ best-effort revoke via `{issuer}/oauth/revoke`)
- `/status` shows the current provider's auth kind; `/provider` listing shows auth status per provider

## 7. Migration & compatibility

1. **Backward compat is the contract**: v1 settings parse identically; default protocol = anthropic; `Client` public surface unchanged for existing callers during phase 1 (refactor is internal).
2. `/provider` listing gains a protocol marker + auth status column: `● road @ https://sub2apis.ruobin.dev/（key sk-8…） [anthropic]`.
3. New capabilities (OAuth login, /login command) default off until configured (D9 "new capabilities default off").
4. Bundled skills `guide.md` config table + capability map updated in the same batch as the settings change (AGENTS.md rule).
5. The OpenAI adapter is usable **before** OAuth lands (API-key providers like `openai` with `protocol: "openai"`); OAuth is additive on top.
6. No migration of stored transcripts/sessions: they store model names only, and the model name is now provider-scoped; cross-provider model reuse already requires explicit model (D29 rule) — unchanged.

## 8. Implementation phases (main's ruling 2026-08: P0+P1 same batch first; P2 immediately after, mandatory)

- **P0+P1 — Contract + Anthropic absorption + OpenAI adapter (ONE batch)**: `api::contract` types, `ProviderClient` trait, registry, move client.rs internals into the anthropic adapter; wire mapping + SSE event mapping + accumulator glue + `count_tokens` fallback + `/model` menu + mock-server tests with Responses SSE fixtures. Zero behavior change for anthropic; existing tests must pass unchanged. **P1 acceptance includes verifying whether api.openai.com/v1/responses requires reasoning items attached to function_call_output — if mandatory, pass back a minimal placeholder (main's ruling; decide by actual API behavior).** Commits: `♻️(refactor): extract provider protocol layer (anthropic adapter)` + `✨(feat): openai responses protocol provider`.
- **P2 — OAuth core (Codex/ChatGPT)**: **0.5-day spike first** — login with a real account, try `api.openai.com/v1/responses` with the OAuth bearer (Path 1, reuses the P1 openai adapter); only if rejected, implement the private `chatgpt.com/backend-api` protocol ("codex" adapter). Then: `TokenProvider` + loopback PKCE + device flow + `/provider login|logout`, auth.json persistence, eager+401 refresh with single-flight, auth status in `/provider`. Commit: `✨(feat): oauth device-flow login (codex)`.
- **P3 — opencode go provider**: named provider + `protocol: "openai"` + `apiKey` (main's ruling: zero OAuth code); verify protocol/base URL from opencode.ai/auth at implementation time. Commit: `✨(feat): opencode go provider`.
- **P4 — UX polish**: protocol badge, auth status columns, error copy for expired tokens, guide.md sync (config table + capability map + diagnostics), README tables.

### 8.1 P1 acceptance checklist (main's requirement #4 — reasoning-return verification, actual API behavior wins)

Recorded at P1 verification time (with a real api.openai.com key):

- [ ] Send a Responses request with `reasoning.effort` set; receive a `reasoning` output item + a `function_call` item in the same turn.
- [ ] Send the follow-up request with ONLY `function_call_output` (reasoning dropped, the v1 plan): does the API accept it, or does it 400/error demanding the reasoning item?
  - If accepted → v1 discard is confirmed; document the finding.
  - If rejected → v1 sends back a minimal placeholder reasoning item (`{"type":"reasoning","id":…,"summary":[],"content":[]}`) alongside `function_call_output`; document the exact shape that works.
- [ ] Record the outcome (accepted / placeholder needed) in the feat commit body + this checklist; it feeds the §6.1b P2 spike decision (whether subscription calls follow the same requirement).

## 9. Testing strategy

- Existing mock-server pattern (raw `TcpListener` + preset SSE, `query.rs`) is reused: add a shared fixture module that serves the same logical turn as **Anthropic SSE** and as **Responses SSE**, and assert both adapters produce the same `StreamEvent` sequence (contract conformance table).
- `contract` unit tests: mapping functions pure (request/response JSON in → out), no HTTP. Explicit P1 contract tests for the three adapter gotchas (§4.2):
  1. **error-flag round-trip**: build a `ToolResult{is_error:true}` → wire encode → decode → assert flag and content survive (both error and success paths).
  2. **index flattening**: a Responses fixture with multiple output items + multi-part message content → assert the emitted `StreamEvent` sequence uses one consistent block index per output item (TextDelta/InputJsonDelta target the right item).
  3. **stop_reason mapping**: `completed`→`end_turn`, `incomplete`(max_output_tokens)→`max_tokens`, `failed`→`ApiError` with detail (never silent `Done`).
- OAuth: mock token endpoint (device code → poll → tokens; refresh); assert single-flight, expiry-driven refresh, 401-triggered refresh, eager refresh 5 min before expiry, permanent-refresh-failure → auth cleared + re-login prompt.
- CI stays `cargo clippy --all-targets -- -D warnings` + `cargo test --locked --bin bingo` (existing workflow, unchanged).

## 10. Open questions (main's rulings logged 2026-08 — remaining items are implementation-time verifications)

**Settled by main:**
1. `apiKey` + `oauth` both set → **apiKey wins + startup warning** (decided).
2. OpenAI adapter default base URL: `https://api.openai.com` (decided).
3. `count_tokens` Unsupported → silent local-estimation fallback + one-time warning, D6 spirit (decided).
4. auth.json: `~/.local/share/bingo/auth.json` + 0600 + opencode-compatible shape (decided).
5. Protocol field naming: **`protocol`** (values `anthropic|openai`), not `type` (decided; avoids confusion with mcpServers `type`).
6. Scope: **P0+P1 one batch first; P2 immediately after (mandatory, part of the user's ask); P3 opencode-go = protocol openai + apiKey, zero OAuth** (decided).
7. OpenAI reasoning: **v1 discard, no verbatim replay**; but P1 acceptance must verify whether the API requires reasoning items on function_call_output — if mandatory, pass back a minimal placeholder (decided, actual API behavior wins).
8. Capability negotiation: v1 static declaration; SSE mapping per verified official docs (decided).

**Implementation-time verifications (not blocking P0/P1):**
9. **P2 spike**: does `api.openai.com/v1/responses` accept the ChatGPT OAuth bearer (opencode's path, reuses P1 adapter), or must bingo implement the private `chatgpt.com/backend-api` codex protocol? (§6.1b — 0.5-day spike at P2 start.)
10. opencode-go protocol/base URL — verify at P3 time (docs point at opencode.ai/auth).

## 11. References

- D1/D6/D9/D10 in notes/research.md
- [`notes/research-oauth-cli.md`](../research-oauth-cli.md) — OAuth CLI research pass (codex 0.146 / opencode source + local installs)
- OpenAI Responses API reference (create, function calling, streaming)
- opencode `packages/opencode/src/auth/index.ts` (auth.json format), `packages/opencode/src/provider/auth.ts` (auth methods)
- Codex CLI OAuth implementation (openai/codex, `codex-rs/login/src/`)

## 12. Decision record (D33 — merged into notes/research.md with P0+P1)

### D33. Provider protocol layer + OAuth access

**Status: confirmed by main (2026-08) — merged into notes/research.md alongside the P0+P1 commits.**

**Problem**: bingo speaks only the Anthropic Messages protocol; each `providers` entry implicitly assumes it. Adding ChatGPT (Codex) subscription access and opencode-go requires (a) a protocol abstraction so a provider can speak the OpenAI Responses protocol, and (b) an OAuth path so subscriptions work without an API key.

**Decisions**:

1. **Protocol abstraction is contract-first**: new `api::contract` module with neutral serde types (`NeutralRequest`/`NeutralMessage`/`StreamEvent`, `Capabilities`, `ThinkingLevel`) and a `ProviderClient` trait (`stream`/`complete_text`/`list_models`/`count_tokens`/`auth_status`). `Client` keeps its public surface; internally it holds `Arc<dyn ProviderClient>` per named provider. AGENTS.md "contract first" applies: settings schema, neutral types, and auth.json shape are the three contracts.
2. **Anthropic adapter absorbs, never rewrites**: `api/client.rs` internals move into `api/providers/anthropic.rs`; retry/backoff/timeouts/400-overflow/SSE/error mapping are byte-identical; existing tests pass unchanged before anything new lands.
3. **OpenAI adapter speaks the Responses API** (`POST {base}/v1/responses`, default `https://api.openai.com`, `Authorization: Bearer`): mapping table in §4.2 (system→instructions, messages→input items, tools→function tools, thinking→`reasoning.effort`, max_tokens→max_output_tokens). SSE event mapping verified against official docs; `function_call`/`function_call_output` are the tool protocol; reasoning summaries map to the existing thinking UI affordance; reasoning is never replayed verbatim.
4. **Settings v2 is additive**: `ProviderConfig` gains optional `protocol` (`anthropic` default — every existing config parses unchanged), `oauth`, `capabilities` overrides. `apiKey` + `oauth` both set → apiKey wins + startup warning. Unknown protocol → `CONFIG_INVALID` at startup.
5. **OAuth target = Codex/ChatGPT only in v1**; device-flow + loopback PKCE both implemented (codex client_id `app_EMoamEEZ73f0CkXaXp7hrann`, issuer `https://auth.openai.com`, refresh `…/oauth/token`, revoke `…/oauth/revoke`). Tokens live in `~/.local/share/bingo/auth.json` (0600, opencode-compatible `{provider: {type:"oauth", access, refresh, expires, accountId?}}` shape) — **user data dir, never the committed project settings**. Eager refresh 5 min before expiry + 401-triggered refresh, single-flight; permanent refresh failures clear auth and prompt re-login. No keyring backend in v1 (avoid the dep; documented trade-off).
6. **opencode-go is an API-key subscription, not OAuth** (research correction): lands as `protocol: "openai"` + `apiKey` named provider; zero OAuth code; endpoints verified at implementation time.
7. **P2 starts with a 0.5-day spike**: does `api.openai.com/v1/responses` accept the ChatGPT OAuth bearer (reuses the P1 adapter wholesale), or must bingo implement the private `chatgpt.com/backend-api` codex protocol (separate adapter)? The `TokenProvider` is shared either way.
8. **Capabilities are statically declared in v1** (`Capabilities` from protocol defaults + per-provider overrides); no runtime negotiation round-trip.
9. **`count_tokens` is Anthropic-only**: OpenAI adapter returns `Unsupported`; the token gate falls back to local estimation with a one-time warning (D6 spirit).
10. **Scope guardrails**: new capabilities default off (D9); bundled `guide.md` config table + capability map updated in the same batch (AGENTS.md); no keyring/sandbox/telemetry additions (D13); cross-provider explicit-model rule for subagents is unchanged (D29).
