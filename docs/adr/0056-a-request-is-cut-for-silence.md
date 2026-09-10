# 0056 — A request is cut for silence, not for size

## Context

Both HTTP providers wrap `send()` — the upload of the request and the wait
for its status line — in the same 60 s guard that watches a quiet body.
That reads a moving upload as silence. The Responses API replays every item
of the conversation each round, so a 406k-token session is an 8.8 MB body,
and on the link to one relay it took 12 s to the first byte at the median,
20 s at p90 — and past 60 s often enough that one session logged 35
`timeout` retries in a day, each after 62 s of the provider's own doing,
while the relay recorded each as a 400 "Failed to read request body".
Codex bounds only the connect and the idle between SSE events (300 s,
`codex-rs/model-provider-info`, `codex-client/src/sse.rs`); its zstd body
and `previous_response_id` apply to OpenAI's own backend only, so nothing
there helps a third-party endpoint. A headless run — `--print`, the
gateway, a channel, a schedule — still needs a server that connects and
then says nothing to end the turn, which is the guard's one real job.

## Decision

1. **Three phases, two bounds.** Connecting is bounded by the client's
   `connect_timeout` (`CONNECT_TIMEOUT`, 20 s). The request — upload and
   the wait for the status line — and the body that follows share one
   guard, `IDLE_TIMEOUT` (300 s): the time in which **no byte has moved on
   the wire in either direction**. Size never ends a request; silence does.
2. **Silence is measured, not assumed.** The body goes out as a metered
   `http_body::Body` over the serialized bytes — exact `size_hint`, so the
   request keeps its `Content-Length` and is never chunked; frames of at
   most 64 KiB — that stamps a shared clock on every frame the transport
   pulls. The guard fires when the clock has not moved for `IDLE_TIMEOUT`;
   after the status line the body stream stamps the same clock per chunk,
   which is the guard it already had.
3. **The builder stays whole.** What `send()` is handed keeps its bytes
   body, so `RequestBuilder::try_clone` for the sign-in-again retry keeps
   working; the metered body exists only between `build()` and `execute()`.
4. **The error is the one the ladder knows.** A fired guard is
   `ProviderError::Timeout`, retried by the turn loop as today; the kernel
   is untouched and the wire says nothing new.
5. **Two copies, one record.** Each provider carries the bricks itself —
   a plugin may not import another plugin, and the sdk owns no wire — as
   `stream.rs` already notes for the guards it has; the numbers are decided
   here, once: 20 s connect, 300 s idle, 64 KiB frame. They are constants,
   not settings.

## Consequences

- A slow upload is no longer a failure; a relay that reads at a trickle
  gets its whole body. A server that goes silent is still cut, 5 min after
  its last byte instead of 1; the ladder's first retry follows as before.
- A reasoning model quiet for under five minutes is no longer cut either —
  Codex's precedent, and what the summary-less endpoints need.
- `http-body` and `bytes` become direct dependencies of both providers.
  Both are already in the tree under `reqwest`, so `scripts/budget.sh` is
  expected unchanged; the run is pasted in the plan's Verified section.
- The bricks take the idle duration as a parameter so a test can drive
  them at milliseconds over a real socket; the constants are read only at
  the two call sites.

## Supersedes

Nothing. Amends the guard note at the head of both providers' `stream.rs`.
