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

- [ ] `/thinking high` mid-session: exactly one `set_config_option`
  with the agent's own value id before the next prompt; the fake
  agent's record proves order.
- [ ] `/model` lists the agent's declared models under the instance;
  choosing one applies it; `agent` applies nothing.
- [ ] An agent with neither knob: zero config calls, one notice.
- [ ] A clamped value is said in the option's own words.
- [ ] Every AGENTS.md gate; no new dependency (budget unchanged).

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
