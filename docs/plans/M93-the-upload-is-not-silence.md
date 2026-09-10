# M93 — The upload is not silence

## Goal

User, 2026-09-11, on a session that "一直重试": the relay's backend showed
400 "Failed to read request body"; bingo's journal showed no 400 at all —
35 `turnRetrying` with reason `timeout`, each ~62 s after the previous
event, on a 406k-token session whose every round replays 8.8 MB. The cut
is bingo's own: `round_trip`/`send` in both HTTP providers wrap
`builder.send()` in the 60 s `IDLE_TIMEOUT` meant for a quiet body, so an
upload that is still moving at 60 s is ended and the relay logs the torso.
ADR-0056 decides the rule: connecting is bounded on its own; everything
after is cut only for silence, and silence is no byte moving on the wire in
either direction for `IDLE_TIMEOUT` (300 s, Codex's number).

After this milestone: a slow upload completes; a server that accepts the
connection and says nothing still ends the turn as `Timeout`; the request
keeps its `Content-Length`; nothing on the wire, in the journal or in the
kernel changes.

## Bricks, in build order

Each brick is written in `bingo-provider-openai` and then copied to
`bingo-provider-anthropic` in the same commit, as the guards already are
(ADR-0056 §5). Both crates gain `http-body` and `bytes` as direct
dependencies (already in the tree).

1. `metered.rs` — `Clock`: an `Arc` of the instant the last byte moved
   (`stamp()`, `last() -> Instant`); `Metered`: an `http_body::Body` over
   one `Bytes` that yields `Frame::data` slices of at most `FRAME` (64 KiB),
   stamps the clock on every frame it yields, and answers
   `size_hint() = SizeHint::with_exact(len)`, `is_end_stream` when drained.
   Unit tests: frames concatenate to the bytes; the exact hint; an empty
   body ends at once; the clock moves once per frame.
2. `metered.rs` — `quiet(clock, idle) -> impl Future<Output = ()>`: sleeps
   until `last() + idle`, re-reads, sleeps again; resolves only when the
   clock has stood still for `idle`. Test under `start_paused`: a clock
   stamped every 10 s under a 60 s idle never resolves in 10 min; one
   stamped once resolves at +60 s.
3. `lib.rs` — `round_trip(builder)` becomes `round_trip(builder, idle)`:
   `let mut request = builder.build()?`; the bytes are read off the body
   (`Body::as_bytes`, copied into `Bytes`) and replaced with
   `Body::wrap(Metered::new(bytes, clock.clone()))`; then
   `select! { biased; r = http.execute(request) => r, _ = quiet(&clock,
   idle) => Err(Timeout) }`. The response's chunk stream (`stream::chunks`)
   takes the same clock and stamps it per chunk; `Body::pump`'s guard reads
   the clock through `quiet` instead of `tokio::time::timeout` on one
   `next()`. The builder handed in is never the metered one, so
   `send`'s `try_clone` for the sign-in-again path is unchanged.
4. `lib.rs` — the client is built once per provider by `http()`:
   `reqwest::Client::builder().connect_timeout(CONNECT_TIMEOUT).build()`;
   `CONNECT_TIMEOUT = 20 s` beside `IDLE_TIMEOUT = 300 s` in `stream.rs`,
   whose head comment now says what silence is. `CODEX_MODELS_TIMEOUT`
   stays as it is.
5. `lib.rs` tests over a real `tokio::net::TcpListener` (not wiremock,
   which reads the whole request first), with `idle` passed in at
   milliseconds and every bound generous (ADR: a machine is not the
   machine): (a) a peer that reads the 2 MB body 8 KiB per 20 ms and then
   answers 200 — the request succeeds under a 300 ms idle, though it takes
   seconds; (b) a peer that stops reading after 64 KiB — `Timeout` arrives,
   and not before `idle` has passed; (c) the request head on the socket
   carries `Content-Length: <len>` and no `Transfer-Encoding`; (d) the
   sign-in-again path still replays the same bytes after a 401 (the
   existing test, kept green).
6. `stream.rs` tests: the paused-time `a_body_that_goes_quiet_times_out`
   keeps its name and passes against the clock-driven guard.

## Files

- `crates/bingo-provider-openai/{Cargo.toml,src/lib.rs,src/stream.rs,src/metered.rs}`
- `crates/bingo-provider-anthropic/{Cargo.toml,src/lib.rs,src/stream.rs,src/metered.rs}`
- `Cargo.lock`; `scripts/budget.toml` gains no line unless the count moves.
- `docs/adr/0056-a-request-is-cut-for-silence.md`, `docs/adr/README.md`.

## Exit criteria

- [ ] `Metered`: frames concatenate to the input, ≤64 KiB each, exact size
      hint, clock stamped per frame (both crates)
- [ ] `quiet`: never resolves while the clock moves; resolves `idle` after
      its last stamp (paused time)
- [ ] real-socket tests (a)–(d) above green in both crates, each in under
      10 s on this machine, none pinning a wall clock tighter than 5× its
      expected duration
- [ ] `cargo tree -p bingo-provider-openai -e normal | grep -c .` and the
      anthropic twin unchanged but for the two direct edges;
      `scripts/budget.sh` output pasted (expected 335)
- [ ] the openai `send` still signs in again on a 401 and replays the body
- [ ] `scripts/tui-smoke.sh` green: its esc-before-first-byte step drives
      the fake provider, not this path, and must not move
- [ ] fmt, check, clippy (1.96 and `cargo +1.98.1 clippy` with a scratch
      `CARGO_TARGET_DIR`), test, discipline, budget, deny all green

## Non-goals

- No setting for any of the three numbers; no per-instance override.
- No body compression, no `previous_response_id`, no smaller replay: the
  body is what the conversation is (ADR-0048 keeps its prefix stable).
- No change to the turn loop's ladder (`max_retries`, backoff), to
  `ProviderError`, to the journal or to `bingo-provider-acp`.
- No sdk home for the guards; the duplication and its note stay.

## Risks

- `Body::as_bytes` is `None` for a body that is already streaming; every
  request these providers build today is bytes, and `round_trip` refuses
  (`Config`) rather than sending an unmetered stream if that ever changes.
- Real-socket tests share the machine with the rest of the workspace run;
  the bounds above are the answer, and a test that fails under
  `--test-threads=2` is not done.
- The Windows cross-check of the two reqwest crates dies in aws-lc-sys'
  C build on this Mac, as since M86; CI's `windows` job is the look.
- `reqwest 0.13`'s `Body::wrap` sets `Content-Length` from an exact size
  hint (`async_impl/body.rs` `content_length`); test (c) pins that, so a
  reqwest bump that changes it fails here and not at a relay.
