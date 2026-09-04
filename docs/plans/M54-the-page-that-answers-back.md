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

- [ ] A `ShowPage` call opens the browser, and the page's
  `window.bingo.submit({...})` is the tool's result (test: a scripted
  client POSTs to the served URL; no real browser).
- [ ] Cancel, timeout and `esc` each end the call as the plan says.
- [ ] `bingo-auth-oauth` logs in through `bingo-loopback` unchanged
  (its tests moved, not rewritten).
- [ ] A POST without the token, or from a second page, is refused.
- [ ] Every AGENTS.md gate; budget 332 with its ADR line; `cargo deny`;
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
