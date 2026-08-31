# Plugin interdependency and openness (research, 2026-09-01)

> Source: two subagent deep-dives into `deepseek-ai/deepseek-harness`, condensed
> with their citations. Feeds the services-v2 / openness ADRs. The registry
> order-independence fix (`3f7a31e`) already landed from this pass.

## The subject

`deepseek-ai/deepseek-harness` ("dsh", *"Everything is a Plugin"*): ~206K
stars, ~250-package TS monorepo, 18 days old at reading. It did not invent its
plugin system — it vendored **Cordis** (2022, Koishi ecosystem) and made
everything a plugin on top. External PRs are refused; the plugin API is the
only contribution channel, which is why the surface is total. Reality checks:
~3,000 clone-verified real plugins (not the 12.9K topic count), the official
packaging example 404s, the curated index never existed publicly, and the
stability story is a shouted anti-guarantee (lockstep 0.x versions, no
changelog, no deprecations). Learn the mechanisms, not the maturity.

## Mechanisms verified worth the trip

1. **Service-key dependencies, never imports.** `inject: ['shell']` gates
   activation; undeclared access refuses at runtime; types ride a contract
   package both sides depend on (consumer peer-depends the seam, the
   implementation appears only in devDependencies). Rust mapping is exact:
   contract trait in a small crate, provider and consumer crates that never
   depend on each other. The three-role vocabulary — Definition / Provider /
   Consumer, "one role alone is not a seam" — is the discipline.
2. **Reactive resolution instead of topological order.** A fiber's epoch is
   derived from its providers; missing → PENDING, provider swapped → consumers
   re-run; a boot audit turns lingering PENDING into a loud startup error
   naming the missing services. We took the order-independence half as a
   fixpoint (`registry.rs standing()`); the boot-audit loudness is still owed.
3. **Fold-unit projections** (`sessionProjections.register{key, init,
   apply(state,event), stateVersion}`): the framework drives one subscription
   and folds every registered unit; domains hold no subscriptions; consumers
   read `state_of(session, key)` or a watermarked snapshot. Their hard-won
   constraints: `apply` synchronous; return the same value when uninterested.
   **This is the direct answer to our triplicated session-tree walk.**
4. **Extension points ranked by power.** Tier 0: `agents.setFactory` — the
   kernel's own agent loop registers through the same public call a plugin
   would use; a live third-party loop replaces full-transcript replay with
   bounded slices, re-emits the host's waterfalls, and fails loudly at boot
   against an incompatible invariant plugin. Tier 1: 14 `waterfall` hooks
   (around-middleware: `agent/pre-step` rewrites/rejects the request,
   `llm/stream` wraps the provider, `tools/execute` wraps dispatch,
   `tools/post-execute` replaces results). Tier 2: deny-only monotonic guards
   whose return type has no allow variant. Tier 4: 49 observe-only emits
   handing out cloned snapshots so a subscriber can never gain `cancel`.
5. **Provider wrapping needs no middleware chain** (modlens, 3.8K stars): an
   ordinary adapter resolves another provider *by id* and delegates,
   converting at request time so the durable log stays truthful. The missing
   bingo piece is one method: resolve `Arc<dyn Provider>` by id.
6. **Substrate seams** (`ctx.fs`/`ctx.subprocess`): the e2b family relocates
   all file/shell/PTY/LSP work to a remote microVM with zero new tools,
   because everything spawns through the seam. Their postmortem 0002 is our
   latent bug too: a permission mode that cannot confine an in-process fs
   path is a permission mode that lies.
7. **Scope layering**: global registry + per-scope shadowing/restriction,
   uniform across tools, prompt sections, commands, guards — a preset gives
   one session a different toolset without touching the global registry. The
   fail-closed detail worth copying exactly: a filtered tool is absent from
   the prompt AND refuses execution, indistinguishably from nonexistent.
8. **The one invariant above all: model-visible ⟺ logged.** Anything that
   reaches a model request must be reconstructable from the session log,
   runtime-enforced. Any request-rewriting hook we add must land its rewrite
   in the journal or resume/fork silently diverge.
9. **Generated capability catalogs with completeness guards**: every service
   key and event is extracted from source, classified in a checked-in table,
   and CI fails on an unclassified key. The docs cannot drift; an edge like
   "the bin's doctor knows the channels plugin" becomes visible and gated.
10. **`ctx.authorization`**: the plugin that knows how to obtain a credential
    registers a flow keyed by the record it writes; config carries env-var
    *names*, consumers resolve per operation. Exactly our wished-for
    "credential wants" seam.

## The trust model, honestly

One boundary matters: the process. In-process plugins get everything; the
sandbox confines spawned argv and fs mutations, never plugin code; their docs
say so plainly. The genuinely fenced things: append-only deep-frozen session
log, allowlisted model-facing projections, reserved names failing loudly,
MCP as the only real process boundary (tools only, env scrubbed). bingo is
**ahead at the process boundary**: bingo-plugin-rpc bridges tools + commands
+ Views over a committed, drift-tested schema; dsh third parties cannot ship
a slash command or UI node in a non-TS language, and dsh has no wire version.

## The four bingo pressures, answered

| Pressure | Answer |
|---|---|
| Tree walk ×3 (agents/rooms/tasks) | One projection unit (or one typed seam) owned by the kernel-driven fold; plugins read a key. |
| tasks resolves rooms' noun | rooms publishes the projection; tasks reads it — the spelling stays with its owner. |
| doctor knows bingo-channels | The authorization/credential-wants seam: plugins declare, doctor iterates the declarations. |
| Locks found by shape | Keep the shape (it worked), add the capability graph so the convention is checked, or a `claims()` contribution later. |

## Refused

Model-written dynamic plugins ("treat like bash access" — their words);
growing the string-keyed `Arc<dyn Any>` registry (we have traits; keys are
for the audit, not the lookup); the epoch/hot-reload machinery (reactive
declaration yes, reversible-everything no); seam proliferation (dsh's ~90
keys include single-consumer services their own docs warn about).

## The strategic question raised

dsh's kernel is *smaller* than ours: the agent loop is a default provider
behind a public `setFactory`. Ours is concrete in `bingo-core` and cannot be
displaced. "The kernel owns no feature nouns" is not yet "the kernel owns no
privilege". A swappable loop is also how one plugin breaks every other —
this wants a deliberate ADR decision, not a default.
