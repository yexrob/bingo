# M40 — The knobs cross

## Goal

ADR-0037 built: an ACP session's effort and model can be adjusted from
bingo — `/thinking`, `/model`, `SpawnAgent`'s `model` field, any path
that reconfigures a session — applied to the agent between turns via
`session/set_config_option`. No kernel change.

## Bricks, in build order

1. **Option matching, pure** (`bingo-provider-acp/src/options.rs`,
   new): find the effort-shaped and model-shaped options in a
   `Vec<SessionConfigOption>`; map `Effort` to the option's own value
   set, clamped to nearest with the option's words; map a model name
   likewise. Match by the ids the real adapters use (verify against
   codex-acp and claude-agent-acp sources — the research names
   `REASONING_EFFORT_CONFIG_ID` and `effort`), falling back to
   name/category. Fixture tests, including an option list with
   neither.
2. **Applied state and the diff** (`session.rs`): the instance keeps,
   per session, what it last applied (effort and model). Before each
   `session/prompt`, diff against the request's `reasoning` and
   `model`; a change goes out as `set_config_option` first. `agent`
   as the model means "theirs" — nothing is sent. A knob the agent
   lacks: apply nothing, notice once per adapter session, remember.
   The legacy `session/set_model` is the fallback for a model change
   when the agent has that and no model option.
3. **The catalogue** (`provider.rs`): the models the agent declared at
   `session/new` (its model option's values, else its legacy list)
   are served as this instance's models through the ADR-0026 door
   (`ServedModels` — find how a provider's endpoint-answered list
   lands and ride the same path); `agent` is always first. Before any
   session, `agent` alone.
4. **The proof**: the scripted fake agent grows a `configOptions`
   capability (declare options at `session/new`, record
   `set_config_option` calls, answer). Black-box in
   `bingo/tests/cli/acp.rs`: a `/thinking` change mid-session reaches
   the fake agent as one `set_config_option` before the next prompt;
   a `/model` change likewise, and the model list shows the agent's
   own; an agent with no options gets no call and one notice; a spawn
   with `model: acp-x/their-model` opens the session already set.
5. `scripts/acp-smoke.md` gains the two live checks (codex: effort
   max→low visible in `/status`; model switch listed). Runbook only.

## Files

`bingo-provider-acp/src/{options.rs (new), session.rs, provider.rs,
config.rs?}`, `bingo-provider-acp/tests/` harness + `fake_agent.rs`,
`bingo/tests/cli/acp.rs`, `scripts/acp-smoke.md`,
`docs/adr/0035-an-agent-answers-as-a-model.md` (§6 pointer to 0037).

## Exit criteria

- [x] `/thinking high` mid-session: exactly one `set_config_option`
  with the agent's own value id before the next prompt; the fake
  agent's record proves order.
- [x] `/model` lists the agent's declared models under the instance;
  choosing one applies it; `agent` applies nothing.
- [x] An agent with neither knob: zero config calls, one notice.
- [x] A clamped value is said in the option's own words.
- [x] Every AGENTS.md gate; no new dependency (budget unchanged).

## Non-goals

A parent live-tuning a running child (no such path exists for any
provider; out of scope by the user's word). `system`, caching, token
counting (still ADR-0035 §6). Session modes and slash commands.

## Risks

- Option ids drift across adapter versions: matching falls back to
  name/category, and a miss is a notice, not a break.
- The model option's values may pair model and effort in one id
  (codex's `model[effort]`); brick 1 owns that spelling.
- M39's joint slice touches the same files; this milestone starts
  only after it lands.

## Verified

2026-09-03, macOS aarch64, branch `m40-knobs` on `dev` at aa9ee52.

**The ids, verified against the adapters' own sources** (not the plan's
guesses): codex-acp `src/ModelConfigOption.ts` has
`REASONING_EFFORT_CONFIG_ID = "reasoning_effort"` and
`MODEL_CONFIG_ID = "model"`, category `thought_level` and `model`, model
values plain slugs; claude-agent-acp `src/session-config-ids.ts` has
`EFFORT_CONFIG_ID = "effort"`, `acp-agent.ts` `MODEL_CONFIG_ID = "model"`,
same two categories, with a `default` sentinel first in both lists and
levels `low|medium|high|xhigh|max` from the SDK. The legacy door is
`session/set_model` with `{sessionId, modelId}`, `modelId` spelled
`model[effort]` (`src/AcpExtensions.ts`). Neither id is a gate: codex's
effort values are `ReasoningEffort = string`, supplied by its own server.

```
$ cargo fmt --all -- --check
$ cargo clippy --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 15s
$ cargo test --workspace --locked
test result: ok. 151 passed; …   (bingo-provider-acp lib)
test result: ok. 162 passed; …   (bingo --test cli, 24 of them acp::)
… 3244 tests, 0 failed
$ scripts/check_discipline.sh
kernel names no tool
cohesion ok
discipline ok
$ scripts/budget.sh
dependencies (unique, normal): 310 (max  310)
warm cargo check -p bingo-core: 0s (max  20s)
relink isolation: touching the TUI recompiled 0 crates for core (must be 0)
budget ok
$ cargo deny check
advisories ok, bans ok, licenses ok, sources ok
$ cargo check -p bingo-provider-acp --all-targets --locked \
    --target x86_64-pc-windows-msvc
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 22.45s
```

The eight black-box scenarios (`bingo/tests/cli/acp/knobs.rs`), each
driving the real binary against the scripted agent and reading its log:

```
test acp::knobs::a_thinking_change_reaches_the_agent_before_the_next_prompt
test acp::knobs::before_any_session_the_instance_serves_the_label_alone
test acp::knobs::the_model_list_is_the_agents_own_once_a_session_has_opened
test acp::knobs::a_model_change_reaches_the_agent_as_its_own_value
test acp::knobs::the_agent_label_applies_nothing
test acp::knobs::an_agent_with_neither_knob_gets_no_config_call_and_one_notice
test acp::knobs::an_adapter_with_only_the_legacy_door_is_set_through_it
test acp::knobs::a_spawn_that_names_the_agents_model_opens_the_session_already_set
```

Not verified: §5's two live checks are runbook text and were not run —
they need a login and a network (`scripts/acp-smoke.md`).

**Found in the building, and left standing.** A level reaches an ACP agent
only when the model declares reasoning, and the embedded snapshot cannot
know an agent's model — `models::resolve` reads
`declared.reasoning.unwrap_or(facts.reasoning)` and `UNKNOWN.reasoning` is
`false`, so `ModelRequest.reasoning` is `None` for `acp/agent` until
settings say `models."<row>/agent".reasoning = true`. `/think` already
tells a person exactly that line, and the black-box writes it; but it is a
kernel default, out of this milestone's scope to change, and the honest
statement of ADR-0037 §1 today is "with that line".
