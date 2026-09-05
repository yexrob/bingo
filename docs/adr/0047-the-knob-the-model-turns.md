# ADR-0047 — The knob the model turns

Status: accepted · 2026-09-05 · Plan: M76

## Context

User, 2026-09-05: can a spawn name the model and the effort; if not, let
an agent adjust its own effort or a child's, and pick a model and a
provider at spawn. The model and the provider have been `SpawnAgent`
fields since M8, and `ListModels` is how one is picked (ADR-0026). The
effort is not: `SessionSpec` carries provider, model, prompt and tools,
and the level lives beside it in the host (`Live.thinking`), filled from
the parent's at open and moved only by `/think`. A parent that wants a
child to think harder, or barely at all, can only write it into the
prompt — a second, lossy spelling of a kernel fact no request reads.
`docs/design/delivery.md` listed `thinking` among a definition's keys
from the start; it was never built.

`Change` — model, thinking, title — is the kernel's, and
`Host::reconfigure` is not a `HostApi` verb: three commands move the
knobs and nothing else can. The ratchet question (README): refusing
these doors forces a second representation — the level as prose in a
prompt, or `/think high` delivered as a line into a queue, a command
text standing in for `Change` behind whatever the session is doing. The
doors open.

## Decision

1. **The level rides the spec.** `SessionSpec.thinking:
   Option<Option<Effort>>` — absent inherits (the parent's level as it
   stands, else the settings'), `null` is off, otherwise a level: the
   spelling `thinking` already has in the settings file and the config
   view. The host resolves it once at open, where it resolves the
   model, and from then on the spec is the one holder: `Live.thinking`
   goes, `Change::Thinking` writes the spec, the choice reads it, a
   resume fills it from the config view as before.
2. **A spawn says it in words.** `SpawnAgent{thinking}` and a
   definition's `thinking:` take the seven words `/think` takes — `off`
   and the six levels — from one list on `Effort`, parsed by one
   function; a word that is not one is refused with the list. Call over
   definition over parent over settings, the order `model` follows.
3. **One verb for a knob.** `HostApi::reconfigure(session,
   SessionChange)`; `SessionChange` is the kernel's `Change` moved to
   the sdk — `Model{provider?, model}`, `Thinking(Option<Effort>)`,
   `Title` — and the three commands call the verb they always did,
   through the trait. It answers nothing: what the next turn runs on is
   read back from `session_model`, and every client learns it from the
   `SessionUpdated` and `ConfigChanged` that follow, as today. The
   default refuses; it joins neither the JSON-RPC wire (a client has the
   commands) nor the plugin bridge.
4. **The tool.** `SetThinking{level, agent?}` in `bingo-agents`: this
   session's level, or a child's by the name `SpawnAgent` gave back. Its
   traits are the plugin's — read-only, trusted, not concurrency-safe —
   because it changes what the next request asks for and nothing
   outside the process; what the session then does is gated in that
   session. It crosses to an ACP agent like any tool the deny list
   leaves in (`offer.rs`); such a call lands on the bingo session the
   agent answers, and ADR-0037 §1 carries it on.
5. **Between turns, never inside one** (ADR-0037 §4). The result says
   so: `thinking: high for reviewer, from its next turn`. For a session
   setting its own level that is the turn after this one; a child
   spawned at a level has it from its first. The inside-a-turn door
   stays shut: an ACP agent applies its knobs between prompts, and a
   level that turns thinking off in the middle of a tool loop is
   unverified against the provider's rules for the blocks it must hand
   back.

Amends ADR-0037 §3: there is a new tool and a new field after all; both
end where §3 says every path ends, in the next request's `reasoning`.

## Consequences

- The contract grows one optional field and one verb; `session/open` on
  the wire takes `thinking` without a method change, pinned by a
  contract test. A spec written before the field reads as absent.
- `Live` loses a field; `choose_model`, `model_for`, `inherited` and
  `Live::new` lose a parameter; `/think` loses its private parse and
  words.
- The tool cannot say what `/think` says about a model that does not
  declare reasoning: it reports the level it set and the moment it
  lands. `ListModels` says whether a model reasons.
- A child keeps the level it was opened at; `/think` on the parent
  afterwards does not reach a child already open, as `/model` does not.
- `SessionChange::Model` makes a `SetModel` tool one file away. Not
  built: a live session's model is a person's to move until asked.
