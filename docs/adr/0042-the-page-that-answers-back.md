# ADR-0042 — The page that answers back

Status: accepted · 2026-09-04 · Plan: M54

## Context

The model can put four options in front of a person (`AskUserQuestion`) and
nothing wider — not a layout to compare, not a wizard. The research of
2026-09-04 (`docs/design/interactive-surfaces-research.md` §(a)) found the
round trip needs no hosting: a page served on a loopback port, opened in the
person's browser, held open until it posts. That socket already exists in
`bingo-auth-oauth` — bind `127.0.0.1`, one connection, a nonce that makes a
request from anywhere else worthless — but it sits in a library ADR-0012 §1
wrote one crate deep, depending on `bingo-sdk` and nothing else in the
workspace, and a tool is a plugin, which ADR-0001 forbids importing a plugin.
No kernel door is asked for: a tool call already carries its result back.

## Decision

1. **`bingo-loopback`, a library.** The socket (`Loopback::any`/`::in_range`,
   one `Connection` per request, an 8 KiB head cap) and the best-effort browser
   opener (`BINGO_NO_BROWSER`) move there out of `bingo-auth-oauth`, which
   keeps the OAuth half: the redirect's path, its `state` check, the page a
   browser is left with. No new external crate — `tokio` `net`, `serde_json`,
   `base64`, `getrandom`, `thiserror`.
2. **The library tier is a layer, not a crate.** A library may depend on
   `bingo-sdk` and on another library, and on nothing else in the workspace;
   `scripts/check_discipline.sh` asserts that, and cargo itself refuses a
   cycle. Plugins are unchanged: a plugin may depend on any library.
3. **What the served page is.** `GET /<token>` answers the one document with
   one script injected before `</body>` (`window.bingo.submit(value)`,
   `window.bingo.cancel()`, posting to `location.pathname + "/answer"`);
   `POST /<token>/answer` takes a JSON body capped at 1 MiB and ends the serve;
   everything else is a 404 and the page keeps waiting, so a reload works and a
   second local process cannot answer for the person. The port is ephemeral and
   the token is 32 random bytes as base64url compared in constant time: the
   path *is* the authority, as for the ACP bridge (ADR-0036 §3, same brick).
4. **A page that cannot be opened is a call that fails.** When the opener
   answers false — `BINGO_NO_BROWSER`, a container, no `xdg-open` — the call
   fails at once with the URL in the message, rather than hold the turn on a
   browser that will never arrive; a person at the machine can still open it.
   The turn's cancellation and a timeout (10 minutes) are the other two ends.
5. **Budget** 331 → 332: the member `bingo-loopback`, and nothing else.

## Consequences

- `bingo-auth-oauth` behaves identically. Its local Windows cross-check does
  not run and did not before — `aws-lc-rs` builds C and wants `windows.h`, as
  `reqwest` does in ADR-0041's note. `bingo-loopback` brings neither, and its
  tests speak raw HTTP over `tokio`, so the new crate cross-checks locally.
- `ShowPage` is not read-only: the gate asks once before the page opens, and
  `plan` refuses it — a page is a person's screen, not a file read.
- A GUI surface may later embed the same page in a webview: same document,
  same token, same `window.bingo` — the isolation is the loopback origin and
  the path, not the browser. Hosting, sharing, comments and multi-viewer state
  stay Artifacts' territory and are not owed here.

## Supersedes

ADR-0012 §1's "depends on `bingo-sdk` and external crates only": a library may
now depend on another library (§2). Refs: ADR-0001, ADR-0036 §3, ADR-0041.
