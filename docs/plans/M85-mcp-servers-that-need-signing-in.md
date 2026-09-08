# M85 — MCP servers that need signing in

## Goal

User, 2026-09-08: an HTTP MCP server that wants a login — `binlesson` at
`https://binlesson.ruobin.dev/api/mcp` — cannot be used from bingo. Claude
Code's `/mcp` shows it as `△ needs authentication`, offers *Authenticate /
Re-authenticate / Clear authentication / Reconnect / Disable*, and has
`claude mcp add|get|remove|list|login|logout`. bingo does the same in its
own grammar: `/mcp` and a `bingo mcp` subcommand family, OAuth 2.1 as the
MCP authorization spec (2026-07-28, back-compatible with 2025-06-18) says.

M7 deferred this ("OAuth for HTTP servers — M10 owns auth"); M10 built
`bingo-auth-oauth` for providers and never came back. The pieces are here:
PKCE S256, a loopback callback server, `auth.json` at mode 0600, single-
flight refresh with a 300 s lead, `InteractionKind::Login`, the shared
browser opener (ADR-0046); `bingo-mcp` already dials HTTP with `headers`
and has `Connecting | Connected | Failed | Disabled` behind `/mcp`.
Missing: discovery, client registration, a typed *needs authentication*,
a bearer at dial, the verbs, and the CLI. ADR-0050 records the boundaries.

## Bricks, in build order (contracts first, each with its fixture test)

**Library tier — `bingo-auth-oauth`** (no plugin import; `bingo-mcp` may
depend on it, as `bingo-provider-openai` does)

1. **`challenge`** (pure): parse a `WWW-Authenticate: Bearer …` value into
   `{ resource_metadata?, scope?, error? }`. **`probe`** (one request):
   POST a well-formed `initialize` to the server URL with the static
   headers and no token; answer `Ok(())`, `Unauthorized(Challenge)` on
   401, or the error. This is how a dial-time 401 becomes a fact.
2. **`discover`**: RFC 9728 protected-resource metadata — the challenge's
   URL, else `/.well-known/oauth-protected-resource<path>`, else the root
   — then `authorization_servers[0]`; then RFC 8414 / OIDC metadata over
   the spec's URL ladder (path-inserted first). **The document's `issuer`
   must equal the issuer the URL was built from, or it is rejected.** Refuse
   an AS whose `code_challenge_methods_supported` lacks `S256`. Yields an
   `Issuer` with absolute endpoints (`authorization`, `token`,
   `registration?`, `revocation?`), `scopes_supported`, and the PRM
   `resource` string. `Issuer` grows to hold absolute endpoints; the codex
   issuer keeps working.
3. **`register`** (RFC 7591): `client_name: "bingo"`, `application_type:
   "native"`, `redirect_uris` for the loopback callback (both `localhost`
   and `127.0.0.1`), `grant_types: [authorization_code, refresh_token]`,
   `token_endpoint_auth_method: none`. A configured `clientId`
   (`mcpServers.<name>.oauth.clientId`) wins over registration; no
   `registration_endpoint` and no `clientId` is a permanent, worded error.
4. **The flow**: authorize URL with `code_challenge` S256, `state`,
   `redirect_uri`, `resource` (RFC 8707: the server URL, fragment stripped,
   path kept — the PRM `resource` verbatim when it covers the URL), `scope`
   (challenge → `scopes_supported` → none); the callback checked for
   `state` and, when `iss` is present, exact-matched to the issuer; the
   token request with `code_verifier`, `redirect_uri`, `resource`. Reuses
   `redirect.rs` (`bind`, `receive`) and `browser::open`. Refresh sends
   `resource` too.
5. **The entry**: `Entry::McpOAuth { issuer, client_id, client_secret?,
   redirect_uri, access, refresh?, expires, scope? }` under the key
   `mcp:<server name>` in `auth.json`. One entry per server; a registration
   whose `issuer` differs from a fresh discovery is dropped and redone. A
   `TokenSource`-shaped handle for MCP: `status()`, `access_token()` (fresh
   or refreshed, single-flight), `login(prompter, method)`, `logout()`
   (revoke when there is a revocation endpoint, then remove).

**Plugin — `bingo-mcp`**

6. **State**: `State::NeedsAuth { why }` beside `Failed`; `Status` and the
   `/mcp` table say `needs authentication`. A dial of an HTTP server with
   no static `Authorization` header: a stored entry puts `Authorization:
   Bearer <fresh access>` into the dial's headers (never into settings,
   never into the `mcp.servers` rows — ADR-0036 forwards rows verbatim); a
   401 with no entry, or after one refresh, is `NeedsAuth`. A tool call
   that comes back 401 refreshes and redials once on its own task, then
   `NeedsAuth`. A static `Authorization` that gets 401 is `Failed`, as
   Claude Code reports it.
7. **`/mcp` verbs**: `login <server>` (authenticate and re-authenticate;
   `instant: false`, the flow through the command's prompter as `/login`
   does; on success, reconnect), `logout <server>` (clear: revoke, remove,
   reconnect → `needs authentication`), `tools <server>` (a table: name,
   the first line of the description), and `reconnect | enable | disable`
   as today. Bare `/mcp`: the table gains an `auth` column — `-`, `signed
   in`, `needs authentication`, `expired`. The hint names every verb.
8. **`bingo mcp` CLI** in `crates/bingo/src/main.rs`, one module `mcp.rs`:
   `list`, `get <name>`, `add [-t http|stdio] [-H "K: V"]… [-e K=V]…
   <name> <url | command [args…]>`, `remove <name>`, `login <name>
   [--paste]`, `logout <name>`. `add`/`remove` write the user settings
   layer through the kernel's own `remember`, never a project file; `get`
   prints header names and env names, never values. `login` is headless
   through the `Terminal` prompter of `crates/bingo/src/login.rs`;
   `--paste` prints the URL and accepts the pasted callback URL or code.
9. **Black boxes**: wiremock AS + resource server for the library flow end
   to end (the test itself follows the redirect to the loopback); manager
   transitions with a fake dial (`NeedsAuth` on 401, bearer present when
   an entry is stored, redial-once on a mid-session 401); command grammar;
   CLI `add → get → list → remove` against a temp home, stdout purity,
   exit codes; `/mcp` with a `needs authentication` row.

## Files

- `crates/bingo-auth-oauth/src/{challenge,discover,register,mcp}.rs`,
  `issuer.rs`, `store.rs`, `tokens.rs`
- `crates/bingo-mcp/src/{manager,dial,command,config,auth}.rs`
- `crates/bingo/src/{main,mcp}.rs`, `crates/bingo/tests/cli/mcp.rs`
- `docs/adr/0050-mcp-servers-that-need-signing-in.md`, `docs/adr/README.md`

## Exit criteria

- [ ] discovery, registration, authorize/token/refresh bodies pinned by
      fixtures; the issuer check and the S256 refusal each have a test
- [ ] a wiremock AS signs bingo in end to end and the token lands under
      `mcp:<name>` at mode 0600; `logout` revokes and removes it
- [ ] `/mcp` shows `needs authentication`; `/mcp login <name>` signs in and
      reconnects; `/mcp logout`, `/mcp tools`; the print surface refuses
      `login` in words
- [ ] `bingo mcp list|get|add|remove|login|logout` black-boxed
- [ ] no token in any log, `Debug`, stdout, settings file or forwarded row
- [ ] `cargo deny check`, `scripts/budget.sh` with a line for any dependency
- [ ] every gate green; Windows check for the touched crates

## Non-goals

- Client ID Metadata Documents: they need a document hosted at an HTTPS
  URL bingo does not have. DCR (deprecated in 2026-07-28, kept for
  compatibility) and a configured `clientId` cover the servers of today.
- An OS keychain: `auth.json` at 0600 is ADR-0012's store, and stays.
- Scope step-up on 403 `insufficient_scope`: the verb `login` re-runs the
  flow; the union-of-scopes retry can come when a server asks for it.
- SSE transport, `headersHelper`, a project-scoped `add`: not asked for.
- A TUI panel for servers: `/mcp` answers a `View::Table`, as today.

## Risks

- R-rmcp: `rmcp` fixes headers at dial and may not expose the 401's
  headers; the probe brick reads them with our own client, and a refresh
  is a redial. If a mid-session 401 is not distinguishable in `rmcp`'s
  error, the redial-once falls back to a substring match and says so.
- R-port: the registered redirect URI names a port; the next login binds
  that port first and re-registers when it cannot.
- R-discovery: servers on the 2025-06-18 revision omit `resource_metadata`
  from the challenge; the well-known ladder covers them.
