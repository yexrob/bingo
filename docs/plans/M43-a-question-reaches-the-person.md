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

- [x] Bypass: the agent's escalation is allowed with zero interaction
  frames and zero notices.
- [x] Interactive: the question renders with the agent's own labels;
  the person's choice reaches the agent; the journal holds the
  interaction like any gate question's.
- [x] Headless non-bypass: fail-closed option, one notice, as today.
- [x] The kernel spells no permission-mode word (grep in review).
- [x] Every AGENTS.md gate; no new dependency; the RPC method table
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

## Verified

**The kernel words** are `Stance` + `PermissionPolicy::stance`
(`bingo-sdk/src/policy.rs`, defaulting to `Ask`), `HostApi::ask`
(defaulted, off the wire) and `bingo-core/src/host/ask.rs`: live session
or nothing, the policy's stance first, then the same `Msg::Ask` the gate
rides — and a question the mailbox could not put comes back as the
refusing option rather than as an error. `QuestionOption.role` carries
which option is which, because `AnswerSpec` is fieldless and could not.

**The mapping** (`bingo-provider-acp/src/question.rs`, new): one
`session/request_permission` is one `InteractionKind::Question` — the
agent's option ids verbatim, its `name`s as the labels, its
`toolCall.title` as the question, the adapter's name as the header, and
nothing of this client's added to any of it. Two options are marked, the
narrowest of each kind: `allow_once` (else `allow_always`) is
`Allowing`, `reject_once` (else `reject_always`) is `Refusing`, and
every other option — the `*_always` variants wherever a narrow one
exists — is a person's alone, so a session answering for somebody can
never install a standing decision the agent offered a narrow
alternative to. `answers()` is `[Choice, Cancel]`: without `Cancel` the
accept-check would reject a headless surface's decline and the question
would wait for ever (the caller contract on `HostApi::ask`).

**The way back**: only an `Answer::Choice` naming an id the agent itself
offered becomes `Selected(id)`. `Cancel`, an id the agent never named,
an `Err` from the door and a conversation with no session behind it (a
cold probe) are all *no answer*: the agent gets `refusal::refused` — its
own `reject_once`, else `reject_always`, else ACP's `Cancelled` — and
one `ACP_ASKED` notice per adapter session says so. A question that came
back with an option is never notice-worthy, so bypass and `dontAsk` say
nothing: the person decided those in advance.

**Where the weight went**: `Inbox` moved out of `session.rs` into
`inbox.rs`, which now owns the conversation's client half and the asking;
`session.rs` is back under the warn line (757 → 683 non-test lines).
`refusal.rs` kept the fail-closed half and lost its own copy of "which
option is the agent's no" to `question::refusing` — one reading, two
callers.

**Decided beyond the plan.** (a) An option with no `kind` cannot
happen: ACP v1 makes `PermissionOption.kind` required with no
catch-all, so such a request fails to parse and is answered
`invalid_params` before any of this runs. (b) The notice says "got no
answer" rather than "nobody was there", because a person who leaves the
prompt reaches the same place as a headless run. (c) The interactive
black-box lives in `bingo/tests/acp_asked.rs` and drives `serve
--stdio`: the `--print` host protocol declines every interaction that is
not a tool `Permission`, so no `--print` surface can pick an option.
What is verified there is the frame and the journal, not painted bytes;
the TUI renders any `Question` through `dialog::question_options`, which
this changes nothing about.

```
$ cargo test -p bingo --locked --test cli acp::asking
running 4 tests ... ok. 4 passed; 0 failed; finished in 0.51s

$ cargo test -p bingo --locked --test acp_asked
running 1 test ... ok. 1 passed; 0 failed; finished in 0.28s

$ cargo test -p bingo-core --locked --lib host::tests::ask
running 7 tests ... ok. 7 passed; 0 failed; finished in 0.01s
  (allow, refuse, ask, an unanswerable ask, an interrupt under one,
   a session this host does not run, a question marking no option)

$ cargo test -p bingo-permissions --locked
running 98 tests ... ok. 98 passed; 0 failed; finished in 0.80s
  (tests::the_stance_is_the_mode_this_session_chose_and_no_other_session_s)

$ grep -rEinw 'bypass|bypassPermissions|dontAsk' \
      crates/bingo-sdk/src crates/bingo-core/src --include='*.rs'
  (nothing — scripts/check_discipline.sh §4d asserts it)

$ cargo fmt --all -- --check                       # clean
$ cargo clippy --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile ...                     # exit 0
$ cargo test --workspace --locked                  # 3325 passed, 0 failed
$ scripts/check_discipline.sh                      # discipline ok
$ scripts/budget.sh                                # 310 (max 310), budget ok
$ cargo deny check                                 # advisories/bans/licenses/sources ok
$ cargo check -p bingo-provider-acp --all-targets \
      --target x86_64-pc-windows-msvc               # exit 0 (nothing here is
                                                    #  platform-shaped; run anyway)
```
