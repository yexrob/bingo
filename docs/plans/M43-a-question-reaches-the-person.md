# M43 — A question reaches the person

## Goal

ADR-0039 built: `HostApi::ask` — one door onto the kernel's existing
interaction machinery; the session's permission policy answers first
through one neutral verb; the ACP provider maps
`session/request_permission` through it, so a bypass session allows
silently, an interactive one asks the person with the agent's own
option labels, and headless stays fail-closed.

## Bricks, in build order

**Kernel words (`bingo-sdk`, `bingo-core`) — worker Y, now**

1. The stance verb: read `bingo-permissions` and `bingo-core/src/
   gate.rs` first — the policy trait the gate consults gains one
   defaulted neutral verb (worker names it; the ADR's shape: how does
   this session stand toward a question no tool defines — ask, allow,
   or refuse). `bingo-permissions` implements it from its own modes;
   the kernel never spells a mode name. Default: ask (fail toward a
   person, not past one).
2. `HostApi::ask(session, kind, answers) -> Answer`, defaulted like
   `invoke`; routed to the session actor's existing ask machinery.
   The `answers` carry which option is the allowing one and which is
   the fail-closed one (an `AnswerSpec` fact, not a string
   convention — look at what `AnswerSpec` has and add the minimum).
   Stance allow → answer the allowing option without an interaction;
   refuse → the fail-closed one; ask → the interaction, rendered by
   whatever surface is attached, exactly as a gate question; an
   unanswerable ask (no surface, headless) meets the fate a gate
   question meets there today — find it and follow it, do not invent
   a timeout. Tests: all four paths, plus interrupt during an open
   question.

**The mapping (`bingo-provider-acp`) — after worker X's row-options
slice lands**

3. `session/request_permission` (refusal.rs's territory) becomes one
   `ask`: the agent's options map to `AnswerSpec`s (its allow option
   marked allowing, its reject marked fail-closed; kinds
   allow_once/allow_always/reject_once/reject_always keep their ids
   so the chosen one maps straight back). The once-per-session notice
   remains only on the unanswerable/refused paths.
   `elicitation/create` stays declined (ADR-0039 §3).
4. Black-box: a bypass session auto-allows (the fake agent's log
   shows the allow answer, the frames show no interaction); a
   default-mode host-driven session shows the interaction and an
   answered allow reaches the agent; headless default refuses as
   today. The fake agent's permission capability already exists from
   M38 — extend, do not rebuild.

## Files

`bingo-sdk/src/{host.rs,tool.rs or permissions place}`,
`bingo-core/src/{gate.rs,session/*,host.rs}`, `bingo-permissions/src/`,
`bingo-provider-acp/src/{refusal.rs,session.rs}`, fake agent,
`bingo/tests/cli/acp/*`; `docs/adr/README.md` template (done with the
ADR commit).

## Exit criteria

- [ ] Bypass: the agent's escalation is allowed with zero interaction
  frames and zero notices.
- [ ] Interactive: the question renders with the agent's own labels;
  the person's choice reaches the agent; the journal holds the
  interaction like any gate question's.
- [ ] Headless non-bypass: fail-closed option, one notice, as today.
- [ ] The kernel spells no permission-mode word (grep in review).
- [ ] Every AGENTS.md gate; no new dependency; the RPC method table
  unchanged.

## Non-goals

Elicitation. Answer memory ("always allow" beyond what the agent's
own allow_always option does on its side). Any wire exposure of `ask`.

## Risks

- The policy trait's shape may resist a defaulted verb; if it does,
  worker Y reports before touching more than the trait.
- Two doors opened this batch already; this is the third and the
  ratchet question is answered in ADR-0039's context — hold that
  line if the building tempts a fourth.
