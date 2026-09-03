# 0010 — Sub-sessions: peer delivery, redirect, tree attachment

## Context

A sub-agent is a session with a `parent` link (ADR-0002); the kernel already mints them under a depth and a width limit, and `ToolHost` already has `spawn_session` and `submit`. Three things a plugin cannot do on top of that: post into another session as a *peer* (a client's `submit` parses commands, runs `on_submit` hooks and opens a `Submit` turn, none of which a message from an agent should do); redirect a person's `@name` line (the actor rejects `HookOutcome::Redirect`); and let the one surface a person is looking at see a child's permission prompt (an attachment is one session, so a child that asks hangs until someone opens it by hand). `ItemBody::ToolCall.child_session` duplicates the child's own `parent.item`.

## Decision

1. **Peer delivery.** `ToolHost::deliver(to, intent, input, Delivery) -> Result<(), KernelError>` replaces `submit`. The error is only "no such session"; outcomes still arrive as the target's `IntentAck`. The target records the text as a `User` item with the sender's `Origin`, parses no command and runs no `on_submit` hook. `Delivery::Wake`: an idle target opens a turn with `TurnOrigin::Peer`, taking any prose already held in its queue first. `Delivery::Hold`: an idle target queues it (`Queued`) and the next turn, whatever opens it, carries it. A busy target queues either, and the barrier absorbs it like any steer. The queue is the inbox; nothing else is.
2. **Redirect.** `HookOutcome::Redirect{session}` from `on_submit` delivers the input as the hook left it to `session` with `Wake` under a fresh intent, and acks the original `Applied{"redirected": session}`; an unknown target is `Rejected{SESSION_NOT_FOUND}`.
3. **Tree attachment.** `HostApi::open(selector, who, OpenOptions)`; with `children: true` the attachment's stream also carries every live descendant's frames from its head (`seq` 1) — descendants created later included — each frame stamped with its own `session`. (2026-09-03: and every stored one's. A resume revives the root alone — a child comes back when it is woken, not before (ADR-0005) — so the attachment asks both authorities: descendants this host runs are followed live, descendants only the store knows are replayed from their journals onto the same stream, read-only, from the store's own listing of the tree however deep. A replay acquires nothing and answers nothing; a replayed session that later wakes here is followed on from the last `seq` forwarded, so no frame reaches the client twice. What a client folds is the whole tree as data; what it can write to is still only what is live.) A lag anywhere in the tree, the root's included, is healed in the kernel — re-subscribed from the last `seq` forwarded — so the stream never carries a `Lagged` marker and each session's `seq` stays contiguous; what a lag loses is the deltas and notices the journal never keeps. The handle's `answer` reaches whichever session in the tree opened the interaction; `submit`, `interrupt`, `history` and `events_since` are the root's. On the wire: `session/open.params.options.children`; an `event` notified under a tree attachment also carries `root`, the session it was opened through, which is what the client routes by — a root's own frames stay the frame verbatim.
4. **One fact.** `ToolCall.child_session` is removed. The child's `SessionSummary.parent.item` names the tool call that spawned it; a client derives the link from the frames it holds or from `sessions{parent}`.
5. **Who spoke.** The fold prefixes a user item whose origin names a principal with a `[from <principal>]` line, so the model can tell an agent's message, or a person's in a group, from the one it works for.
6. `delete(parent)` deletes live descendants first. Limits stay in `HostConfig`: depth 1, twenty children.

## Consequences

- sdk touched once: `OpenOptions`, `Delivery`, `ToolHost::deliver`, `ToolCall.child_session` gone. Every `HostApi` and `ToolHost` double gains a parameter (core, rpc, print, tui, bash, permissions, skills, mcp tests).
- Crates: `bingo-core` (actor, tool host, `host/tree.rs`, fold), `bingo-surface-rpc` (`OpenParams.options`, schema), `bingo-surface-print` (stream-json opens with children; `parent_tool_use_id` is the child's `parent.item`), `bingo-surface-tui` (one reducer per session in the tree, the same `draw`), `bingo-agents` (new).
- The kernel still has no agent noun: a child is a session, a message is a queued input, a name is a plugin's `title`, and `@` is a hook's business.
- A held message can wait indefinitely in an idle session; `QueueChanged` shows it.

## Supersedes

—
