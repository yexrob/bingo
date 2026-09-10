# ADR-0055 — The context an agent holds is the agent's to measure

Status: accepted · 2026-09-10 · Plan: M91

## Context

An ACP session keeps its conversation on the agent's side (ADR-0035): each
request carries the newest turn, and the transcript bingo journals is a
fallback the agent never reads. The kernel's ruler (ADR-0006) does not know
this. It measures the fallback, anchors on the turn's bill — which for an
agent is every API call of the turn summed, cache reads included, so a
`used` of 2.3 million against a window of 191 808 — and past 90 % it asks
for a summary that the provider refuses, three times, then trips the
breaker and says so on every turn. The status line reads `2,367,503/191,808
· /compact` in a session that is 40 % full. The window is wrong because no
catalogue names an agent's model; the number the adapter reports in every
`usage_update` (`used`, `size`) is read only as a stand-in when the bill is
missing. And when the agent compacts itself — Claude Code does, at
`window − min(max_output, 20 000) − 13 000` tokens, so 967 000 of a
million and 167 000 of 200 000 — the adapter says so as three text chunks,
`Compacting...`, `Compacting completed.`, `Compacting failed: …`, which
bingo journals as the model's prose.

Would refusing a new door force a second representation of a kernel fact?
Yes: without a capability the kernel compacts what it does not hold, and
without a reading the bill stands in for the context. The user's decision,
2026-09-10: bingo does not compact an ACP session; the window is the
adapter's or the settings'; the agent's own compaction is shown properly.

## Decision

1. **An endpoint may hold the context.** `EndpointCapabilities` gains
   `holds_context: bool` (default `false`) and `context_window:
   Option<u64>`, the window the endpoint last named for that model.
   `ModelCapabilities` carries `holds_context`. Where it holds, the kernel
   sends the newest turn and draws no line: no threshold compaction, no
   `CONTEXT_WARNING`, no overflow ladder, and `/compact` is refused in
   words — "`<provider>` holds this context and compacts it itself".
2. **A reading beats a bill.** `ModelEvent::Context { used, window }` is the
   endpoint's own count of what it holds, sent as often as it likes. The
   round that saw one reports it as `TurnUsage.context` — `used` and
   `window` as read, `trigger` equal to `window` because the kernel draws
   no line there (`ContextUsage.trigger` says so) — instead of the bill's
   input total plus an estimate. A window an endpoint names outranks the
   settings' declaration, the learned clamp and the catalogue: the server's
   word on itself is the fact the others guess at.
3. **A cut the endpoint made is a compaction item with nothing replaced.**
   `ModelEvent::Compacted { before, after }` records `ItemBody::Compaction
   { summary: "", replaced: 0, before, after, duration_ms: 0 }` — the same
   row every surface already draws — and no `Event::Compacted`, since the
   journal lost nothing and no generation advances. The context fold writes
   no note for a compaction with no summary.
4. **The ACP provider reads the adapter.** Every `usage_update` is a
   `Context` reading; a reading whose `used` fell below the last one in the
   same session is preceded by `Compacted { before, after }`. The two
   `claude-agent-acp` banners `Compacting...` and `Compacting completed.`
   are the adapter's status and not the model's words: dropped as whole
   chunks. `Compacting failed: …` is kept as text, since it is what the
   person needs to read. The provider remembers the last `size` per model
   and names it in `endpoint()`, so the next turn's first frame is right.
5. **Nothing changes for a provider that holds nothing.** Anthropic, OpenAI
   and the fake keep `holds_context: false`, `context_window: None`; the
   ruler, the breaker and the ladder are as ADR-0006 left them.

## Consequences

- The status line of an ACP session shows the agent's count against the
  agent's window from the first `usage_update`; a Fable `[1m]` session
  reads `412k/1,000k`, and the `/compact` tail never shows before the agent
  has compacted itself.
- A refused `/compact` is the honest answer, not a forwarded one: mapping
  ACP slash commands stays outside (ADR-0035 §6).
- `guide-acp`'s "no compaction" sentence becomes "the agent's own".
- Amends ADR-0035's first consequence (the ruler shapes nothing for a held
  context) and ADR-0006 §2 (a reading replaces the anchor where the
  endpoint holds the context).
