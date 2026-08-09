# CLI OAuth research notes: Codex CLI × opencode (reference for the bingo OAuth design)

> Research date 2026; source code taken from the GitHub main branches (openai/codex `main`, sst/opencode `dev`). Hard facts follow the source code.

## 1. Codex CLI — ChatGPT OAuth

**Login method**: `codex login` = authorization code + PKCE + **loopback** (starts a local HTTP server that waits for the callback; default port 1455, falls back to another port if occupied); `codex login --device-auth` = a **custom device flow** (not RFC 8628).

**client_id**: `app_EMoamEEZ73f0CkXaXp7hrann` (overridable via env `CODEX_APP_SERVER_LOGIN_CLIENT_ID`).

**Endpoints** (issuer = `https://auth.openai.com`):
- authorize: `GET {issuer}/oauth/authorize?response_type=code&client_id=…&redirect_uri=http://localhost:{port}/auth/callback&scope=openid profile email offline_access api.connectors.read api.connectors.invoke&code_challenge=…&code_challenge_method=S256&id_token_add_organizations=true&codex_cli_simplified_flow=true&state=…&originator=…`
- token exchange: `POST {issuer}/oauth/token` (form-encoded) `grant_type=authorization_code&code=…&redirect_uri=…&client_id=…&code_verifier=…` → `{id_token, access_token, refresh_token}`
- device flow (custom): `POST {issuer}/api/accounts/deviceauth/usercode` body `{"client_id"}` → `{device_auth_id, user_code, interval}`; poll `POST …/deviceauth/token` `{"device_auth_id","user_code"}` (403/404 = pending, 15-minute timeout); on success returns **server-generated** `{authorization_code, code_challenge, code_verifier}`, then exchanges via the same token endpoint (redirect_uri = `{issuer}/deviceauth/callback`); the browser opens `{issuer}/codex/device` to enter the code.

**Token storage**: `$CODEX_HOME/auth.json` (default `~/.codex/auth.json`; optional keyring):
```json
{"auth_mode":"chatgpt",
 "tokens":{"id_token":"<JWT>","access_token":"<JWT>","refresh_token":"…","account_id":"…"},
 "last_refresh":"2026-…Z"}
```
The id_token is a JWT carrying a `https://api.openai.com/auth` claim (chatgpt_plan_type/user_id/account_id); at startup it is parsed to determine the plan type and account.

**refresh**: `POST https://auth.openai.com/oauth/token` JSON `{"client_id","grant_type":"refresh_token","refresh_token"}` → `{id_token?, access_token?, refresh_token?}` (may rotate); error codes `refresh_token_expired / _reused / _invalidated` = permanent failure → prompt re-login. On 401 it goes through the UnauthorizedRecovery state machine: refresh and retry first → only prompt re-login if that still fails.

**API routing**: the ChatGPT subscription does **not** go through `api.openai.com`; it uses `https://chatgpt.com/backend-api/` (the chatgpt_base_url default): `Authorization: Bearer <access_token>` (newer versions upgrade to a task JWT issued by agent-identity) + `ChatGPT-Account-ID` + `OAI-Product-Sku: codex` headers; the SSE protocol is the private codex responses format (response_item.done etc.), not the public Responses API.

Sources: [device_code_auth.rs](https://github.com/openai/codex/blob/main/codex-rs/login/src/device_code_auth.rs) · [server.rs](https://github.com/openai/codex/blob/main/codex-rs/login/src/server.rs) · [auth/manager.rs](https://github.com/openai/codex/blob/main/codex-rs/login/src/auth/manager.rs) · [auth/storage.rs](https://github.com/openai/codex/blob/main/codex-rs/login/src/auth/storage.rs) · [chatgpt/chatgpt_client.rs](https://github.com/openai/codex/blob/main/codex-rs/chatgpt/src/chatgpt_client.rs) · [core/src/config/mod.rs](https://github.com/openai/codex/blob/main/codex-rs/core/src/config/mod.rs)

## 2. opencode (sst/opencode) OAuth

**Commands**: after the v2 refactor it is `opencode console login [url]` (formerly `opencode auth login`), plus `console logout/switch/orgs/open`. The opencode subscription uses a **self-hosted auth server** `https://console.opencode.ai` (a custom server URL can be passed).

**OAuth flow (standard RFC 8628 device flow)**, client_id = `opencode-cli`:
- `POST /auth/device/code` `{"client_id":"opencode-cli"}` → `{device_code, user_code, verification_uri_complete, expires_in, interval}`
- The browser opens `verification_uri_complete` (the user_code is already embedded)
- Poll `POST /auth/device/token` `{"grant_type":"urn:ietf:params:oauth:grant-type:device_code","device_code","client_id"}` → `{access_token, refresh_token, token_type:"Bearer", expires_in}` or `{error, error_description}` (authorization_pending / slow_down / expired_token / access_denied)
- On success GET `/api/user` (id/email), `/api/orgs`, `/api/config` (request header `x-org-id`) for account/org configuration.

**refresh**: `POST /auth/device/token` `{"grant_type":"refresh_token","refresh_token","client_id"}`; **eager refresh 5 minutes before expiry**.

**Token storage**: `$XDG_DATA_HOME/opencode/auth.json` (macOS = `~/Library/Application Support/opencode/auth.json`), 0600, keyed by provider ID:
```json
{"opencode":{"type":"oauth","access":"…","refresh":"…","expires":1730000000000,"accountId":"…"},
 "anthropic":{"type":"api","key":"sk-…"}}
```
The env var `OPENCODE_AUTH_CONTENT` can inject the whole document.

**Provider abstraction**: providers declare `methods: [{type:"oauth"|"api",label,prompts}]`; authorize returns `{url, method:"auto"|"code", instructions}` (auto = open the browser + poll in the background; code = paste the code back manually); the callback writes the auth store. Multiple providers coexist, and multi-account switching (account table + active flag) is granular down to the org.

Sources: [packages/opencode/src/auth/index.ts](https://github.com/sst/opencode/blob/dev/packages/opencode/src/auth/index.ts) · [packages/opencode/src/account/account.ts](https://github.com/sst/opencode/blob/dev/packages/opencode/src/account/account.ts) · [packages/opencode/src/cli/cmd/account.ts](https://github.com/sst/opencode/blob/dev/packages/opencode/src/cli/cmd/account.ts) · [packages/core/src/plugin/provider/opencode.ts](https://github.com/sst/opencode/blob/dev/packages/core/src/plugin/provider/opencode.ts)

## 3. OpenAI Responses API (reference for the bingo implementation)

- `POST https://api.openai.com/v1/responses`, `Authorization: Bearer <key>` (optionally `OpenAI-Organization` / `OpenAI-Project`).
- Request body essentials: `{model, instructions?, input:[{role,content}], tools:[{type:"function",name,description,parameters,strict?}], tool_choice?, stream:true, reasoning:{effort:"low"|"medium"|"high"}, text:{format:{type:"text"|"json_schema",…}}, store?}`.
- SSE event sequence:
  - Overall lifecycle: `response.created` → `response.in_progress` → … → `response.completed` (usage appears only here; failures are `response.failed`)
  - Tool calls (the Anthropic tool_use counterpart): `response.output_item.added` (item.type=`function_call`, with id) → `response.function_call_arguments.delta` (**arguments are incremental JSON-string deltas; must be accumulated until `.done`**) → `response.output_item.done`
  - Text: `response.output_item.added` (output_text) → `response.output_text.delta` → `response.output_text.done` → `response.output_item.done`
  - Every content stream is an add/delta/done triple; the final text lives in the done event.
- Model names: `gpt-5` / `gpt-5-mini` / `o3` / `o4-mini` / `gpt-5-codex` etc.

Sources: [Responses streaming events](https://developers.openai.com/api/reference/resources/responses/streaming-events) · [Responses API reference](https://developers.openai.com/api/reference/resources/responses) · [community streaming guide](https://community.openai.com/t/responses-api-streaming-the-simple-guide-to-events/1363122)

## 4. CLI-layer UX comparison (login/status/switch/logout)

| Dimension | Codex CLI | opencode |
|---|---|---|
| Login | `codex login` auto-opens the browser + loopback callback; `--device-auth` prints "1. open the link 2. enter the one-time code (15 min)"; `--api-key` | `opencode console login [url]`: prints `Go to: <url>` + `Enter code:` → auto-opens the browser → spinner "Waiting for authorization…" → `Logged in as <email>` |
| Status | at startup AuthManager reads auth.json and parses the id_token (plan type/account), restricting login method and workspace | `opencode console orgs` lists accounts+orgs, active marked with ● |
| Multi-account/switch | single identity is primary; `forced_chatgpt_workspace_id` whitelist restricts | `console switch` picker across accounts+orgs; `logout [email]` logs out per account; active flag in DB |
| logout | `codex logout`: revokes the token (`POST {issuer}/oauth/revoke`) + deletes auth.json | `console logout [email]`: selects the account and deletes the local record (no revoke call) |
| fallback | headless/remote use `--device-auth`; loopback port conflicts auto-switch ports | device flow is headless-friendly by nature; `open()` failure is silent, prompting the user to open the URL manually |

**Lessons for bingo**: ① two reusable templates — Codex's "auth.openai.com + custom deviceauth" and opencode's "self-hosted server + standard RFC 8628 device flow + refresh rotation"; ② the simplest storage is one 0600 JSON keyed per provider as `{provider_id: {type, access, refresh, expires}}`; ③ copy the opencode login UX: print URL + code → auto-open browser → spinner polling → echo the email on success; ④ 401 recovery = silent refresh first (including eager refresh before expiry) and only then ask for re-login.

**Sources**:
- [openai/codex — login source](https://github.com/openai/codex/tree/main/codex-rs/login)
- [sst/opencode — auth/account source](https://github.com/sst/opencode/tree/dev/packages/opencode/src)
- [Responses streaming events | OpenAI API Reference](https://developers.openai.com/api/reference/resources/responses/streaming-events)
- [Responses | OpenAI API Reference](https://developers.openai.com/api/reference/resources/responses)
- [Responses API streaming - the simple guide to "events"](https://community.openai.com/t/responses-api-streaming-the-simple-guide-to-events/1363122)
- [Streaming — OpenAI Agents SDK](https://openai.github.io/openai-agents-python/streaming/)
