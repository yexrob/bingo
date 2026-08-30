# Architecture decision records

One record per boundary decision: a trait shape, a wire format, a persisted format, a dependency, a crate split, a threshold family. Template: Context (≤10 lines) / Decision (≤15) / Consequences (≤10) / Supersedes. Hard cap 120 lines; longer material goes to `docs/design/` and is linked. Bug fixes are commit bodies, not ADRs.

- [0001 — Crate map and dependency direction](0001-crate-map.md)
- [0002 — One event stream: frames, journal, reducers, intents](0002-event-stream.md)
- [0003 — Settings: three JSONC layers, merged per key by the claiming plugin](0003-settings.md)
- [0004 — Model facts: catalogue, endpoint, server](0004-model-facts.md)
- [0005 — Session persistence: JSONL journal, sidecar lock, derived summary](0005-session-persistence.md)
- [0006 — Context budget: kernel measures and cuts, plugin summarises and remembers](0006-context-budget.md)
- [0007 — The wire: JSON-RPC over NDJSON, methods 1:1 with HostApi, events verbatim](0007-rpc-wire.md)
- [0008 — Commands: parsed and dispatched by the session actor, outcomes as acks](0008-commands.md)
- [0009 — Contribution sources: tools and commands that exist only after I/O](0009-contribution-sources.md)
- [0010 — Sub-sessions: peer delivery, redirect, tree attachment](0010-sub-sessions.md)
- [0011 — Log sessions, plugin state in the journal, the host in hand](0011-log-sessions-and-plugin-state.md)
- [0012 — OAuth credentials: a library tier, one store, login as an interaction](0012-oauth-credentials.md)
- [0013 — UI as data: one view vocabulary, three lanes, actions](0013-ui-as-data.md)
