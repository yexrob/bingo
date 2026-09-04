# Architecture

One sentence: a minimal kernel — session actor, ordered event journal (the event hub), turn state machine, permission gate, plugin host — with everything else as plugin crates behind stable traits; every surface is a client of one submission entry and one subscription.

```
bingo (bin)                     composes Vec<Box<dyn Plugin>>, picks a Surface
├── bingo-core                  kernel: session actor · journal + broadcast · turn state machine
│                               · permission gate · tool executor · plugin host · ContextUsage ruler
│                               · ContextView::fold (journal → provider messages)
├── plugins (each its own crate, depends on bingo-sdk and the libraries only)
│   providers   bingo-provider-fake · -anthropic · -openai (openai + codex) · -acp (an agent as a model)
│   tools       bingo-tool-fs · bingo-tool-bash · bingo-tool-web · bingo-mcp · bingo-agents
│   policy      bingo-permissions · bingo-hooks-shell
│   session     bingo-store-jsonl (journal + index) · bingo-context (compactor + memory)
│   features    bingo-skills · bingo-rooms · bingo-tasks · bingo-experience · bingo-schedule   (a team is resident agents: bingo-agents)
│   surfaces    bingo-surface-print · bingo-surface-rpc · bingo-surface-tui · bingo-acp · bingo-channels
│   demo        bingo-demo-ui               off unless `--demo-ui`: the worked example of ADR-0013's
│                                           three lanes, and what a plugin author reads first
├── libraries (`tier = "library"`: register nothing, depend on bingo-sdk and each other, ADR-0042 §2)
│   bingo-auth-oauth            PKCE redirect · device code · auth.json · single-flight refresh (ADR-0012)
│   bingo-loopback              a port on 127.0.0.1 · one request at a time · the page a tool holds open
│                               until the person answers it · the browser opener (ADR-0042)
│   bingo-pictures              a picture as pixels: whatever a decoder reads, as the PNG a terminal
│                               takes and the type a provider accepts · a path or a URL this machine
│                               fetches · the one place that knows a decoder (ADR-0041)
└── bingo-sdk                   stable API: ids · Message/ContentPart · Frame/Event/Item · SessionState + apply
                                · traits (Plugin, Provider, Tool, PermissionPolicy, Hook, ContextContributor,
                                  Command, Surface, SessionStore, Compactor) · HostApi · Service registry · testing fakes
```

Dependency direction is strictly downward; the forbidden edges are listed in ADR-0001 and asserted by `scripts/check_discipline.sh`.

Data flow for one turn: a client calls `SessionHandle::submit(intent, input)` (synchronous, returns nothing) → the session actor appends a user `Item`, mints `seq`, and hands `Run::Turn` to the turn state machine → the loop asks contributors for context, streams the provider, folds `ModelEvent`s into `Item`s, gates each tool call through hooks and the policy (an `Interaction` when a person must answer), executes tools, absorbs queued steering at the barrier, and closes with exactly one `TurnCompleted` → every frame is journaled first and then broadcast to each subscriber's bounded channel → every client folds frames with `SessionState::apply`.

Sessions are the only conversational noun. A sub-agent is a session with a `parent` link; a room is a session without a model. Both render through the same reducer and the same draw code.

Where to read next: `docs/adr/` for the decisions, `docs/plans/` for what is being built now, `docs/design/` for the full proposals and the research behind the library choices.
