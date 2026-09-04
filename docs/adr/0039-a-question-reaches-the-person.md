# ADR-0039 — A question reaches the person

Status: accepted · 2026-09-03 · Plan: M43

## Context

ADR-0035 §5 refused a prompter door: permissions are the agent's own,
a stray `session/request_permission` is answered with its reject
option, and "the need is recorded here, not built". The need arrived:
in real use a claude adapter running in its default mode has every
escalation refused while a person sits at the TUI — the agent itself
reports "nobody is present to approve, so it is treated as refusal".
The machinery to ask already exists and is all kernel-owned: the gate
opens interactions, surfaces render and answer them, `HostApi` carries
the answering half (`answer`) — only the asking half has no door.
Refusing the door forces every plugin that needs a human verdict to
invent its own question channel: a second representation of "a
question awaiting a person". (The ratchet this passes is now written
into the ADR template: a kernel door must show that refusing it forces
a second representation.)

## Decision

1. **One door.** `HostApi::ask(session, kind, answers) -> Answer`, a
   defaulted method like `invoke` and `notice`: hosts that run no
   sessions refuse it, the twenty test doubles compile untouched, and
   it does not join the JSON-RPC wire (the method table stays pinned).
   It routes to the session actor's existing ask machinery — the one
   the gate and the bridged-call door already use. No new interaction
   concept, a door onto an old one.
2. **The stance is the policy's, and the kernel spells no mode.** The
   asker names, among its `answers`, which one means "let it happen"
   and which is the fail-closed refusal. Before anything reaches a
   person the door consults the session's permission policy through
   one neutral verb (the policy trait grows it; bingo-permissions
   implements it): a stance of *allow* answers the allowing option at
   once — this is what a bypass session means and why bypass now
   propagates as a stance rather than as a translated mode name; a
   stance of *refuse* answers the fail-closed option; a stance of
   *ask* opens the interaction and the attached surface asks the
   person, exactly as a gate question is asked today. Unanswerable
   (no surface, a headless run): the fail-closed option, the same
   fate a gate question already meets there. The word `bypass` never
   enters the kernel — the policy owns its own vocabulary.
3. **The ACP provider maps, it does not judge.** A
   `session/request_permission` becomes one `ask`: the agent's own
   options become the answers (its allow option marked allowing, its
   reject option fail-closed), the person's choice maps straight
   back. The adapter's words cross untranslated in both directions.
   `elicitation/create` stays declined — free-form input is another
   interaction shape, recorded here, not built.
4. **The row still speaks first.** An adapter configured in its own
   words (`options: {"mode": "dontAsk"}`, an approval-policy env)
   never asks, and that remains the recommended shape; the door is
   for the agent that asks anyway.

## Consequences

- A bypass session's ACP agent gets its escalations allowed silently;
  an interactive session gets a real approval prompt in the TUI with
  the agent's own option labels; headless stays fail-closed. The
  refusal notice remains only for the unanswerable paths.
- The policy trait gains one neutral verb in the sdk — the same
  second-representation test: without it, every asker would have to
  learn the permission modes.
- MCP elicitation and future device-code prompts have their door
  ready; each is its own small mapping when its need arrives.
- No new dependency; the method table is untouched. The building found
  one wire ripple the decision missed: the role rides the question's
  own option (`QuestionOption.role`, optional — `AnswerSpec` is a
  fieldless spelling of an answer and could not carry it), so an
  unmarked option serializes as before and the schemas gained one
  optional field. Everything else is behind a defaulted method or a
  defaulted trait verb.
- *2026-09-04, M53:* questions asked together are one interaction, not
  a second door. `InteractionKind::Form { title, questions }` holds the
  same `Question` a lone one always was — extracted, so the bare
  variant's wire shape is byte-identical — and `Answer::Form` carries
  one answer per question in order; `answer_for` (§2) answers a form
  with every question's role option or none at all, so the stance and
  the fail-closed fate are what they were. A preview rides on the
  option it shows (`QuestionOption.preview`), never on the question.
  §3's declined `elicitation/create` is now built as exactly the
  mapping recorded there: a server's request becomes one form naming
  the server, the reply is `accept`/`decline`/`cancel`, and a schema
  the spec does not allow is declined with a notice.
- *2026-09-04, M59:* an answer carries both halves of what a person
  said. `Answer::Choice.other` holds the words typed beside the ticks
  (serde-defaulted and skipped when absent, so an old frame reads as
  the ticks alone, and words with nothing ticked stay `Answer::Text`).
  An elicitation property has room for one value, so there the words
  are what the server hears — recorded in the mapping's own doc.

Refs: ADR-0035 §5, ADR-0036 §2, ADR-0011; Plans: M43, M53
