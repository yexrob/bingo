# ACP live smoke (M38, M39, M40)

The one thing the suite cannot do: run a real agent. Everything else about
`bingo-provider-acp` is proven offline against the scripted agent and recorded
bodies (`cargo test -p bingo-provider-acp`, `cargo test -p bingo --test cli
acp::`); this is the twenty minutes that decides whether the protocol notes in
ADR-0035 are still true of the adapters people actually run.

Needs: `node` on PATH, a Claude Code login (`claude login`) or a ChatGPT one
(`codex login`), and a scratch repository to work in. Never CI: it needs a
credential and a network.

## 1. The row

`~/.bingo/settings.json` — one row per adapter, and the adapter's own
permission words on it:

```json
{
  "acp": {
    "adapters": {
      "claude": {
        "command": "npx",
        "args": ["-y", "@agentclientprotocol/claude-agent-acp"],
        "options": { "mode": "dontAsk" }
      },
      "codex-acp": {
        "command": "npx",
        "args": ["-y", "@agentclientprotocol/codex-acp"]
      }
    }
  }
}
```

The npm scopes have moved twice; check the package name against
<https://agentclientprotocol.com> before blaming the plugin. A first run pays
for `npx` fetching the package, so give the first turn a minute.

## 2. The smoke

Tick each line and paste what you saw. Run it once per adapter — the point is
that no code path is adapter-specific, so a difference between the two is a
finding.

- [ ] **A turn.** `bingo --print --provider claude --model agent "what files
      are in this directory?"` answers with the listing. The agent used its
      own tools: the transcript shows what it ran, and bingo executed nothing.
- [ ] **The agent's own calls.** `--output-format json` on the same question:
      each call the agent made is one `reasoning` item whose
      `providerMetadata.acp.external` is `true` and whose payload carries the
      call whole — id, kind, status, title, `rawInput`. No `toolCall` item
      appears; the loop was asked to run nothing.
- [ ] **Two turns, one child.** In the TUI (`bingo`), ask twice. The second
      answer knows the first without either being repeated to it — the agent
      is keeping the conversation, and only the new turn crossed. One child
      process for both (`pgrep -fa claude-agent-acp` shows one).
- [ ] **An interrupt.** Ask for something long, press `esc`. The turn ends
      with the interrupt marker within a second or two, and the *next* turn
      still works on the same agent session. The child is still there.
- [ ] **Restore.** Quit, then `bingo --continue`. Ask something that depends
      on the earlier turns. Either it simply knows (a resume — no notice), or
      an `ACP_RESTORE` notice says what was lost and how it got back in. Say
      which rung the adapter took: `claude-agent-acp` should resume,
      `codex-acp` should not.
- [ ] **Permission.** With no permission words on the row, ask for something
      the agent would normally ask about (`edit README.md`). If the agent asks
      bingo, the call is refused, one `ACP_ASKED` notice names
      `acp.adapters.<name>`, and the turn goes on. Then put the adapter's own
      permission mode on the row and watch the same request succeed without
      bingo being asked at all. That is the whole of ADR-0035 §5. Which door
      that mode goes through is the adapter's: `codex-acp` takes one from
      `env`, and `claude-agent-acp` has neither a flag nor a variable for it —
      its mode is the session config option `options` sets, so that is the
      half of the row to change for it.
- [ ] **Login refused.** Log the adapter out and run a turn. The error is the
      adapter's own words on `session/new`, and `bingo login claude` does not
      offer to fix it — auth is `NotApplicable` and the fix is
      `claude login`.
- [ ] **The tree dies.** Quit bingo. `pgrep -fa acp` finds nothing: the npx
      tree went with the process group.
- [ ] **Usage.** After a turn, the session's token counts are the agent's own
      where it reports them (`claude-agent-acp`), and zero where it does not
      (`codex-acp`). Zero is honest, not free.
- [ ] **The bridge (M39, ADR-0036).** Ask the agent to list the tools it has
      from the MCP server named `bingo`, then to use one. Codex is the harder
      of the two and the one to run this on first — it dials `mcpServers`
      through its own client, not ours.
      1. `bingo --print --provider codex-acp --model agent "list the tools on
         the MCP server called bingo, one per line, and nothing else"`. The
         list holds this house's tools — `SendMessage`, `ListAgents`,
         `TaskCreate` and whatever else the session offers — and holds none of
         `Read`, `Write`, `Edit`, `Bash`, `WebFetch`, `SpawnAgent`,
         `AskUserQuestion`: the agent brought those itself.
      2. `pgrep -fa acp-mcp-proxy` shows one proxy per live ACP session while
         a turn is running, and the socket it dials is under
         `~/.bingo/data/acp/<pid>.sock` with mode `600`.
      3. Ask it to act: with a second session open, `"send a message to the
         session called <name> saying hello"`. The post appears in that
         session; `--output-format json` on this one shows a real `toolCall`
         item under the turn, wearing `external: true` in its meta, and the
         permission gate treated it as it treats any call.
      4. Ask it to call one after its turn is over — it cannot, so instead
         watch the same call inside a turn you interrupt with `esc`: the
         agent is told the call was interrupted, the turn ends, and the next
         turn works on the same child.
      5. If `mcpServers` is configured, the agent was handed those rows too:
         its own `/mcp`-equivalent lists them beside `bingo`, and their tools
         are *not* in the `bingo` server's list. Set `"forwardMcp": false` on
         the adapter row, run 1 again, and they are — under their
         `mcp__<server>__<tool>` names, gated and untrusted.
      6. Read the first prompt the agent was sent (the adapter's own log, or
         `--output-format json` on a fresh session): it names the bridge and
         says a call is served only during the turn.

- [ ] **The thinking level (M40, ADR-0037).** codex-acp is the one to run
      this on: its effort values come from the Codex app-server at runtime, so
      what it offers is the live answer to "what vocabulary is out there".
      1. Say `models."codex-acp/agent".reasoning = true` in settings first.
         A model the embedded snapshot has never heard of fails closed on
         reasoning, and no level reaches any provider without that line —
         `/think` says so itself when the model does not declare it.
      2. In the TUI, `/think max`, then ask something. Codex's own `/status`
         (or its transcript header) says the effort it is running at. It is
         the deepest codex offers, not `max`: an `ACP_LEVEL` notice named the
         clamp in codex's own word.
      3. `/think low`, ask again: `/status` moves. `/think low` a second time
         sends nothing — the level did not move, so no message did.
      4. An agent with neither knob (any adapter that declares no
         `configOptions`) gets one `ACP_KNOB` notice and keeps its own level,
         and every turn still runs.
- [ ] **The model list (M40, ADR-0037 §2).** Before any session, `/models`
      lists `codex-acp` with `agent` alone — an external agent picks its own
      models per session and there is no door to ask before one is open.
      1. Run a turn. Then `/models refresh` and `/models`: the instance now
         serves the agent's own ids beside `agent` — codex's plain slugs
         (`gpt-5.4-codex`, …), claude-agent-acp's `default`, `opus`,
         `sonnet`.
      2. `/model codex-acp/<one of them>`, then ask something. Codex answers
         on that model — its `/status` says which — and the change crossed as
         one `session/set_config_option` before the prompt, never inside it.
      3. `/model codex-acp/agent` sends nothing: `agent` is bingo's word for
         the agent's own, and it never crosses.
      4. An adapter old enough to have no `model` option but the legacy
         `session/set_model` is set through that instead, with the bracketed
         `model[effort]` id it lists. If neither is there, an `ACP_KNOB`
         notice says so once.

## 3. What a failure means

- A turn that hangs with no output: the adapter is writing something that is
  not a message to stdout, or it wants a capability this client declared it
  does not have. Run the adapter by hand
  (`npx -y @agentclientprotocol/claude-agent-acp`) and watch the first lines.
- `could not start the ACP adapter`: `npx` is not on PATH, or the package name
  moved.
- A second `session/new` where a resume was expected: the agent forgot the
  session. That is the ladder working, and the notice is the proof.
- The agent says there is no server called `bingo`: read the `session/new` it
  was sent. If the row is there, the agent's MCP client could not spawn the
  proxy — run `BINGO_ACP_BRIDGE_ADDRESS=… BINGO_ACP_BRIDGE_TOKEN=… bingo
  acp-mcp-proxy` by hand against a live run and see what it says. If the row
  is not there, an `ACP_BRIDGE` notice on the session says why.
- `/think` moves nothing and no `session/set_config_option` crosses: either
  the model does not declare reasoning (say `models."<row>/agent".reasoning =
  true`, and `/think` will have told you so), or the agent declared no
  effort-shaped option — an `ACP_KNOB` notice says which. Read the
  `session/new` answer: `configOptions` is where the knobs are, and an
  adapter that suppresses the block for some clients suppresses it for this
  one too.
- The agent lists the bridge's tools but every call is refused: the calls are
  arriving outside a turn. That is the rule, not a fault — an agent that
  batches its tool calls until after it has answered cannot use this bridge,
  and that is a finding worth writing down.
