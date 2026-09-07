# 0048 — Stable request prefixes

## Context

Provider caches match consecutive rendered prefixes, not independently reusable
pieces of text. A late peer changed earlier user attribution, and mutable context
in System invalidated conversation history after it. Normal rolling result
elision did the same. Preserving a prefix does not justify filling the UI with
internal state. The journal already owns plugin state and context provenance;
private contributor caches would duplicate it. Existing extensions can hold a
captured knowledge baseline, and existing sourced pieces carry runtime changes.
A compactor cannot reconstruct the parent's request without duplicating the core
fold; its context therefore receives that actual request, not a second model field.

## Decision

1. Source attribution is a function of one item's origin, never future items.
   The person's own messages carry their identity from their first rendering.
2. Instructions and memory indexes stay in System as captured baselines owned by
   `bingo-context`. Private `_bingo.context` extensions store their rendered blocks
   and capture generation, once per contributor and lifecycle epoch. Session-start
   invalidation covers create/reopen; successful compaction or rewind advances the
   existing history generation. The next request lazily refreshes from disk.
   Ordinary turns reuse the capture; failed compaction does not refresh it.
3. Runtime reminders may use `ContextPiece::snapshot(id, text, items)` only on
   change. Journal items supply comparison state, including after compaction;
   event-style contributions are not deduplicated. Internal context is never a
   person’s utterance: no title/count input, user bar, injection row or expanded
   notice in ordinary UI. Extension namespaces beginning with `_` are internal:
   no panel-picker or pinned-card entry. The pre-release `bingo.context` namespace
   remains private too, so old journals need no rewrite. Raw RPC retains the data.
4. Normal requests do not elide old tool results. Full compaction retains its
   90% threshold and overflow recovery may still elide results on its retry.
5. OpenAI conversational requests derive `prompt_cache_key` from their session;
   side requests without a session invent no affinity. Explicit caller options
   retain precedence. Cache read, write and ordinary input counts stay disjoint.
6. New model-specific breakpoints, retention and effort-update protocols are not
   enabled on unknown compatible endpoints without capability verification.
7. `CompactContext.request` is the core-assembled normal/failed request. Summary
   calls retain its system, tools, history, reasoning, options and session affinity,
   then append a request-local summary instruction and bound output headroom.
   No normal tool loop runs; tool calls, missing/length finishes and cancellation
   cannot yield an accepted summary. Errors retain observed usage for billing.
8. `provider_options.bingo.purpose=compaction` marks that side call. ACP rejects
   it before touching a delegated session; a single local drain would not prevent
   that agent executing tools. Unknown explicit purposes also fail closed there.
   Plugin protocol 6 carries the SDK request and a tagged completed/failed result
   with observed error usage; it does not grant parent-provider credentials.
   Generic transport failures have no measured usage to report. Major-5 peers
   are rejected at handshake rather than accepted into an incompatible call.
   Overflow may use one disclosed shortened retry, then the explicit no-model
   fallback; it is not represented as full-prefix cache reuse.

## Consequences

A software upgrade changes attribution once; subsequent peer arrivals do not.
Disk edits do not immediately alter a captured baseline; tools can still read
current files. Reopen/compact/rewind refresh may deliberately invalidate that
prefix. Runtime updates grow the tail but are invisible as conversation entries.
Keeping results can reach compaction sooner. Summary input covers the full parent
request while replacement retains the existing tail, so some overlap is possible.
Live cache/cost improvements require measurement; an affinity key guarantees none.
No crate or dependency is added. A context-owned extension payload and the
request/observed-error compactor contracts have fixtures; the User shape is unchanged.

## Supersedes

ADR-0006 §1/§3's normal microcompaction and §7's mutable memory System indexes;
ADR-0014 §6's mutable experience System index. Their other decisions stand.
