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
   in`, `expired`. The hint names every verb.
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

- [x] discovery, registration, authorize/token/refresh bodies pinned by
      fixtures; the issuer check and the S256 refusal each have a test
- [x] a wiremock AS signs bingo in end to end and the token lands under
      `mcp:<name>` at mode 0600; `logout` revokes and removes it
- [x] `/mcp` shows `needs authentication`; `/mcp login <name>` signs in and
      reconnects; `/mcp logout`, `/mcp tools`; the print surface refuses
      `login` in words
- [x] `bingo mcp list|get|add|remove|login|logout` black-boxed
- [x] no token in any log, `Debug`, stdout, settings file or forwarded row
- [x] `cargo deny check`, `scripts/budget.sh` with a line for any dependency
- [x] every gate green; Windows check for the touched crates

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

## Verified (2026-09-08, commit 148e94a2 + this)

```
$ cargo fmt --all -- --check                                        exit 0
$ cargo check --workspace --all-targets --locked                    exit 0
$ cargo clippy --workspace --all-targets --locked -- -D warnings    exit 0
$ cargo test --workspace --locked --no-fail-fast                    exit 0 — 4387 passed, 0 failed
  new: auth-oauth 52 → 93 (challenge 8, discover 12, register 5, mcp 9, callback +3,
       redirect +2, store +2, issuer +2) · mcp 61 → 78 lib + 6 tests/signing_in · bin cli mcp 10
$ scripts/check_discipline.sh                                       exit 0
  dependency direction ok — a library may be depended on by a plugin, so bingo-mcp → bingo-auth-oauth passes
  size warnings unchanged in kind; crates/bingo/src/main.rs 821 → 846 non-test lines (the `mcp`
  subcommand's wiring; its clap shape lives in crates/bingo/src/mcp.rs, not beside it)
$ scripts/budget.sh                                                 dependencies 335 (max 335, unchanged)
  No crate was added: reqwest, serde, wiremock and rmcp were all in the tree already.
$ cargo deny check                                                  advisories ok, bans ok, licenses ok, sources ok
$ cargo check -p bingo-auth-oauth -p bingo-mcp --all-targets \
    --target x86_64-pc-windows-msvc --locked                        exit 1 — aws-lc-sys will not cross-build here
  Not this milestone's: `reqwest` already resolves `aws-lc-sys` in both crates' trees, so neither
  could be cross-checked on this box before M85 either. The toolchain itself is fine —
  `cargo check -p bingo-loopback --target x86_64-pc-windows-msvc` exits 0 — and nothing added here
  is platform-gated: no process, path, signal or clock. CI's `windows` job is the backstop.
```

`cargo test --workspace` failed once on `pty::esc_twice_rewinds_the_turn_and_the_file_it_wrote`,
a TUI rewind test M85 touches nothing of; it passed three times alone, the whole `--test pty`
suite passed alone (16/16), and the next full workspace run was 4387/0. Recorded as a
load-sensitive flake, not a regression.

Exit criteria, item by item:

- [x] **Discovery, registration and the bodies are pinned by fixtures.** `issuer.rs` pins both
      authorize URLs as literals (codex's unchanged, the discovered one with `resource` and no
      `scope`); `register.rs` pins the RFC 7591 body as a literal and matches it on the wire with
      `body_json`; `discover.rs` pins both well-known ladders (the path *inserted* after the
      well-known segment, the RFC 8414/OIDC order) as pure tests. The issuer echo check has
      `a_document_that_calls_itself_something_else_is_refused`; the S256 refusal has
      `a_server_without_s256_is_refused_in_words`, over both `["plain"]` and the field absent.
- [x] **A wiremock AS signs bingo in end to end; the token lands under `mcp:<name>` at 0600;
      `logout` revokes and removes.** `crates/bingo/tests/cli/mcp.rs::a_pasted_code_signs_in_and_the_token_lands_in_auth_json`
      drives the real binary: `bingo mcp add`, then `bingo mcp login remote --paste` against a
      wiremock resource server + AS (401 → PRM → AS metadata → registration → token). It asserts
      the `mcpOauth` entry under `mcp:remote`, mode 0600, and that neither token is in the
      settings file; then `bingo mcp logout remote` empties the entry and the AS saw one
      `/revoke`. The library has the same flow again at unit level (`mcp.rs`), plus the
      single-flight renewal (eight callers, two `/token` requests), the retired-refresh-token
      path, and the two forgeries (a wrong `state`, a foreign `iss`).
- [x] **`/mcp` shows `needs authentication`; the verbs work; print refuses `login` in words.**
      `tests/signing_in.rs` dials a scripted streamable-HTTP server: no entry → `NeedsAuth` and
      an `auth` column reading a dash (the status column already says it); a stored entry → the bearer on the wire
      and `Connected`; a mid-session `401` → the tool call fails, the manager renews and redials
      on its own task, and `at_2` is written back; a renewal that fails → `NeedsAuth`; a person's
      own `Authorization` → `Failed`, never a sign-in. `/mcp tools` is
      `a_connected_server_lists_what_it_offers`. The print surface is
      `the_print_surface_refuses_a_sign_in_and_names_the_way_through` — one `[error]` line naming
      `bingo mcp login remote`, and nothing stored.
- [x] **`bingo mcp list|get|add|remove|login|logout` black-boxed.** Ten cases in
      `crates/bingo/tests/cli/mcp.rs`: add → get → list → remove against a temp home; a stdio
      server joining neighbours already in the file with `model` untouched; `get` printing
      `Authorization` and `GITHUB_TOKEN` but neither value; four verbs on an unknown name as one
      `[error] code=INVALID_INPUT` line with empty stdout and exit 1; a row the plugin would
      refuse refused before anything is written; a child process refused a sign-in;
      `--mcp-config` listed because it is what the next run would dial.
- [x] **No token in a log, a `Debug`, stdout, a settings file or a forwarded row.** `McpAuth`
      and `Manager` print names and endpoints only (`nothing_of_the_credential_reaches_a_debug_line`);
      the bearer is put into the *dial's* headers by `auth::bearing`, which is tested to leave
      the configured map untouched — so `mcp.servers` rows stay the person's own (ADR-0036 §4);
      the dial's failure strings are `redact`ed and asserted not to contain the bearer; the
      end-to-end CLI test asserts no token on stdout, on stderr, or in the settings.
- [x] **`cargo deny check`, `scripts/budget.sh`.** Both above. No dependency line was needed:
      nothing was added.
- [x] **Every gate green; the Windows check attempted.** Above, with the `aws-lc-sys` note.

### What differed from the plan

1. **The dial reads its `401` from rmcp, not from the probe.** R-rmcp expected `rmcp` to hide the
   `401`; rmcp 3.1.4 exposes it — `ClientInitializeError::is_authorization_required()` and, for a
   call in flight, `AuthRequiredError` inside the public `DynamicTransportError::error`. Both are
   read by type in `bingo-mcp/src/auth.rs`; the substring fallback the plan allowed was not
   needed and is not there. The probe brick still exists and still earns its place: `login` uses
   it to get the challenge before any connection exists, which is how `bingo mcp login` works
   with no dial at all. **Caveat, recorded:** rmcp raises `AuthRequiredError` only when the `401`
   carries a `WWW-Authenticate` header. MCP requires one, and the tests send one; a server that
   sends a bare `401` reads as `Failed` at dial time. `/mcp login` and `bingo mcp login` still
   work for it, because discovery falls back to the well-known ladder.
2. **`/mcp` is no longer instant.** `instant` is a property of the command, not of one verb, and
   the plan asks for `instant: false` on `login`. So the whole command holds the queue, as
   `/login` does. The cost is that a bare `/mcp` typed during a turn now waits for it.
3. **`Issuer` grew a `form_encoded` flag.** RFC 6749 §6 and RFC 7009 want a form; codex answers
   its refresh and its revocation in JSON, and M10's Verified says that stays until a live
   refresh says otherwise. Written down as one issuer's quirk rather than guessed from another
   field. `revoke_path` and the three device paths also became optional (the latter grouped into
   `Issuer::device`), because a discovered issuer has neither — the old shape could not express
   an issuer without a device flow.
4. **`refreshed()` takes the bearer that bounced.** A forced renewal cannot key on the expiry —
   the whole point is that the clock said *fresh* and the server said no. Passing the refused
   token is also what keeps it single-flight, with no second generation counter.
5. **`mcpServers.<name>.oauth` holds only `clientId`.** A `clientSecret` in a settings file is a
   credential in a file a project layer commits (ADR-0012 §2); a public native client has none,
   and a secret an AS issues during registration is kept in `auth.json` where it belongs.
6. **`bingo mcp`'s clap shape lives in `crates/bingo/src/mcp.rs`.** The first cut had a clap enum
   in `main.rs` and a twin enum in `mcp.rs` with a translation between them — two representations
   of one fact. There is now one, and `main.rs` keeps five lines of wiring.
7. **A misplaced `-H`/`-e` is refused rather than dropped.** Writing `bingo mcp add -H "X: y"
   files npx` used to write a stdio row with the header silently gone; it now says which
   transport a header belongs to.
8. **`callback::parse` reads `iss` (RFC 9207) and an `error=` refusal**, and `redirect::receive`
   answers with the whole `Callback` rather than the code alone — the codex flow takes `.code`
   and is otherwise unchanged. The plan's "reject when the metadata advertises
   `authorization_response_iss_parameter_supported` and `iss` is absent" was **not** implemented:
   the plan's own wording is "when `iss` is present, exact-matched to the issuer", and that is
   what is there.

### Not done, and why

- **A live drive against `https://binlesson.ruobin.dev/api/mcp`** — the server that prompted this
  milestone. Everything here is proved against wiremock and a scripted streamable-HTTP endpoint;
  a real sign-in needs the user's own browser and account, and a worker must not open one. This
  is the one thing left before the milestone can be called finished in the world rather than in
  the suite.
- **No TUI work.** `/mcp` answers a `View::Table` with a fourth column and that is the whole of
  the surface change; `docs/design/tui.md` and the TUI crate are untouched, as the brief says.
- **Scope step-up on `403 insufficient_scope`** stays a non-goal: `challenge::probe` reads only
  `401`, and a `403` is a plain failure with a test that says so.

### Review and live drive (2026-09-08, after the merge into `dev`)

Three things the review changed. **A token the issuer gave no lifetime for
read as stale** (`Tokens::is_fresh` on `expires_at: None`), so every dial
renewed it and one with no refresh token was retired the moment it was won;
`McpAuth::access_token` now sends it until the server refuses it, and the
scope the person consented to is stored. **The ask door refused the login
in a bypass session** — `HostHandle::ask` weighed the policy's stance on an
`InteractionKind::Login`, which names no allowing option, and rejected it
with *this question names no allowing option*; the door now weighs the
stance only for a question or a form (ADR-0039, dated note), so `/mcp
login` asks as `/login` does. **The `auth` column said *needs
authentication* for every HTTP server nobody had signed in to**, deepwiki
included, which never asks for one; a signed-out server reads as a dash
and the status column keeps the word.

Live, against the user's own settings (ten servers) in a harness-owned
tmux pane, `BINGO_NO_BROWSER=1`:

```text
server                    status                              tools  auth
binlesson                 needs authentication                    –  -
cloudflare                needs authentication                    –  -
deepwiki                  connected                               3  -
dokploy                   connected                              67  -
laplace                   connected                              18  -
```

`/mcp tools laplace` listed eighteen tools with their first lines; `/mcp
login binlesson` opened the *Sign in to binlesson* dialog with the real
authorize URL — `binlesson.ruobin.dev/api/auth/oauth2/authorize`, a client
id the AS registered on the spot, `code_challenge_method=S256`, `resource=
https://binlesson.ruobin.dev/api/mcp`, `state` — and `esc` cancelled it.
`bingo mcp login binlesson --paste < /dev/null` printed the same URL and
refused the empty paste. The browser consent itself is the user's step and
was not driven. Carried: each cancelled login leaves one dynamic
registration behind on the AS, since the client is written only with its
tokens; and with ten servers dialled at once, one or two hit the dial's 5 s
connect timeout on each start — older than this milestone.
