# 0009 — Contribution sources: tools and commands that exist only after I/O

## Context

`Plugin::register` is synchronous and does no I/O (ADR-0001): the registry is fixed before any plugin starts. Two M7 plugins cannot say what they contribute until they have done I/O — an MCP server advertises its tools only after a handshake that may take seconds or fail, and a skills directory holds its `/name` commands on disk, and both change while the process runs (`/mcp reconnect`, a skill saved mid-session). The old project solved this with a session-level manager the executor consulted by name and a global skill cache read every turn. A client also has no way to read a plugin's per-session setting (ADR-0008: `/permission` sets a mode nobody can display), and `LiveTurn` drops the retry ceiling.

## Decision

1. **Sources.** Two contribution kinds resolve late: `Contribution::Tools(Arc<dyn ToolSource>)` with `async fn tools(&self) -> Vec<Arc<dyn Tool>>`, and `Contribution::Commands(Arc<dyn CommandSource>)` with `async fn commands(&self) -> Vec<Arc<dyn Command>>`. A source is registered synchronously and answers from whatever it has now; it is never wrong to answer with nothing. The kernel reads sources at the moments it needs the set: a turn gathers its tools once when it starts (static tools first, then every source, in registration order; a later duplicate name is dropped with a warning); the actor consults command sources when a name is not in the static table; the catalogue reads both. A source that blocks holds a turn's start, so a source answers from a cache and does its I/O elsewhere.
2. **MCP tools** are named `mcp__<server>__<tool>`, the form the permission grammar already reads; the catalogue entry carries `meta.server`. Their traits are `ToolTraits::default()` with `trusted: false`: `readOnlyHint` is a claim, never a fact, and the gate asks. A server's stdio stderr goes to `<data_dir>/logs/mcp-<server>.log`, never to the terminal.
3. **Skills are commands, a tool and a contributor.** Each `SKILL.md` is a `/name` command whose outcome is `Prompt{body with $ARGUMENTS, $1…, and the named arguments substituted}`; the `Skill` tool lets the model invoke one by name and returns the body; a `System` contributor lists the available skills with their descriptions. Layers: `<config_dir>/skills/<name>/SKILL.md`, then `.bingo/skills` from the git root down to cwd (nearest wins), then the bundled guide, which any disk skill of the same name overrides. Rescanned on directory or file mtime change.
4. **Hooks reach every point.** The kernel now calls `on_compact(Before|After)` around a compaction, `on_session(Start)` when an actor opens and `End` when it closes, and `on_event(frame)` for every frame but the deltas (notices included: a hook that wants `Notification` needs them, and the journal's durability is about the disk, not about who may watch), on one ordered task per session that never holds the actor. `HookOutcome::Ask{reason}` from `before_tool` opens a permission interaction whose summary carries the reason.
5. **A policy describes itself.** `PermissionPolicy::describe(&self, session) -> Value` (default `Null`) is what a client may show — the mode, the session rules. The kernel publishes it as `ConfigView.plugins[policy.id()]` on open, after every verdict and after every command, only when it changed. The policy's own map stays the one fact; the view is its projection.
6. `LiveTurn.retrying` becomes `Option<Retry{attempt, max}>`.

## Consequences

- sdk touched once: `ToolSource`, `CommandSource`, two `Contribution` variants, `PermissionPolicy::describe`, `LiveTurn.retrying` — plus one correction the skills plugin exposed: `CommandSource::commands` takes the session's `cwd`, because which `/name`s exist depends on where the line is typed; the catalogue, which has no session, asks for the process's directory. Crates touched: `bingo-core` (registry, turn, actor, catalogue, hooks), `bingo-permissions` (`describe`), `bingo-surface-tui` (the badge, `retrying N/M`), `bingo-surface-print` (the stream-json init line's `permissionMode` becomes real), `bingo-surface-rpc` (schema regenerates for `LiveTurn`).
- A turn's tool set is fixed for that turn; a reconnect shows up on the next one. Nothing hot-swaps a tool under a running call.
- Dependencies: `rmcp` 3.1 (`client`, `transport-child-process`, `transport-streamable-http-client-reqwest`), the first crate outside the kernel's tree with a runtime of its own; `serde-saphyr` 1.1 for frontmatter YAML. `scripts/budget.toml` rises to 290; `check_discipline.sh` keeps `rmcp` out of `bingo-core`'s tree.
- The kernel still knows no plugin by name: a source is a trait object like every other contribution, and `mcp__` is a string the permission grammar owns.

## Supersedes

—
