# ACP live smoke (M38)

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
        "args": ["-y", "@agentclientprotocol/claude-agent-acp",
                 "--permission-mode", "acceptEdits"]
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
      bingo being asked at all. That is the whole of ADR-0035 §5.
- [ ] **Login refused.** Log the adapter out and run a turn. The error is the
      adapter's own words on `session/new`, and `bingo login claude` does not
      offer to fix it — auth is `NotApplicable` and the fix is
      `claude login`.
- [ ] **The tree dies.** Quit bingo. `pgrep -fa acp` finds nothing: the npx
      tree went with the process group.
- [ ] **Usage.** After a turn, the session's token counts are the agent's own
      where it reports them (`claude-agent-acp`), and zero where it does not
      (`codex-acp`). Zero is honest, not free.

## 3. What a failure means

- A turn that hangs with no output: the adapter is writing something that is
  not a message to stdout, or it wants a capability this client declared it
  does not have. Run the adapter by hand
  (`npx -y @agentclientprotocol/claude-agent-acp`) and watch the first lines.
- `could not start the ACP adapter`: `npx` is not on PATH, or the package name
  moved.
- A second `session/new` where a resume was expected: the agent forgot the
  session. That is the ladder working, and the notice is the proof.
