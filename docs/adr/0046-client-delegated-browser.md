# ADR-0046 — The client opens the browser

Status: accepted · 2026-09-05

## Context

An attached client may present pages itself. `BINGO_NO_BROWSER` cannot express
that: it reports failure, so `ShowPage` drops its loopback listener immediately.
The URL already reaches the client through tool progress or a login interaction;
no kernel door, additional event, or RPC wire change is needed.

## Decision

1. `bingo_loopback::browser::BROWSER_MODE_ENV` names `BINGO_BROWSER_MODE`.
   Its exact value `client` delegates URL presentation to the attached client:
   `browser::open` returns `true` without spawning an operating-system opener.
2. Presence of `BINGO_NO_BROWSER`, even empty, retains precedence and returns
   `false`. An absent, empty, or unrecognized mode keeps the existing platform
   opener and its success/failure behavior; only `client` opts into delegation.
3. The launcher sets this environment variable only when its client owns URL
   presentation. The client consumes the existing URL, chooses an embedded or
   external browser, and does not treat delegation as proof a page rendered.
   This applies to every caller of the shared opener, including login flows.

## Consequences

- CLI/TUI behavior is unchanged unless explicitly opted in. A delegated
  `ShowPage` continues serving until submission, dismissal, cancellation, or
  timeout. An opener returning `false` still fails immediately.
- A missing or disconnected client can leave a page waiting until its existing
  timeout; this mode adds no delivery acknowledgment or connectivity detection.
- No GUI-specific branches, new dependencies, persistent settings, or wire fields.

## Supersedes

Nothing. Extends ADR-0042's opener boundary with explicit client delegation.
