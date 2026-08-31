# wordcount — a bingo plugin in Python

One tool and one command, in Python 3 with nothing but the standard library.
It is the worked example of ADR-0015: a third party ships a bingo-native
`Tool` and `Command` in a language that is not Rust, and both run through the
same permission gate and the same registry as everything else.

## Install it

Copy this directory to either layer; the directory's name is the plugin's name
and must match the manifest's `name`:

- `<config_dir>/plugins/wordcount/` — yours, in every project
  (`~/.bingo/plugins/wordcount/` by default)
- `<project>/.bingo/plugins/wordcount/` — this repository's, and it wins the
  name against yours

Then:

```
$ bingo "how many words are in notes.txt?"
$ bingo "/wordcount notes.txt"
```

The tool reaches the model as `plugin__wordcount__count`. Like every tool that
comes from outside this binary it is **untrusted**: the gate asks about every
call, whatever the plugin says about itself.

## The contract

`schema/plugin.json` at the root of this repository is the whole of it,
generated from the kernel's own types — read that, not this file, when you
write your own. In short:

- **The manifest** is `plugin.json`: `{name, version, entry: {command, args?,
  env?}, config?}`. `${PLUGIN_ROOT}` in `command`, in any argument and in any
  environment value is the directory the manifest was read from.
- **The wire** is JSON-RPC 2.0, one message per line, on the process's stdin
  and stdout. Stdout carries messages and nothing else; stderr goes to
  `<data_dir>/logs/plugin-wordcount.log`.
- **The handshake** is `initialize {protocol, pluginRoot, config, env}`, and
  the answer is `{protocol, name, version, tools, commands}`. A `protocol`
  the host does not speak is refused rather than guessed at.
- **Calls** are `tool/call {callId, name, input, cwd, session, turn}` →
  `{output}`, `command/run {name, args, cwd, session}` → `{outcome}`, and
  `command/complete {name, partial, cwd}` → `{completions}`.
- **Notifications**: the plugin may send `tool/progress {callId, tail}` while a
  call runs — it becomes that call's live output line — and the host sends
  `tool/cancel {callId}` when the turn is interrupted. The host still waits for
  the answer, so a plugin that ignores a cancel is slow, never broken.

`ToolOutput`, `CommandOutcome`, `View`, `ToolSpec` and `CommandSpec` are the
kernel's own types, so `display` in a tool's output and a `view` outcome are
drawn by every surface — the table this plugin answers with is one `View`.

## What is not here

A plugin contributes tools and commands and nothing else in v1. Hooks,
context contributors, providers, surfaces and stores stay in-process; each
later kind needs its own line in ADR-0015.
