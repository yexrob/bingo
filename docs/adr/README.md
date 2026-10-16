# Architecture decision records

One record per boundary decision: a trait shape, a wire format, a persisted format, a dependency, a crate split, a threshold family. Template: Context (≤10 lines) / Decision (≤15) / Consequences (≤10) / Supersedes. Hard cap 120 lines; longer material goes to `docs/design/` and is linked. Bug fixes are commit bodies, not ADRs.

An ADR that opens a kernel door — a new `HostApi` verb, a new field on
a kernel type, a new trait verb the kernel calls — must answer one
question in its Context, in writing: **would refusing this door force
a second representation of a kernel-owned fact somewhere else?** If
the honest answer is no, the door stays shut (ADR-0039 is the ratchet's
first record; 0036 §2 answered it before it was named).

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
- [0034 — The room is read, not delivered](0034-the-room-is-read-not-delivered.md)
- [0040 — A picture beside the words](0040-a-picture-beside-the-words.md)
- [0041 — The picture from anywhere](0041-the-picture-from-anywhere.md)
- [0042 — The page that answers back](0042-the-page-that-answers-back.md)
- [0043 — The binary that replaces itself](0043-the-binary-that-replaces-itself.md)
- [0044 — One fact, one file](0044-one-fact-one-file.md)
- [0045 — The turn that can be undone: one kernel verb, the files a plugin's](0045-the-turn-that-can-be-undone.md)
- [0046 — The client opens the browser](0046-client-delegated-browser.md)
- [0047 — The knob the model turns](0047-the-knob-the-model-turns.md)
- [0048 — Stable request prefixes](0048-stable-request-prefixes.md)
- [0049 — Memory has one writer](0049-memory-has-one-writer.md)
- [0050 — An MCP server that needs signing in](0050-mcp-servers-that-need-signing-in.md)
- [0051 — The chat carries more than words](0051-the-chat-carries-more-than-words.md)
- [0052 — The picture is a file](0052-the-picture-is-a-file.md)
- [0053 — A room has one purpose, and four verbs](0053-the-room-s-verbs.md)
- [0054 — The plugin writes its own page](0054-the-plugin-writes-its-own-page.md)
- [0055 — The context an agent holds is the agent's to measure](0055-the-context-an-agent-holds.md)
- [0056 — A request is cut for silence, not for size](0056-a-request-is-cut-for-silence.md)
- [0057 — A plugin is switched, not deleted](0057-a-plugin-is-switched-not-deleted.md)
- [0058 — Settings are TOML, and the file keeps its comments](0058-the-settings-file-keeps-its-comments.md)
- [0059 — The agent has a view of its own](0059-the-agent-has-a-view-of-its-own.md)
- [0060 — An agent ends where it began: by whoever started it](0060-an-agent-ends-where-it-began.md)
- [0061 — The picture the model has seen](0061-the-picture-the-model-has-seen.md)
- [0062 — The picture a model is sent](0062-the-picture-a-model-is-sent.md)
- [0063 — The shell on Windows is Git Bash](0063-the-shell-on-windows-is-git-bash.md)
- [0064 — One fold for the transcript, one for the tree](0064-one-fold-for-the-transcript-one-for-the-tree.md)
- [0065 — The record is not the live type](0065-the-record-is-not-the-live-type.md)
- [0066 — Resume starts at a checkpoint](0066-resume-starts-at-a-checkpoint.md)
- [0067 — The picture is kept once](0067-the-picture-is-kept-once.md)
- [0068 — Every task has an owner](0068-every-task-has-an-owner.md)
- [0069 — A seam has a type](0069-a-seam-has-a-type.md)
- [0070 — A plugin places its own view](0070-a-plugin-places-its-own-view.md)
- [0071 — Collaboration is one library](0071-collaboration-is-one-library.md)
- [0072 — Mechanism lives in the library tier](0072-mechanism-lives-in-the-library-tier.md)
- [0073 — A channel is an adapter](0073-a-channel-is-an-adapter.md)
- [0074 — Every dispatch is a chain](0074-every-dispatch-is-a-chain.md)
- [0075 — Plugin protocol 2](0075-plugin-protocol-2.md)
- [0076 — An agent is not a model](0076-an-agent-is-not-a-model.md)
- [0077 — One lifecycle vocabulary for an agent](0077-one-lifecycle-vocabulary.md)
- [0078 — A mod is a module the host runs](0078-a-mod-is-a-module-the-host-runs.md)
- [0079 — A view is drawn where it is asked for](0079-a-view-is-drawn-where-it-is-asked-for.md)
- [0080 — The desktop app is a surface in this repository](0080-the-desktop-app-is-a-surface-in-this-repository.md) — rejected
- [0081 — The screen is the person's](0081-the-screen-is-the-persons.md)
- [0082 — Every place on screen is a site](0082-every-place-on-screen-is-a-site.md)
- [0083 — The chat is a surface](0083-the-chat-is-a-surface.md)
- [0084 — The places you act are sites](0084-the-places-you-act-are-sites.md) — proposed
- [0085 — Choosing a model without leaving the chat](0085-choosing-a-model-without-leaving-the-chat.md)
- [0086 — Built-in features are mods](0086-built-in-features-are-mods.md) — proposed
- [0087 — What a repository brings waits for a yes](0087-what-a-repository-brings-waits-for-a-yes.md)
- [0088 — A plugin without tools switches now](0088-a-plugin-without-tools-switches-now.md) — proposed
- [0089 — A session is forked from the list](0089-a-session-is-forked-from-the-list.md) — proposed
- [0090 — The crew acts through services](0090-the-crew-acts-through-services.md) — proposed
- [0091 — Rewind names what it takes back](0091-rewind-names-what-it-takes-back.md) — proposed
- [0092 — The person may read and forget](0092-the-person-may-forget.md) — proposed
- [0093 — Schedules are the person's too](0093-schedules-are-the-persons-too.md) — proposed
- [0094 — A schedule carries who asked for it](0094-a-schedule-carries-who-asked.md)
- [0095 — The frame follows the approved prototype](0095-the-frame-follows-the-prototype.md) — proposed
- [0096 — The owner hears its disable](0096-the-owner-hears-its-disable.md)
