# M38 — An agent answers as a model

## Goal

ADR-0035 built: a `bingo-provider-acp` plugin whose every configured
ACP adapter (`acp/claude`, `acp/codex`, …) is a `Provider` instance.
Types from `agent-client-protocol-schema`; the ndjson JSON-RPC client
loop is ours, tokio, `Send`. One bingo session is one ACP session
(Zed's shape): `ModelRequest` gains `session: Option<SessionId>`, the
instance maps it to the agent's `sessionId` journaled once as an
extension, and restore climbs resume → load (replay swallowed) →
fresh session + transcript file. The adapter children are kept the
way plugin processes are kept — spawned lazily, `initialize`d once,
restarted on death, killed at `stop()`. The agent's tool calls stream
first-class wearing `acp.external: true`; permissions are the
adapter's own, configured on its row, and a stray
`session/request_permission` is refused closed. The kernel changes by
one field, no door.

## Bricks, in build order

0. **The request names its session.** `bingo-sdk` `ModelRequest`
   gains `session: Option<SessionId>` (serde default, absent
   serializes to nothing, so recorded fixtures stand); core stamps it
   at the one place requests are built. A test on the stamping; every
   provider compiles untouched. Nothing else in sdk or core moves.
1. **The contract.** `agent-client-protocol-schema = "=1.5.0"`
   (budget 302 → 307, the ADR line; `cargo deny check`). `method.rs`:
   a pure table, type ↔ method string, for the ~15 methods used both
   ways. Fixture tests: each message round-trips serde against a
   recorded JSON body — the contract before any transport.
2. **The loop.** `wire.rs`, `connection.rs`: newline-framed JSON-RPC
   over `AsyncRead`/`AsyncWrite` — id counter, pending map, responses
   matched by id, incoming requests routed to a handler, notifications
   to a stream. Generic over the transport: unit tests drive it over
   an in-memory duplex. `child.rs`: spawn from `{command, args, env}`,
   kill-on-drop by process group / job object (tool-bash's `Group`
   lesson, cfg both platforms in the same change).
3. **The fake agent.** A test-helper binary in the crate (a scripted
   ACP agent on stdin/stdout, script from an env path like
   `BINGO_FAKE_SCRIPT`), found via `CARGO_BIN_EXE_…` — rust, so CI
   needs no node. It advertises whatever capabilities its script says:
   the same fake drives the loop, the permission refusal and the
   black-box.
4. **The mapping, pure.** `events.rs`: `SessionUpdate → Vec<ModelEvent>`
   — message chunk → `Text*`, thought chunk → `Reasoning*`,
   `tool_call`/`tool_call_update` → `ToolInput*`/`ToolCall` plus the
   synthetic `ToolResult`, `acp.external: true` in `provider_options`;
   `stop_reason` → `Finish`; usage folded from `usage_update` and the
   end-turn field when present, zero otherwise. Fixture tests per
   update kind.
5. **The session map and the ladder.** `pool.rs`: the kept children
   (lazy spawn, `initialize` once with capabilities cached, respawn
   on death with a notice, `stop()` kills all, kill-on-drop the
   backstop) and the map bingo session → agent session. `session/new`
   journals the extension (`bingo.acp`, `session:<instance>`) through
   the plugin's host handle; restore climbs `session/resume`, else
   `session/load` behind a swallowing flag, else a fresh session
   whose first prompt names the file `transcript.rs` renders from the
   request's fold at that moment. Tests against the fake advertising
   each capability set; one pins that a load replay journals nothing.
6. **The provider.** `provider.rs`, `config.rs`, registration: one
   instance per `[providers.acp.<name>]` row. `stream()` resolves
   `request.session` through the map (a request without one gets a
   one-shot session), holds one `session/prompt`, forwards mapped
   events; `cancel` → the `session/cancel` notification, then awaits
   the cancelled stop — the child and the agent session outlive the
   esc. `models()` empty; auth `NotApplicable`. `request_permission` is answered with its reject
   option and a notice naming the row — the adapter's own permission
   config (args/env on the row) is where yes is said;
   `elicitation/create` declines the same way.
7. **The words.** Config docs; AGENTS.md commit scopes gain
   `provider-acp`; black-box `crates/bingo/tests/cli/acp.rs` (a
   `--print` turn, an interrupt, the ladder's three restores); a live
   `claude-agent-acp` smoke behind an env guard beside the other live
   smokes — needs node and a login, never CI.

## Files

`crates/bingo-provider-acp/src/{lib,method,wire,connection,child,
events,pool,transcript,provider,config}.rs` + the fake-agent bin;
`crates/bingo-sdk/src/model.rs` and the one core request-build site;
`crates/bingo/tests/cli/acp.rs`; `scripts/budget.sh` (the number);
`Cargo.{toml,lock}`; `AGENTS.md` (one scope); docs for config.

## Exit criteria

- [x] Every message used has a fixture round-trip; `cargo deny check`
  green; budget 307 with the ADR line. † The measured number is 308,
  the member crate included; `scripts/budget.toml` carries the count
  and the arithmetic.
- [x] `--print` through the fake agent: text, thought and an external
  tool call land in valid NDJSON, marked `acp.external: true`, and
  nothing was executed by the loop. † The mark rides
  `ItemBody::Reasoning`'s `providerMetadata`, not `ToolCall` — see
  Verified, "decided beyond the plan".
- [x] Esc mid-turn sends `session/cancel` and the turn ends with the
  interrupt wording; the child and the agent session survive to
  serve the next turn.
- [x] Two turns of one bingo session ride one agent session on one
  child (the fake counts its spawns and its `session/new`s); the
  `bingo.acp` extension is journaled exactly once.
- [x] `--continue` against a fake advertising resume / only load /
  neither: each rung proven; on the last, the prompt names the file
  and the file holds the prior fold; a load replay journals nothing.
- [x] A scripted `request_permission` is answered with its reject
  option, one notice names the config row, and the turn goes on.
- [x] `cargo check -p bingo-provider-acp --all-targets --target
  x86_64-pc-windows-msvc` (the child spawns, both spellings); every
  gate in AGENTS.md.

## Non-goals

`bingo-acp` the surface (opposite role, its own milestone). Handing
our tools over MCP. ACP plans, modes, slash commands, `fs/*`,
`terminal/*`, the `-http` transport, protocol v2. A prompter door
for providers — recorded in the ADR, not built. Shipping default
adapter rows — configuration belongs to the person.

## Risks

- The `=1.5.0` pin: a schema bump is deliberate and the compiler
  walks the mapping; recorded fixtures catch silent shape drift.
- Adapters are node children; when one goes (stop, death, fallback)
  the whole process group must go with it or the npx tree lingers
  (M34-D's lesson).
- An adapter may mishandle many sessions across one connection's
  life: when a child misbehaves at `session/new`, the instance falls
  back to a fresh child for that turn and says so.
- The swallowing flag on a `session/load` replay is load-bearing;
  its test is the ladder's most important line.
- codex-acp reports no usage today: zeros are honest, and the ruler
  reads them as unknown, not as free.

## Verified — bricks 0–7, 2026-09-03

Gates, in the worktree: `cargo fmt --all -- --check` clean; `cargo
clippy --workspace --all-targets --locked -- -D warnings` no
diagnostic; `cargo test -p bingo-provider-acp --locked` → 78 unit + 6
integration `ok, 0 failed`; `cargo test --workspace --locked` exit 0,
73 targets, 3128 passed, 0 failed (`bingo --test cli` 145, of which 7
are `acp::`); `scripts/check_discipline.sh` → `dependency direction ok`
/ `kernel names no tool` / `cohesion ok` / `discipline ok`, no new
warn; `scripts/budget.sh` → `dependencies (unique, normal): 308 (max
308)`, `budget ok` — the soft `target/debug` warning is a worktree that
built the workspace twice, not a dependency; `cargo deny check` →
`advisories ok, bans ok, licenses ok, sources ok`; `cargo check -p
bingo-provider-acp --all-targets --target x86_64-pc-windows-msvc` →
`Finished` (the job object compiles beside the process group).

Criteria, and where each is proven. Every wire claim is read from the
scripted agent's own log of what it received, not from what the client
believes it sent; the black-box is `crates/bingo/tests/cli/acp.rs`,
through the real binary.

- Fixtures: `method::tests` round-trips every message the plugin sends
  or reads, `elicitation/create` and its decline included.
- The turn: `a_turn_through_an_adapter_streams_text_thought_and_the_
  agents_own_call` — `Hello there.` as an assistant item, the thought
  as an unmarked reasoning item, the agent's call as a reasoning item
  whose `providerMetadata.acp.external` is `true`, no `toolCall` item
  at all, and `["initialize", "session/new", "session/prompt"]` on the
  wire.
- Interrupt: `an_interrupt_cancels_the_turn_and_the_child_serves_the_
  next_one` — `session/cancel` between two prompts on one child, the
  first result an error, the second answered.
- One session, one child: `two_turns_of_one_session_ride_one_child_and_
  one_agent_session` (one `initialize`, one `session/new`, two prompts)
  and `the_agents_session_id_is_journaled_once_as_an_extension`.
- The ladder: `the_restore_ladder_climbs_resume_then_load_then_a_file`
  — one conversation across four runs whose adapter changes what it can
  restore; resume says nothing, load says `ACP_RESTORE` and its replay
  reaches no item, the fresh rung's prompt names the file and the file
  holds the fold and nothing the journal never had.
- Permission: `a_permission_question_is_refused_and_one_notice_names_
  the_row` — the agent gets `optionId: reject` back, one `ACP_ASKED`
  notice naming `acp.adapters.scripted`, and the turn finishes.
- Death: `an_adapter_that_died_between_turns_is_replaced_and_said`.

Decided beyond the plan:

- **The agent's tool call rides `ReasoningEnd`, not `ToolCall`.**
  ADR-0035 §4 asks for `ToolInputStart/…/ToolCall` and a synthetic
  `ToolResult` wearing `acp.external`, with "the loop never executes
  what wears the mark". Neither is reachable without kernel changes
  this milestone forbids: `ModelEvent::ToolCall` carries no
  `provider_options` to wear a mark in, there is no `ToolResult`
  variant, and a non-empty `tool_calls` sends `Turn::decide` into
  another round — a second `session/prompt` for a turn the agent
  already finished. So the mark went where the kernel already carries
  provider-private data to the journal untouched: `ReasoningEnd`'s
  `provider_metadata` holds the call whole (id, kind, status, title,
  locations, content, raw input and output), and its text is what a
  person reads. Nothing is executed, no second prompt is sent, and
  flipping this to `ToolCall` the day the kernel learns the mark is one
  match arm in `events.rs`. **The ADR's §4 wants amending to say so.**
- **Permissions are refused, not asked** (ADR-0035 §5 as settled):
  `refusal.rs` picks the agent's own `reject_once`, else
  `reject_always`, else `Cancelled`; `elicitation/create` gets
  `{"action": "decline"}`. The notice is said **once per adapter
  session**, not once per question — an agent may ask on every call it
  makes, and the same line twenty times is not a clearer line.
- **`unstable_elicitation` joins the schema features.** The decline is
  then the protocol's own word rather than a JSON shape invented here.
  It adds no dependency; the budget is unmoved.
- **The pointer is journaled only when it is news.** A restore that got
  back into the session the journal already named writes nothing —
  "journaled once" is exact, not approximate.
- **A dead adapter is replaced, not retried.** The plan's brick 5 asked
  for it and the code did not do it: a link stayed in the map after its
  child died, and the next turn failed as transport. `Connection::
  is_alive` and `Sessions::bury` close that, with an `ACP_RESPAWN`
  notice.
- **The budget line is 308, not the ADR's 307.** The research measured
  the schema crate's five edges without counting the member crate,
  which this file has always counted as one. `scripts/budget.toml`
  carries the arithmetic. **The ADR's §2 number wants amending.**
- **No settings manual to write into.** This repository has no
  user-facing config document; the worked pair of rows lives in
  `config.rs`'s module doc, where the key is claimed, and the live
  runbook is `scripts/acp-smoke.md` beside `feishu-smoke.md`.

Not done here: `bingo-acp` the surface (a non-goal, its own milestone).
The live smoke is written, not run — it needs `node` and a login.
`--print` under a real `esc` is not exercised: an interrupt reaches a
headless run as the stream-json control request, which is what the
black-box drives; the TUI's key is unchanged and untested here.
