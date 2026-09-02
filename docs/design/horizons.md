# Horizons — decisions taken ahead of the code (2026-09-02)

> Direction settled with the user on 2026-09-02, before any of it is
> built. Each section names what already exists, what was decided, what
> was refused, and the ADR that lands when the work does. A milestone
> that starts from here reads its section first; a decision that turns
> out wrong is amended here with a date, not silently.

The rule that shaped every answer: **point at a door the kernel already
has before minting a new one.** Three consumers earn a trait (ADR-0015
waited for them); one imagined consumer earns a sentence in this file.

## 1. A hosted runtime

**Exists.** `SessionStore` is an sdk trait with seven methods; JSONL is
one plugin (`bingo-store-jsonl`), `MemoryStore` another. A Postgres
store is a third implementation and no kernel change.

**Decided.** Four things that sound like one are four layers:

| what | unit | how it moves online |
|---|---|---|
| the session (journal, summary) | `SessionStore` | a DB-backed store plugin |
| settings, credentials, `Env` | today: files under `data_dir`/`config_dir` | a `SettingsSource` and a `CredentialStore` trait, and `Env` as "a session's working ground" — minted when the first hosted deployment needs them, not before |
| the workspace tools act on | a machine | a sandbox or container per session; `tool-fs` and `tool-bash` run there unchanged |
| a database the model may query | a tool | `bingo-tool-sql` or an MCP server |

**Refused.** An "fs provider" abstraction under `tool-fs`: Bash needs a
real machine, so a remote filesystem gives the two tools two semantics.
The workspace is abstracted at the sandbox, never at the file call.

**ADR when built:** settings and credential sources; the DB store's
persisted shape (its own fixture test, ADR-0005's twin).

## 2. Many players on one kernel

**Exists.** The session is the only conversational noun. A person is a
session (each IM chat is one, ADR-0016); a room is a session without a
model; every input carries `Origin { surface, principal, conversation }`;
many attachments may hang on one tree and `answer` routes to whoever
asked (ADR-0010 §3); the serial room (ADR-0025) suits a turn-based game.

**Decided.**
- **No `Agent` trait.** What fills a seat is already abstract: a model
  (`Provider`), a person (a surface), or an external agent. An ACP agent
  such as Claude Code joins as a **`Provider` implementation** — an ACP
  client that sends the context and streams the reply back, running its
  own tools on its own side; it may live out of process (ADR-0030). One
  remote agent, one session.
- A game (werewolf, a tabletop session) is **rooms plus a rules plugin**:
  a hook at the room's fan-out deciding who sees a post, a command that
  advances the round, the board (ADR-0023) for state. The kernel never
  learns the word "game".
- Before a second person is let in over the wire, two gaps close:
  **authentication and authorization at the RPC surface** (a connection
  carries a principal; `submit` speaks only as it; `Interaction` gains
  an optional `for: principal` so only the one asked may answer), and
  **addressed delivery in `bingo-rooms`** (a post to a subset of seats).
- **A room and its members live in one kernel.** Scaling out is sharding
  by room tree, never routing between kernels. A constraint, not a
  mechanism.

**Refused.** Kernel-level identity, a game noun, cross-kernel routing.

**ADR when built:** wire authentication and the addressed interaction
(sdk: one optional field); addressed room delivery (plugin-local).

## 3. SDKs in three languages

**Exists.** Two schema-first, drift-tested contracts: `schema/rpc.json`
(drive bingo) and `schema/plugin.json` (be driven by it). A Python
plugin already runs against the latter with the standard library alone.

**Decided.**
- **TypeScript, Go, Python**, each an ordinary package of its ecosystem
  that links no Rust: `@bingo/sdk`, `github.com/<org>/bingo/sdks/go`,
  `bingo-sdk`. Each has two modules, `client` and `plugin`, one per
  schema.
- **Types are generated** from the schemas into `sdks/<lang>/generated/`,
  committed, regenerated in CI with `git diff --exit-code`
  (`json-schema-to-typescript`, `go-jsonschema`, `datamodel-codegen`).
  Hand-written code is the transport, the JSON-RPC envelope and a port
  of `SessionState::apply` — a few hundred lines each.
- **One conformance suite before the second SDK**: `schema/fixtures/`
  holds frame sequences and the folded state they must produce; every
  SDK runs the same files. Without it three `apply`s drift.
- **The binary ships beside the SDK the way each ecosystem expects**:
  npm through `optionalDependencies` on `@bingo/bin-<platform>` (one per
  release archive, the esbuild pattern); PyPI through `bingo-bin`
  platform wheels; Go finds `bingo` on `PATH` or a gateway socket and
  packs nothing.
- **One version fact: the tag.** `release.yml` grows `sdk-ts`,
  `sdk-python`, `sdk-go` jobs after `build`, beside `release`, consuming
  the same artifacts; `package.json`/`pyproject.toml` are committed at
  `0.0.0` and stamped from the tag in the job, never written back; the
  Go job pushes `sdks/go/v<tag>` on the same commit. PRs run only the
  touched SDK's tests by path; publishing is always the whole set.

**Refused.** Hand-written types; an SDK-only release flow; shipping the
binary inside the Go module.

**ADR when built:** the conformance fixture format (a persisted format).

## 4. The stream as a bus

**Exists.** Per session: journaled first, then broadcast to bounded
subscribers; `seq` is the offset, `events_since(seq)` the catch-up,
`Lagged` the overflow marker — log semantics already. Usage is on the
stream and in the journal: `TurnUsage { usage, context }` per round with
input, output, cache-read, cache-write and reasoning tokens, and
`TurnCompleted { usage }` per turn; cache hit rate is
`cache_read / input_total`, derivable live or from history. Tool
durations come from item timestamps; retries from `TurnRetrying`.

**Decided.**
- **A host-level stream** with its own `seq`, journaled and broadcast
  like a session's: session opened/closed, `CatalogChanged`, plugin
  start/stop. M35's catalogue refresh is its first publisher.
- **Subscription filters on the wire** — by session, event kind,
  plugin — so a consumer that wants tool calls does not drink the lot.
- **`bingo-outbox`**, a plugin that forwards frames to Kafka, NATS or a
  webhook with the `seq` as its cursor, at-least-once, resuming after a
  restart. The bus stays behind the plugin; the kernel never hears of it.
- **Two additive fields**: `TurnUsage` gains latency (`ttft_ms`,
  `elapsed_ms`); `TurnStarted` gains the `{provider, model}` the turn
  runs on, so a report costs per turn without replaying `ConfigChanged`.
- **Compatibility is additive-only** within a schema version: no variant
  removed, no field re-meant, new fields `#[serde(default)]`. Written
  into ADR-0002 when the host stream lands.
- "Every event, open": `docs/events.md` generated from `schema/rpc.json`
  — each event, its fields, when it fires, whether it is journaled.

**Refused.** A broker inside the process; cost in the kernel (usage ×
a price table is the outbox's or a report's arithmetic — prices change,
proxies set their own).

## Order

| # | work | why first |
|---|---|---|
| 1 | host stream + wire filters | §1, §2 and §3 all stand on it; M35 is already pushing on the door |
| 2 | settings/credential sources + a Postgres store | the real threshold of a hosted runtime |
| 3 | ACP-client `Provider` | zero kernel change; proves "no `Agent` trait" |
| 4 | `bingo-outbox` + the two usage fields | one file once 1 exists |
| 5 | conformance fixtures, then the TS SDK, then Go and Python | on the first external consumer |
| — | wire auth + addressed interactions + addressed room delivery | before a second person is let in over the wire |
