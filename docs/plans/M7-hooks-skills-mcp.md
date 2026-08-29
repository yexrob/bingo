# M7 — Hooks, skills, MCP: three plugins, dogfooded on the TUI

## Goal

A person's shell hooks fire at every point Claude Code fires them, on the same stdin/stdout contract, and a hook that says "ask" is a dialog, not a crash. Skills on disk are `/name` commands the dropdown lists, a `Skill` tool the model calls, and a line in the system prompt. MCP servers dial in the background at startup; their tools reach the model untrusted through the gate, their stderr reaches a log and never the screen, and `/mcp` says what is connected. The permission mode is a badge the TUI cycles with shift+tab. The kernel wires the three hook points it never called, and the registry learns to read contributions that exist only after I/O (ADR-0009).

## Bricks, in build order (owner)

1. **ADR-0009 + sdk** (kernel) — `ToolSource`, `CommandSource`, `Contribution::{Tools, Commands}`; `ToolSpec.meta` (serde default, skipped when empty; the catalogue passes it through); `PermissionPolicy::describe(session) -> Value`; `LiveTurn.retrying: Option<Retry{attempt, max}>`. One change; the workers build on it.
2. **Sources in the kernel** — `Registry.tool_sources` / `command_sources`; the turn gathers its tools once at start (`cfg.tools`, then each source in order; a duplicate name is dropped with a `TOOL_SHADOWED` notice) and every lookup in the gate and the executor reads that set; `Commands::find` falls through to the sources when the static table has no such name; `catalog::{tools, commands}` read the sources too, and a tool's `spec.meta` lands in `CatalogEntry.meta` beside `description`.
3. **Hook points and the policy view** (kernel) — `on_compact(Before)` before `compactor.compact`, `After` once the cut is absorbed or discarded; `on_session(Start)` when the actor opens, `End` when it closes (on the tracker, before `done`); `on_event(frame)` for every durable frame on one ordered task per session fed by a channel — publishing never waits on it; `before_tool`'s `Ask{reason}` opens the permission with `reason` as its summary; `ConfigView.plugins[policy.id()] = policy.describe(session)` published at open, after every verdict and after every command, only when it differs from the last; `TurnRetrying.max` folded into `LiveTurn`.
4. **`bingo-hooks-shell`** (worker A) — config claim `hooks` (Claude Code's shape: event → `[{matcher, hooks: [{type: "command", command, timeout}]}]`, `Merge::ByName` per event, lists accumulate); one `Hook` whose matcher is every point; events → sdk points: `PreToolUse` → `before_tool` (`permissionDecision: allow|deny|ask`, `permissionDecisionReason`, `updatedInput` accumulating over the hooks, exit 2 → `Deny` with stderr as the reason, other non-zero → warn and continue), `PostToolUse` / `PostToolUseFailure` → `after_tool` (`decision: block` → `Block`), `UserPromptSubmit` → `on_submit` (block → `Deny`; `additionalContext` appended to the text), `Stop` → `on_stop` (block once), `PreCompact` → `on_compact(Before)`, `SessionStart` / `SessionEnd` → `on_session` (1.5 s for `End`), `Notification` → `on_event(Notice)`, `PermissionRequest` → `on_event(InteractionOpened{Permission})`. Stdin JSON per event: `hook_event_name`, `session_id`, `cwd`, and the point's own fields (`tool_name`, `tool_input`, `tool_response`, `prompt`, `trigger`, `source`); verified against the current Claude Code hooks reference, date in the module doc. `BINGO_ENV_FILE`: a `SessionStart` hook may write `KEY=VALUE` lines to the path in that variable; they reach every later hook's environment. The matcher is a whole-string regex over the tool name, empty matches all, a bad regex falls back to equality with one warning. Timeouts: 60 s default, per-hook override; stdin written concurrently with the wait (a 64 KB `tool_input` and a hook that never reads must not deadlock); a hook past its deadline is killed. Every stdin shape is a snapshot fixture.
5. **`bingo-skills`** (worker B) — layers `<config_dir>/skills/<name>/SKILL.md`, `.bingo/skills` from the git common root down to cwd (nearest wins), the bundled `guide` last; frontmatter via `serde-saphyr` (`name`, `description`, `argument-hint`, `arguments: [names]`, `allowed-tools` and `model` read and recorded, not enforced); a `CommandSource` minting one `/name` command per skill (`Prompt{body}` with `$ARGUMENTS`, `$1..$9` and the named arguments substituted, `${BINGO_SKILL_DIR}` the skill's directory; `ArgSpec::Free{hint: argument-hint}`); the `Skill` tool (`{name, arguments?}` → the same body as its result; unknown name → error listing the names); a `System` contributor (order 5) listing `name — description` per skill; rescan when a directory or file stamp (len, mtime) changed; the bundled guide rewritten for this product from `AGENTS.md`, `ARCHITECTURE.md` and the M0–M7 plans (what it is, commands, permissions, sessions, hooks, skills, MCP), ≤200 lines.
6. **`bingo-mcp`** (worker C) — `rmcp` 3.1 (`client`, `transport-child-process`, `transport-streamable-http-client-reqwest`); config claim `mcpServers` (`ByName`: `{command, args, env, cwd?}` or `{type: "http", url, headers}`), `disabledMcpServers` (`Accumulate`); `start` spawns one dial per server with a 5 s timeout, concurrently, and returns at once; a `ToolSource` answering from what has landed; tools `mcp__<server>__<tool>` with `spec.meta.server`, `ToolTraits::default()` (untrusted), `call` → `tools/call`, content blocks → `ContentPart`s (text, image), `isError` → `is_error`; a stdio server's stderr → `<data_dir>/logs/mcp-<server>.log`; `/mcp` (instant) → `View::Table` of server, status (`connected N tools` / `failed: why` / `disabled` / `connecting`), `/mcp reconnect <server>`, `/mcp enable|disable <server>` (`Applied`); a failure is reported once as a `Notice`? — no event API for a plugin: the status is the `/mcp` table and the startup log line. Tests use `rmcp`'s `server` feature as a dev-dependency to run a scripted stdio server in-process.
7. **TUI** (worker D, small) — the footer badge from `state.config.plugins["bingo.permissions"]["mode"]`, shift+tab submits `/permission <next of the five>`, `retrying N/M` from `LiveTurn.retrying`, snapshots for both.
8. **bin** (kernel) — register the three plugins (hooks before tools; MCP after the permissions policy); `--mcp-config <path>` adds a settings layer holding the file's `mcpServers` (OpenClaw's `bundleMcp`); `scripts/budget.toml` to 290 with the number recorded.

## Files

`docs/adr/0009-contribution-sources.md`, `crates/bingo-sdk/src/{plugin,tool,model,policy,state}.rs`, `crates/bingo-core/src/host/{registry,catalog}.rs`, `crates/bingo-core/src/{host,turn,session}.rs` + `session/{commands,hooks}.rs`, `crates/bingo-hooks-shell/**`, `crates/bingo-skills/**`, `crates/bingo-mcp/**`, `crates/bingo-permissions/src/lib.rs` (`describe`), `crates/bingo-surface-tui/src/{view,input,keys}.rs`, `crates/bingo/src/main.rs`, `crates/bingo/tests/cli/{hooks,mcp}.rs`, `scripts/budget.toml`, `schema/rpc.json`.

## Dependencies

`rmcp` 3.1 (in `bingo-mcp` only; `check_discipline.sh` already keeps it out of the kernel's tree; the `server` feature only as a dev-dependency), `serde-saphyr` 1.1 (in `bingo-skills`; `serde_yml`/`serde_yaml` stay banned in `deny.toml`), `regex` (already a workspace dep) for hook matchers. `budget.toml` `max_dependencies` 260 → 290; the measured number goes in Verified.

## Exit criteria

- [x] `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `scripts/check_discipline.sh`, `scripts/budget.sh`, `cargo deny check`
- [x] Sources (kernel): a `ToolSource` that starts empty and later answers `Echo2` is absent from the first turn's request and present in the next; a source tool named like a static one is dropped with `TOOL_SHADOWED`; a `CommandSource` command runs as `/name` and appears in `catalog/read Commands`; `catalog/read Tools` carries `spec.meta`
- [x] Hook points (kernel): `on_compact` Before and After bracket one compaction; `on_session` Start at open, End before `wait_closed` returns; `on_event` sees every durable frame in seq order and a hook that sleeps 200 ms delays no frame; a `before_tool` `Ask{"why"}` opens a `Permission` whose summary is `why`, and `AllowOnce` runs the tool (the P0 regression)
- [x] `ConfigView.plugins["bingo.permissions"]["mode"]` is the configured mode at open and changes on `/permission acceptEdits`; the stream-json init line's `permissionMode` says the same
- [x] hooks-shell: one stdin snapshot per event; deny / ask / updatedInput / exit 2 / timeout kill / bad regex; the 64 KB case finishes; `SessionStart` writing `BINGO_ENV_FILE` is seen by the next hook; a `Stop` block loops the turn once
- [x] skills: layer order and override; substitution table; `/name a b` becomes a turn whose user item is the body; the `Skill` tool returns it; the contributor's block lists every skill once; an edited `SKILL.md` is seen without restart; the bundled guide parses and describes this product
- [x] mcp: the scripted server's tool is `mcp__test__echo` in the catalogue with `meta.server`, in the next turn's request, asked for by the gate, run on `AllowOnce`, and `Bash`-like traits never read-only; a hanging server fails within 5 s without delaying start; stderr in the log file; `/mcp` table; `/mcp reconnect`; `cargo tree -p bingo-core -e normal` has no `rmcp`
- [x] TUI: badge and `retrying 2/10` snapshots; shift+tab submits `/permission acceptEdits` from `default`
- [x] Black-box: `--print` with a settings file whose `PreToolUse` hook denies `Write` → the tool result says so and the turn completes; `--mcp-config` naming a scripted server → `catalog/read Tools` over RPC lists `mcp__…`
- [x] sdk changed once (ADR-0009 lists what it touched)

## Non-goals

`SubagentStop` and hook `Redirect` (M8). MCP resources, prompts, sampling and OAuth for HTTP servers (M10 owns auth). Enforcing a skill's `allowed-tools` or `model`. A `/skills` browser. Hot-swapping a tool under a running call. Hook-driven `updatedPermissions`.

## Risks touched

R3 — `rmcp` is the heaviest plugin dependency so far; the budget rises once, with the number, and the relink check keeps the kernel isolated. R6 — an MCP tool is untrusted by construction (a property test: no MCP tool's traits ever read `read_only`); a shell hook's `ask` goes through the one gate. R1 — one sdk change. R4 — the hook stdin shapes are snapshots, so a wording change is a deliberate diff.

## Verified (2026-08-29, commit e3d1267 + the catalogue test that follows it)

```
$ cargo fmt --all -- --check                                        exit 0
$ cargo check --workspace --all-targets --locked                    exit 0
$ cargo clippy --workspace --all-targets --locked -- -D warnings    exit 0
$ cargo test --workspace --locked                                   exit 0
  core 133 · tui 124 · hooks-shell 98 · permissions 96+6 · provider-openai 80+15 · print 80 · tool-web 77
  tool-fs 69 · skills 68 · context 66 · tool-bash 60 · provider-anthropic 56+12 · mcp 46+14 · bin (cli 39 + rpc 11) 50
  store-jsonl 34 · provider-fake 19 · sdk 19 · rpc 16+19 = 1257 passed, 0 failed
$ scripts/check_discipline.sh                                       exit 0 (one warning: turn.rs 754 non-test lines)
$ scripts/budget.sh                                                 dependencies 264 (max 264, was 252; the plan allowed 290); relink isolation 0
$ cargo deny check                                                  advisories ok, bans ok, licenses ok, sources ok (one duplicate-version warning: process-wrap 9 via rmcp, 10 ours)
$ scripts/tui-smoke.sh                                              exit 0
$ tmux drive: the footer badge reads `default` at open, shift+tab → `acceptEdits` → `plan` through the real
  ack and ConfigChanged; `/gu` completes to `/guide` from the catalogue
```

Exit criteria, item by item:

- Sources (kernel): `a_source_tool_is_gathered_when_the_turn_starts_and_a_duplicate_is_dropped` (turn tests) — the source's tools are absent from the first request and present in the next, the duplicate `Echo` dropped with `TOOL_SHADOWED`; `a_command_from_a_source_dispatches_like_a_registered_one` (session tests); `the_catalogue_reads_the_sources_too` (host tests) — a source's tool with its meta and a source's command beside the built-ins.
- Hook points (kernel): `compaction_hooks_bracket_the_cut` (Start, End around one manual compaction); `session_and_journal_hooks_observe_without_delaying_anything` — Start first, End last, every frame the client saw but the deltas in seq order, and the client received them all while the observer was gated; `a_hook_that_asks_opens_a_permission_with_its_reason_and_allow_runs_the_tool` — the P0 regression.
- Policy view: `the_policys_view_is_published_at_open_and_after_a_verdict` (host tests) — in the snapshot at open, and `ConfigChanged` right after the permission receipt; the permissions plugin's `describe` unit test names mode, modes and rules; the stream-json preamble reads `plugins["bingo.permissions"]["mode"]` (no test of its own beyond the helper — the snapshot tests run without a policy view and see `default`).
- hooks-shell (98 tests): eleven stdin snapshots, deny / ask / `updatedInput` accumulating across two hooks / exit 2 with stderr / non-zero warn-and-continue / a 0 s deadline killing `sleep 30` and its grandchild / bad JSON / the 64 KB input against `exit 0` / the 13-case matcher table / `Stop` block by exit 2 and by JSON / `additionalContext` / `export FOO=bar` in `$BINGO_ENV_FILE` read by a later hook. Black-box: `a_pre_tool_use_hook_that_denies_stops_the_write_and_tells_the_model` (`cli/hooks.rs`).
- skills (68 tests): frontmatter shapes, layer order and override (nearest project wins, user over project, disk over bundled), the substitution table, the command's `Prompt`, the `Skill` tool and its unknown-name error, the contributor's block snapshot, an edited or newly written `SKILL.md` seen on the next query, the guide parses and stays under 200 lines. Black-box: `a_project_skill_is_a_command_whose_body_becomes_the_prompt` (`cli/skills.rs`) — `/hello world` submits `Say hello to world, warmly.`
- mcp (46 + 14 tests against a real `rmcp` server, `examples/echo_server`): `mcp__test__echo` with `meta.server`, echo, `isError`, cancel, env/cwd, stderr in `logs/mcp-test.log`, a `sleep 30` server `connecting` then `failed: timed out` at 5 s without holding `start` or the source, the `/mcp` table snapshot, reconnect, enable/disable, a bad `type` refused, the fail-closed traits property over arbitrary inputs. `check_discipline.sh` asserts `rmcp` is not in the kernel's tree. Black-box: `an_mcp_server_from_mcp_config_offers_its_tool_through_the_gate` (`tests/rpc.rs`) — the tool reaches the catalogue after the dial, the gate asks, `AllowOnce` runs it, the result is the echo.
- TUI (124 tests): footer snapshots with `acceptEdits` and `bypassPermissions` and one without a badge; the shift+tab table walks the list the policy published and refuses an unlisted mode with a notice; `retrying 2/10`.
- sdk changed once (7ebc52d) plus one correction the skills plugin exposed (`CommandSource::commands(cwd)`, 61a7a57), both in ADR-0009.

Found while integrating (each is a commit body too):

- `CommandSource::commands` had no `cwd`: which `/name`s exist depends on where the line is typed. The actor passes the session's; the catalogue, which has no session, passes the process's.
- Observers see every frame but the deltas, not only the durable ones: a `Notification` hook needs `Notice` frames, which the journal never keeps.
- The policy view is keyed by the *plugin* id (`bingo.permissions`), the same id that names its settings slice and its catalogue entry; the plan had said `permissions`.
- The view carries `modes`, the ordered list the policy accepts, so shift+tab walks the policy's list instead of a copy in the surface.
- `ByName` cannot key a hook rule (no `name`/`id`) and the merge recurses before it reads a mode, so the hooks plugin claims one `hooks.<Event>` key per event, all `Accumulate`, verified against the real merge.
- `/model` no longer publishes a `ConfigChanged` when the view did not change (ADR-0008 §4 amended).
- The dependency cap is the merged number (252 → 264: `rmcp` and its four transitive crates, `serde-saphyr` and three, three new workspace crates).

Open, carried forward:

- A plugin still cannot emit a notice: a server that never dials, a `SKILL.md` with broken YAML, are visible only in `/mcp`, in the log, or not at all. Next sdk change: a `notice` on `HostHandle` that records an `ItemBody::Notice` on the attached sessions (`ToolHost::record` already does it for a tool).
- `HookContext` carries no permission mode, `on_compact`/`on_session` only a `Phase`, `on_stop` no items — `permission_mode`, `trigger`, `source`, `end_reason`, `last_assistant_message` are omitted or constant in the hooks' stdin. `PermissionRequest` hooks' own verdict is read and dropped (`on_event` returns nothing). Next sdk change.
- `CommandSpec` has no `description`, so a skill's description and its argument hint share `hint`.
- Skills: an explicit `name` wins over the directory name (the reference says the directory names the command); `$1` is 1-based here; no `\$` escape; no `BINGO_PROJECT_DIR`.
- MCP: HTTP URL redaction in failure text is byte-for-byte on the configured URL; a server named with a `/` gets no log file (stderr to null, warned); `process-wrap` is duplicated (9 via rmcp, 10 ours) until rmcp moves.
- hooks: `updatedPrompt` and plain stdout as context on `UserPromptSubmit` are not implemented; POSIX shells only.
- A `HostApi`/`ToolHost` test double now lives in four plugins; a `testing` feature in the sdk would end that.
- `turn.rs` is 754 non-test lines (warning).
- Live smokes against Anthropic and OpenAI (M1, M2) — still need keys; a real MCP server (`npx @modelcontextprotocol/server-filesystem`) has not been dialed by hand.
