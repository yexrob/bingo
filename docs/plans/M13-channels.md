# M13 — IM channels: Feishu first, the trait proven twice

## Goal

A person drives a session from an IM thread (ADR-0016): their messages open or continue a session keyed `<adapter>/<chat>[/<thread>]`, the answer streams into an edited message, a permission question is buttons (or numbered replies) that resolve the same `Interaction` the TUI would, and a resolution anywhere edits the card. Feishu is the first real adapter; the loopback adapter proves every Deliverer behaviour offline first.

## Bricks, in build order (worker C)

1. **Caps + Limits + `ChannelAdapter`** — mechanism-accessor capabilities (`edit()/buttons()/typing()/threads()` as `Option<&dyn …>`), `Limits{max_text: (usize, Encoding), dialect, max_actions, max_label}`; the loopback adapter with all of it configurable.
2. **`Deliverer`** — pure reducer, frames → `Open/Replace{full}/Finalize/Status/Resolved{by}`; dual-gate coalescing (sentence boundary, ≥N chars, timer), one pending snapshot per conversation drained before finalize; the question ladder (buttons → numbered replies, free text where the spec allows); fixtures shared with the TUI's frame fixtures.
3. **The surface host** — `SurfaceKind::Concurrent`; inbound event → open-or-continue by key (mention-gated in groups), `Origin.principal` stamped from the platform id; outbound pump per conversation; the per-credential lock file with a loud refusal.
4. **Feishu wire bricks** (each pure, each fixture-tested before its I/O): tenant token cache (TTL 2 h, endpoint self-caches); the frame codec (bootstrap POST shape, 11-field protobuf hand-decode, ACK echo with base64 data, chunk reassembly `sum`/`seq` 5 s); the WS client (app-level ping, hot-updated intervals, read deadline `2×PingInterval+5 s`, reconnect jitter `[0,30)` then 120 s flat, single-use URL, handshake-status errors: 403 fatal, conn-limit fatal).
5. **Feishu delivery** — CardKit streaming (entity → send by `card_id` → full-text element PUTs, one strictly-increasing `sequence` per card, `uuid` idempotency, re-arm before the 10-min close, finalize with `streaming_mode:false`); overflow to `post` (`md`/`code_block`); question cards with `behaviors:[{type:"callback"}]`, 3 s callback ACK with toast, resolution edits; per-chat send queue (5 QPS shared per group), 429/`230020` drops the frame and streams on; scopes `im:message.p2p_msg` + `im:message.group_at_msg`.
6. **Black-box + live** — loopback end-to-end through the real binary (message in → streamed edits → question → numbered reply resolves → transcript shows the turn); the two-surface race (answered in TUI while the card shows → card edited `approved in the TUI`); kill-the-socket reconnect test; a `scripts/feishu-smoke.md` runbook for the live smoke (needs the user's app credentials — self-built app, events + callbacks switches, long-connection mode).

## Files

`crates/bingo-channels/src/{lib,caps,deliver,host,loopback,feishu/{mod,token,frame,ws,card,send}}.rs` (worker's split may differ; one noun per module holds); `crates/bingo/src/main.rs`, workspace + bin `Cargo.toml`; `crates/bingo/tests/channels.rs`; `scripts/budget.toml` (+ measured tokio-tungstenite delta, reason line); `docs/plans/M13-channels.md` (this file, Verified appended).

## Capability matrix (recorded for the adapters-later; verified 2026-08-31)

| | Feishu | Telegram | Slack | Discord |
|---|---|---|---|---|
| transport, no public URL | WS long-conn (self-built apps, cluster: one random client) | `getUpdates` (409 single-consumer) | Socket Mode (≤10 conns) | Gateway WS (1000 identifies/24 h) |
| edit sent message | text: **20/lifetime**; card PATCH 5 QPS; **CardKit stream QPS-exempt** | yes | yes + native `chat.startStream` | yes, bucket unpublished |
| max length | card/post 30 KB | 4096 chars | 4000; `markdown_text` 12000 | 2000 chars |
| buttons | card callback, 3 s | inline keyboard, `answerCallbackQuery` | Block Kit, 3 s | components, 3 s, deferrable |
| typing | — (the stream is the affordance) | `sendChatAction` | none for bots | 10 s re-poke |
| send rate | **5 QPS/chat shared across bots** | ~1/s chat, 20/min group | ~1/s channel | 50/s global, per-route buckets |

## Exit criteria

- [ ] Deliverer fixtures: a streamed answer coalesces by the dual gate; the pending snapshot never overwrites finalize; a question renders both rungs of the ladder; `Resolved{by}` edits
- [ ] frame codec fixtures round-trip a captured bootstrap answer, an event frame, a chunked pair, a ping/pong with hot-updated intervals
- [ ] loopback black-box: message → streamed edits → button and numbered-reply answers both resolve; the TUI race edits the card
- [ ] a killed socket reconnects on the documented ladder; the second process against one credential refuses loudly
- [ ] budget: tokio-tungstenite measured and recorded; deny green
- [ ] every gate green (fmt, check, clippy, test, discipline, budget, deny, tui-smoke)
- [ ] live Feishu smoke per the runbook — **needs the user's credentials; ticked last, together**

## Non-goals

Telegram/Slack/Discord adapters (matrix above is their brief); webhook mode; marketplace apps; multi-process sharding; voice/files/images inbound; `bingo-rooms` semantics (a room is not a channel).

## Risks

R-wire — the Feishu long-connection format is undocumented; the codec-as-brick with fixtures is the mitigation, and a format move is one failing test. R-intl — Lark International's long-conn availability is unverified; the live smoke decides. R-loss — events during a long outage may be dropped (no replay API); accepted for chat. R-streams — CardKit's 10-minute close and the sequence rules are exercised by fixtures, not live, until the smoke.
