# Hooks

A hook is a shell command bingo runs at a fixed point in a session. The
settings block is Claude Code's, so a `.claude/settings.json` `hooks` block can
be pasted in as written:

```jsonc
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [{ "type": "command", "command": "./check.sh", "timeout": 10 }]
      }
    ]
  }
}
```

`type: "command"` is the only kind served; an `http`, `mcp_tool`, `prompt` or
`agent` hook is refused at startup rather than skipped in silence, and so is an
event name this plugin does not serve — a hook nobody will run is a rule its
author believes is enforced. A project's `hooks` add to the person's rather
than replacing them.

## The events, and what each may answer

| event | when | an answer may |
|---|---|---|
| `PreToolUse` | before a call is gated | rewrite the input, deny, ask |
| `PostToolUse` | after a call succeeded | end the turn after this round |
| `PostToolUseFailure` | after a call failed | end the turn after this round |
| `UserPromptSubmit` | a prompt was typed | reject it, add context to it |
| `Stop` | the turn is ending | ask for one more turn, with a reason |
| `PreCompact` | before a compaction | nothing |
| `SessionStart` | a session opened | export variables (see below) |
| `SessionEnd` | a session closed | nothing |
| `Notification` | a notice was said | nothing |
| `PermissionRequest` | a prompt reached a person | nothing |

`permissionDecision: "allow"` does not skip the gate: bingo has one permission
path, the policy, and a hook is not it — `allow` reads as "no objection". A
`PostToolUse` `decision: "block"` ends the turn after the round rather than
undoing the call.

## The matcher

A whole-string-anchored regex over the event's subject: the tool's name for
`PreToolUse`, `PostToolUse`, `PostToolUseFailure` and `PermissionRequest`, the
notice's code for `Notification`, `startup` for `SessionStart`, `auto` for
`PreCompact`, `other` for `SessionEnd`. `UserPromptSubmit` and `Stop` carry no
subject, so only an absent or empty matcher selects them.

`Edit` therefore does not select `EditNotebook`, and `mcp__.*` selects every
MCP tool. An absent or empty matcher selects everything. A pattern that will
not compile is matched as a literal string, with one warning.

## What a hook reads, and what it answers with

One JSON object arrives on stdin. Every event carries `hook_event_name`,
`session_id` and `cwd`, then the event's own fields — `tool_name`,
`tool_input`, `tool_use_id`, `tool_response`, `prompt`, `message`. Two fields
Claude Code sends are absent and are never faked: `transcript_path` (the
journal is not a Claude Code transcript) and `permission_mode` (it lives in the
permissions plugin, and one plugin may not read another's state).

The exit code decides:

- `0` — "here is what I have to say"; stdout is read.
- `2` — blocked, and what the hook wrote on stderr is the reason.
- anything else — a broken hook: it is logged, and it never decides.

Stdout is read only when it opens with `{`; plain text is not context here.
Both decision dialects are accepted — the top-level `decision` / `reason` pair
and `hookSpecificOutput` (`permissionDecision`, `permissionDecisionReason`,
`updatedInput`, `additionalContext`) — and `hookSpecificOutput` wins where they
disagree. `"continue": false` stops bingo whatever the decision said.

A hook gets 60 seconds unless it asks for another `timeout`; a `SessionEnd`
hook gets 1.5 seconds and may ask for at most 60, because teardown is not the
place to wait on somebody's script.

## Exported variables

A `SessionStart` hook is given `BINGO_ENV_FILE`, a path it may append
`KEY=value` or `export KEY=value` lines to. Every later hook in that session
runs with those variables. The file is read as assignments, never sourced as
shell.
