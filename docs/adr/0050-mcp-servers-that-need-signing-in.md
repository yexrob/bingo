# ADR-0050 — An MCP server that needs signing in

Status: accepted · 2026-09-08 · Plan: M85

## Context

An HTTP MCP server may answer `401` and want an OAuth 2.1 bearer, as the
MCP authorization specification (2026-07-28; 2025-06-18 before it) lays
out: RFC 9728 resource metadata, RFC 8414 server metadata, PKCE S256, RFC
8707 `resource`, a registered client. bingo dials HTTP servers with static
`headers` (M7) and has one OAuth library, `bingo-auth-oauth`, built for
providers (ADR-0012): PKCE, a loopback callback, `auth.json`, refresh, and
login as an interaction. M7 deferred MCP auth to M10; M10 never returned.

No kernel door is asked for. `mcp` stays a plugin noun, the store stays
the library's, and the kernel's `/login` stays the providers'.

## Decision

1. **The library learns the resource-server side.** `bingo-auth-oauth`
   gains discovery (challenge → protected-resource metadata → server
   metadata, the document's `issuer` checked against the URL it came
   from; no S256, no login), RFC 7591 registration as a native client,
   and a flow that sends `resource` on authorize, token and refresh. The
   codex issuer keeps its static shape; an issuer may now carry absolute
   endpoints. Any plugin may use it, as any may use the store.
2. **One entry per server, in the one store.** `auth.json` holds
   `Entry::McpOAuth { issuer, client_id, client_secret?, redirect_uri,
   access, refresh?, expires, scope? }` under `mcp:<server name>`: the
   registration and the tokens are one fact about one server, and a
   registration whose `issuer` differs from a fresh discovery is redone.
   A token never enters a settings file (ADR-0012 §2 stands) and never
   enters the `mcp.servers` rows (ADR-0036 forwards them verbatim to
   foreign agents, which dial with their own auth).
3. **A fourth state, not a string.** `State::NeedsAuth { why }` beside
   `Failed`: a dial of an HTTP server with no static `Authorization` that
   gets a `401` — with no entry, or after one refresh — is *needs
   authentication*, and `/mcp` says so. A static `Authorization` that
   gets a `401` is `Failed`. The bearer is put into the dial's headers
   from the store, fresh or refreshed single-flight; a mid-session `401`
   refreshes and redials once on its own task.
4. **The verbs live where the servers do.** `/mcp login <server>` runs the
   flow through the command's prompter (`instant: false`, as `/login`),
   `/mcp logout <server>` revokes and removes, `/mcp tools <server>` lists;
   `reconnect | enable | disable` stand. The table gains an `auth` column.
   `bingo mcp list | get | add | remove | login | logout` is the headless
   twin: `add`/`remove` write the user settings layer through the kernel's
   `remember`, `get` prints header and env *names*, `login` runs the
   `Terminal` prompter, `--paste` accepts the callback URL by hand.
5. **Client ID Metadata Documents are not done.** They need a document
   served at an HTTPS URL bingo does not have. Registration (deprecated in
   2026-07-28, kept for compatibility) and a configured `clientId` cover
   the servers of today; the day a server offers only CIMD, this is the
   line to revisit.

## Consequences

- `bingo-mcp` depends on `bingo-auth-oauth` (library tier; ADR-0012 §1).
  No new external dependency is expected; any is a budget line.
- A print-surface `/mcp login` is refused in words (the surface renders
  no `Login`); `bingo mcp login` is the headless path.
- The redirect URI registered with a server names a port; the next login
  binds that port first and re-registers when it cannot.
- A server on the 2025-06-18 revision sends no `resource_metadata`; the
  well-known ladder finds it.

## Supersedes

Amends M7's deferral ("OAuth for HTTP servers — M10 owns auth"). Refs:
ADR-0012, ADR-0036 §4, ADR-0046.
