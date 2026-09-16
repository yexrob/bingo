# 0060 — An agent ends where it began: by whoever started it

## Context

A sub-agent is a child session (ADR-0010) and, since 2026-09-05, one that stays live after it has answered so it can be spoken to again (ADR-0010 §6). Nothing was given the other half of that: the model that started an agent could neither stop its turn nor delete it, and `/agents` took no arguments, so a person could not either. The roster only grew — a project that has cycled its team a few times carries every retired role for good — and M8 had written the gap down ("killing or deleting an agent from the model") as a non-goal, M82 as `KillAgent`-shaped work nobody had asked for. The user asked (2026-09-16), with four dead roles on the roster and no verb to remove them.

The kernel already has every verb: `interrupt` on an attachment, `HostApi::delete` (descendants first), `deliver` to a persisted child (reopened first). No door opens here, so the ratchet question of `docs/adr/README.md` does not arise: this record decides which sessions a plugin's tool may end, and how the roster shows what the summary already carries.

## Decision

1. **`StopAgent { agent }`** ends the running turn of a child the caller started: an attachment opened by id, `interrupt(Head)`, and the receipt says how the turn ended once its `TurnCompleted` is read (or that it had already ended, when the ack is a refusal). An idle child is refused in words. Its traits are the plugin's — trusted, read-only, in process — so it runs unasked like `SendMessage`: it ends work, it destroys nothing, and the background watcher of a spawn reports the cut turn exactly as it reports one a person cut with `esc`.
2. **`DismissAgent { agent }`** deletes an idle child the caller started: `HostApi::delete`, the journal with it. A busy child is refused — stop it first, or wait — so a deletion never races a turn. It is `destructive: true, read_only: false, trusted: true`: the gate asks in `default`, refuses in `plan`, and an allow rule names it like any other tool. A teammate beside the caller is not the caller's to end; `/agents dismiss <name>` is the person's spelling for their own children, ungated as every command is. A dismissed team role is seated afresh at the next root open (ADR-0011); a room roster keeps the name, and skips it, as it does any name nobody holds.
3. **`SpawnAgent { reopen: true }`** hands the prompt to the caller's own child of that name instead of minting `name-2`: the same session, its memory intact, the same three arms (waited for, watched, or held on standby). Staffing fields in the call are ignored for a child that already stands, because what it is was settled when it was made; without such a child the call is an ordinary spawn.
4. **A roster row carries what the summary carries**: `agent`, `session`, `state`, `model`, `messages`, `tokens`. No new fact — `SessionSummary` has held `model`, `messages` and `usage` all along; the row simply stopped dropping them. `tokens` is `usage.input_total() + output_tokens`, the one arithmetic already used for a session's cost.

## Consequences

- `bingo-agents` grows two tools (`stop.rs`, `dismiss.rs`), one field, three columns, and `/agents [stop|dismiss <name>]`; `provides` and the page say so.
- The crate's "every tool is read-only and trusted" no longer holds: `DismissAgent` is the one exception, and the test that pinned the rule now pins the exception.
- A deleted child leaves the TUI's tree two ways, because a deletion reaches a client two ways: a live child's own stream ends in `SessionClosed { Deleted }`, and a child that was only stored — replayed into a resumed tree from its journal — has no stream, so the row leaves on the gateway's `SessionRemoved`, which the TUI had ignored until this record (found 2026-09-16: four dismissed roles stayed `stored` in the switcher).
- Not decided here: ending a teammate, a budget per spawn, `SetModel`, a wait-with-timeout, and the state words beyond busy/idle. Each is its own record when asked for.

## Supersedes

— (amends ADR-0010 §6 with its missing half; ADR-0010 is annotated).
