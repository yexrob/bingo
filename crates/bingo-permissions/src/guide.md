# Permissions

Every tool call passes one gate. This plugin's policy answers, the kernel
enforces the answer, and what a person decides at a prompt comes back into the
same policy.

## The ladder

One decision; the first step that decides, decides:

1. the tool's own confirmation — a call only a person may take
2. a `deny` rule
3. a write into a sensitive directory (`.git`, `.claude`, `.vscode`, `.idea`)
4. an `ask` rule
5. `bypassPermissions`
6. an `allow` rule
7. what the mode does when no rule decided

Steps 1 to 4 stand above every mode: no allow rule and no mode silences a
tool's confirmation, a deny rule, a write into a sensitive directory or an ask
rule. Two modes then bound whatever the ladder said — `plan` may not act and
`dontAsk` has nobody to ask — so each turns what it cannot honour into a
denial. A mode that cannot answer never answers yes.

## Modes

A mode says what happens when no rule decided. Set it with
`permissions.defaultMode` in the settings, `--permission-mode <mode>` on the
command line, or `/permission <mode>` for this session alone:

- `default` — trusted read-only tools run; everything else asks.
- `acceptEdits` — edits inside the working directories run without a prompt.
- `plan` — nothing that is not read-only runs at all.
- `bypassPermissions` — everything runs except what only a person may decide.
- `dontAsk` — nobody is there to answer, so what would have asked is denied.

`--dangerously-skip-permissions` is `--permission-mode bypassPermissions`. The
working directories `acceptEdits` trusts are the session's own and whatever
`permissions.additionalDirectories` lists; a tool that names no path names
nowhere, and nowhere is inside nothing.

## Rules

`permissions.allow`, `permissions.deny` and `permissions.ask` are lists of rule
lines. `--allowed-tools <rule,rule>` adds to the allow list for one run. One
form per line:

- `Bash` or `Bash(*)` — every call of that tool; a rule that names nothing
  narrows nothing.
- `Bash(git status)` — a prefix of the command, read both as text and as the
  words a shell would see. `Bash(git status:*)` and `Bash(prefix:git status)`
  say the same thing.
- `Edit(/src/**)` — a path glob, where `*` stops at a separator. Written
  without a glob character, `Edit(/src/)` covers everything under it. `~` and a
  relative path resolve against the home and the session's directory, as text:
  a rule holds for a file that does not exist yet.
- `WebFetch(domain:example.com)` — the URL host, exactly.
- `Skill(deploy)` — the exact name, for a call whose subject is a name.
- `mcp__server` or `mcp__server__tool` — a whole MCP server, or one tool of it.

Deny and ask read a rule the broad way: one subject, or one command inside a
compound line, is enough. Allow reads it the narrow way: every subject and
every command must be covered, and a line whose text could not be parsed is
covered by nothing. Each takes the reading that fails closed.

A rule this grammar cannot read stops the process at startup, because a deny
rule dropped in silence is worse than a boot that says why.

## What a prompt may offer

A prompt may offer the narrowest rule that would really silence it: the ladder
is run again with that rule in the allow table, and the offer is made only if
the answer becomes an allow — so a confirmation, a sensitive path and a deny
rule offer nothing. Accepting it puts the rule in this session's memory. It
reaches no file, and no other session and no sub-agent hears it; a mode chosen
with `/permission` is the same kind of answer.

## Traits fail closed

A tool nobody has described is not read-only, not trusted and not
concurrency-safe. Only a trusted read-only tool runs unasked in `default`, and
an MCP tool's `readOnlyHint` is a claim by the thing being gated, so the gate
asks about it anyway.

## Commands

- `/permission`, `/permissions` — the mode this session runs in, and the five
  it could run in.
- `/permission <mode>` — run in that mode from now on, this session only. It is
  instant: it answers while a turn is busy, and the next call is decided
  against what it set.
