# ADR-0002 — One event stream: frames, journal, reducers, intents

Status: accepted (2026-08-29). Plan: M0 (in-memory), M3 (on disk). Source: `docs/design/gateway-and-surfaces.md` §0–§4, `docs/design/kernel-and-sdk.md` §4, `docs/design/research/provider-stream-model.md`.

## Context

The old bingo carried four event enums for one stream (`StreamEvent → EngineEvent → AppEventPayload → UiEvent`), five shapes for "a message", seven files for "a conversation", and a projection layer reconciling the transcript with what clients showed. Writes returned receipts, so a synchronous key handler could not be made a client of an async core.

## Decision

1. **One frame type crosses kernel → client**: `Frame { seq, ts, session, cause?, event: Event }`. `Event` is the only output vocabulary; the TUI, `--print`, JSON-RPC, ACP and IM channels all consume it and derive their views at render time. No surface defines a private mirror enum.
2. **Per-session ordered journal.** `seq` is minted by the session actor under one lock and is gapless for durable frames. Durable frames are appended to the store *before* being published. `ItemDelta`, `Notice` and `Lagged` are ephemeral: they take a live `seq` but are never written; replay is therefore a subsequence and clients require monotonic, not gapless, sequences.
3. **Two pure reducers over the same journal.** `SessionState::apply(&Frame)` produces the client view and is the reducer the kernel itself uses for its snapshot; `ContextView::fold(frames)` produces the provider messages. Compaction and rewind are events (`Compacted{boundary, kept, summary}`, `Rewound{to_turn, dropped}`), never rewrites. The journal header carries a `version`; a format change is a migrator, never an in-place edit.
4. **Writes return nothing.** `submit / interrupt / answer` take a client-minted `IntentId` (also the idempotency key) and return `()`. The outcome arrives as `Event::IntentAck { intent, outcome }`.
5. **Ids are ULIDs minted once by the actor** (`SessionId`, `TurnId`, `ItemId`, `InteractionId`) and persisted; they are never re-minted on restart. `IntentId` is minted by the client.
6. **Backpressure is the kernel's to announce.** Each subscriber has a bounded channel; on overflow the kernel sends `Lagged { from, to }` and the client re-reads `events_since(seq)`. The kernel never blocks on a client.
7. **The model stream never leaves the loop.** `ModelEvent` mirrors the Vercel `@ai-sdk/provider` V4 stream-part algebra (per-block ids, `text/reasoning/tool-input` start/delta/end, `Finish { usage, finish_reason { unified, raw } }`, provider metadata keyed by provider id). The accumulator folds it into `Item`s; only `Item`s and `Event`s are published.
8. **Plugin-owned resources** (roster, rooms, tasks) travel as `Event::Extension { plugin, kind, payload }`; the kernel does not enumerate them.

## Consequences

- Any client's view equals `apply(snapshot, frames since snapshot.seq)`; a GUI store is the generated twin of the same reducer, pinned by shared fixtures.
- `ContextView::fold` is the most load-bearing function in the system: it gets a golden test per journal version.
- A synchronous key handler can never wait for a receipt, because none exists.
- A sub-agent is a session with a `parent` link and a room is a session without a model; both render through the same reducer and the same draw function.

## Supersedes

Nothing. Rejects: the old four-enum chain; "session as a container of conversations"; journaling text deltas; ACP as the native protocol.
