# M54 — The page that answers back

## Goal

The model can put a page in front of the person — a layout to compare,
a wizard, a form richer than four options — and the page's result comes
back into the turn as the tool's answer. Claude Code's Artifacts do
this through a hosted page and comments; the research of 2026-09-04
(`docs/design/interactive-surfaces-research.md` §(a)) says the same
round trip needs no hosting: a page served on a loopback port, opened
in the person's browser, held open until it posts. The user chose this
route ("本地网页可以搞"). Sharing and multi-viewer state are not part
of it.

## Bricks

1. **The loopback, one brick.** `bingo-auth-oauth` already owns a
   one-shot loopback server (`loopback.rs`: bind `127.0.0.1` on a
   port from a short list, one connection, a `state` nonce) and a
   best-effort browser opener (`browser.rs`, `BINGO_NO_BROWSER`).
   A tool plugin cannot import a plugin, so the two move to a
   library-tier crate `bingo-loopback` (tokio `net` only; no new
   external crate; the workspace member is +1 on the budget, 331 →
   332, one line in the ADR) and `bingo-auth-oauth` reads them from
   there — a net move, its tests with it. The server grows what a
   page needs and the login never did: serve one `GET /<token>` with
   a body, accept one `POST /<token>/answer` with a JSON body (cap 1
   MiB), answer a `GET` for anything else with 404, and close. Still
   loopback, still one page, still a random token in the path so no
   other local process can answer for the person.
2. **The tool.** `ShowPage` in `bingo-tool-web` (it is a page; the
   crate already owns what the model fetches and reads): `{ title,
   html, timeout_secs? }`. It serves the HTML with one script injected
   before `</body>` that defines `window.bingo.submit(value)` (POSTs
   JSON, then shows "sent — you can close this tab") and
   `window.bingo.cancel()`; opens the browser; waits for the POST, the
   cancel, the timeout (default 10 min), or the turn's interrupt
   (`ToolContext`'s cancellation — an `esc` drops it like any call);
   answers with the posted JSON as text, `cancelled` on cancel, and an
   error on timeout. Traits: not read-only, not concurrency-safe
   (it holds the turn on a person, like `AskUserQuestion`), trusted.
   `DESCRIPTION` tells the model when a page beats a question
   (comparison, layout, more than four options, a multi-step choice)
   and that the page must call `window.bingo.submit` with a plain
   JSON value. Headless (`--print` without a browser, `BINGO_NO_
   BROWSER`): the call fails closed at once with the URL in the
   message — a person on the machine can still open it.
3. **What the person sees.** While the call is open, the tool's row
   shows the URL (a `Preview::Url`, which the row already draws for
   `WebFetch`) and the status line's notice says a page is waiting in
   the browser; `esc` ends it. No new card, no new interaction kind:
   a page in flight is a tool in flight.
4. **The model's word.** One sentence in the tool description is
   enough; the identity prompt does not change (rule 4c).

## Files

`crates/bingo-loopback/` (new, library tier: `lib.rs`, `server.rs`,
`browser.rs`), `bingo-auth-oauth/src/{loopback.rs,browser.rs}` →
deleted, `Cargo.toml`s; `bingo-tool-web/src/{lib.rs,page.rs}`;
`scripts/budget.sh` limit 332; `docs/adr/0012-*.md` (the library tier
list) or a new ADR-0042 (≤60 lines: the loopback brick, the token, the
fail-closed headless path, budget); `docs/design/tui.md` §5 (the row).

## Exit criteria

- [x] A `ShowPage` call opens the browser, and the page's
  `window.bingo.submit({...})` is the tool's result (test: a scripted
  client POSTs to the served URL; no real browser).
- [x] Cancel, timeout and `esc` each end the call as the plan says.
- [x] `bingo-auth-oauth` logs in through `bingo-loopback` unchanged
  (its tests moved, not rewritten).
- [x] A POST without the token, or from a second page, is refused.
- [x] Every AGENTS.md gate; budget 332 with its ADR line; `cargo deny`;
  Windows cross-check for `bingo-loopback` (`cargo check --target
  x86_64-pc-windows-msvc` — the browser command has a Windows arm).
- [ ] Hands-on (main session with the user): "给我三个布局方案让我选"
  → a page, a click, the choice back in the transcript.

## Non-goals

Hosting, sharing, comments, multi-viewer state (Artifacts' territory).
Serving assets beyond the one HTML (the model inlines what it needs).
A page that stays open across turns. HTTPS. Reusing the page's port.

## Risks

- The model writes the page: a bad page never submits. The timeout
  and `esc` bound it; the row shows the URL so the person can reload.
- The browser opener is best effort on a headless box; the URL in
  the message is the fallback, as for `/login`.
- 1 MiB of JSON back into the context is a lot; the tool's result is
  clipped by the kernel's global limit like any other.

## Verified

*2026-09-04, worktree `.claude/worktrees/m54` on `m54-page`, base
`5ea52795`. Four commits, one per brick.*

### What landed

1. **The loopback, one brick** (`34285562` the record, `1d3a7fca` the
   move, `64f2c3a9` the page). `crates/bingo-loopback`, library tier, no
   external crate of its own: `request` parses a head and the one header
   that says how much more there is, `response` writes one back,
   `server` owns `Loopback::{any,in_range}` and one `Connection` per
   client, `token` mints and constant-time-compares the secret in the
   path, `script` holds the injected `window.bingo` as a `const`, `page`
   is the URL, the document and the three routes, `answer` reads the
   posted envelope, `serve::until_answered` is the only place the socket
   and the meaning meet, and `browser` is the opener moved whole.
   `bingo-auth-oauth` keeps the OAuth half in a new `redirect` module —
   the ports the issuer's allow-list names, the `state` check, the page a
   browser is left with — and its 51 tests pass unedited but for the
   module they live in and the `head()` helper they no longer need.
2. **The tool** (`e3bb715c`). `ShowPage` is `bingo-tool-web`'s third
   tool: `{ title, html, timeout_secs? }`, and four endings — the page
   submits (the JSON is the result), the page cancels (`is_error`, no
   answer), the turn is interrupted (`ToolError::Cancelled`, checked
   first in the `select!` so an `esc` already taken is never overtaken),
   or nobody comes inside the wait (ten minutes by default, an hour at
   most). `Opener` is the seam the tests reach through: a scripted client
   fetches the served page, keeps it for the test to read, and posts what
   a person clicking in it would have posted. No real browser anywhere.
3. **What the person sees.** No TUI code at all — see below.
4. **The model's word.** `DESCRIPTION` only; the identity prompt is
   untouched, as rule 4c says.

### What the plan got wrong

- **`Preview::Url` cannot carry the URL.** `Tool::preview` is a pure
  function of the call's input, read by the gate *before* the call
  (`bingo-core/src/gate.rs:154`), and the port is only known once it is
  bound. The URL reaches the person on the **running row's live line**
  instead (`ToolContext::progress`, which the transcript already draws
  as a tool's tail): `title — http://127.0.0.1:<port>/<token>`. It is
  also the fallback a person reads when no browser could be opened.
  Recorded as a dated entry in `docs/design/tui.md` §10 rather than in
  §5 — a page in flight is not a content kind, it is a tool in flight.
- **The library tier had to widen.** The plan wrote "the two move to a
  library-tier crate … and `bingo-auth-oauth` reads them from there"
  without noticing that `scripts/check_discipline.sh` forbade exactly
  that: a library depended on `bingo-sdk` and on nothing else in the
  workspace. ADR-0042 §2 widens the tier to "and on other libraries",
  the script asserts the new rule, and cargo refuses a cycle for free.
  That the whole check passes with `bingo-auth-oauth -> bingo-loopback`
  in the graph is the proof the new crate is read as a library.
- **"One page then close" is not what a browser does.** The serve loop
  ends on the answer only: a `GET` may be served any number of times
  (which is what makes the plan's own "the person can reload" true), a
  404 leaves the page waiting, an oversized body is a 413 and the page
  waits on, and a socket a browser opened without ever speaking on is
  not an error. Closing after one request would have died on the first
  speculative connection or favicon.
- **The status line's notice was not built.** Brick 3 wanted "a page is
  waiting in the browser" on the status line; the row's live line says
  it instead. The status line is `bingo-surface-tui`'s and another worker
  held those files this milestone. Not done, not attempted.
- `callback::parse` takes the request target rather than the whole
  request head: the head is the library's to read now, so the parser got
  smaller rather than moving.
- The plan's file list said `bingo-loopback/{lib,server,browser}.rs`;
  what the bricks wanted is ten small modules, one noun each.
- `PageArgs` is `deny_unknown_fields`, unlike the tree's other tool
  args. A `timeoutSecs` that nobody claims would otherwise default
  silently to ten minutes of a person's time — this cost the worker one
  600-second hang before it was written down.

### Not verified here

- **The hands-on criterion** — a real browser, a real click — is the
  main session's and is left unticked.
- The injected script is asserted as **text**, never executed: no test
  runs a JS engine, so `window.bingo.submit` is proved to be served
  once, before `</body>`, and not proved to work in a browser.
- **`bingo-auth-oauth` and `bingo-tool-web` do not cross-check for
  Windows locally, and did not before this change**: `aws-lc-rs` (oauth,
  for PKCE) and `reqwest` (tool-web) each build C that wants
  `windows.h`, which is ADR-0041's recorded note. `bingo-loopback` is
  kept clean of both on purpose — its own test client speaks HTTP by
  hand — and does cross-check. CI's `windows` job is the backstop for
  the other two.
- `ShowPage` is not read-only, by the plan's own choice, so `plan` and
  `dontAsk` modes refuse it and every other mode asks once before the
  page opens. Checked at the trait level, not through the gate.
- `ShowPage` crosses to a bridged ACP agent like any other tool of the
  turn (`bingo-provider-acp/src/shared.rs`: "a tool added to the house
  reaches the agent the same turn it reaches any other model"). Left as
  it is; whether a sub-agent should be able to open a page on the
  person's screen is not a question this milestone asked.

### Gates

```
$ cargo fmt --all -- --check                       # exit 0, no output
$ cargo check --workspace --all-targets --locked
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 06s
$ cargo clippy --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 35.91s
$ cargo test --workspace --locked | tee target/m54-test.log
81 test binaries, 3653 passed, 0 failed          # PIPESTATUS=0
                                                 # no known flake was hit
$ scripts/check_discipline.sh
dependency direction ok · kernel names no tool · cohesion ok
discipline ok            # warns pre-existing (session.rs, host.rs, main.rs …)
$ scripts/budget.sh
dependencies (unique, normal): 332 (max 332)
warm cargo check -p bingo-core: 0s (max 20s)
relink isolation: touching the TUI recompiled 0 crates for core (must be 0)
budget ok                # target/debug 7 GB warns: a worktree's first full build
$ cargo deny check
advisories ok, bans ok, licenses ok, sources ok
$ cargo check -p bingo-loopback --all-targets --locked \
      --target x86_64-pc-windows-msvc
    Finished `dev` profile [unoptimized + debuginfo] target(s)
$ cargo check -p bingo-auth-oauth … --target x86_64-pc-windows-msvc
warning: aws-lc-sys@0.44.0: … jitterentropy-base-windows.h:49:10:
    fatal error: 'windows.h' file not found       # pre-existing, see above
```
