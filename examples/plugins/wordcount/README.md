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
  the answer is `{protocol, name, version, tools, commands, contributors,
  compactors, providers, hooks, services}`. A `protocol` the host does not
  speak is refused rather than guessed at; declare only the kinds you have,
  and leave the rest out.
- **Calls** are `tool/call {callId, name, input, cwd, session, turn}` →
  `{output}`, `command/run {name, args, cwd, session}` → `{outcome}`,
  `command/complete {name, partial, cwd}` → `{completions}`,
  `context/contribute {id, query}` → `{pieces}` and
  `compactor/compact {id, context, reason}` → `{compaction}`.
- **Services** are the one method that travels both ways: `service/call
  {key, method, params}` → `{result}`. Declare `services: {"<key>":
  {"methods": {"<name>": <schema>}}}` in the handshake to serve one, and send
  the same request to the host to call anybody's — the host routes it, so two
  plugins pair without knowing of each other.
- **Hooks** are declared as `hooks: [{id, points?, tool?}]` — the points you
  claim, and an optional tool name for the tool points; claim nothing and you
  are asked at every point. The four that decide arrive as `hook/decide
  {id, site, point, payload}` → `{outcome, value?}`, where `outcome` is
  `{"kind": "continue" | "deny" | "ask" | "block" | "redirect"}` — there is no
  allowing outcome, so a hook can tighten what happens and never widen it —
  and `value` is the rewritten input or call at `submit` and `beforeTool`
  only. The four that watch arrive as a `hook/observe {id, site, point,
  payload}` notification, which nothing waits on. A `hook/decide` you do not
  answer in time decides nothing at all.
- **Notifications**: the plugin may send `tool/progress {callId, tail}` while a
  call runs — it becomes that call's live output line — and the host sends
  `tool/cancel {callId}` when the turn is interrupted. The host still waits for
  the answer, so a plugin that ignores a cancel is slow, never broken.

`ToolOutput`, `CommandOutcome`, `View`, `ToolSpec` and `CommandSpec` are the
kernel's own types, so `display` in a tool's output and a `view` outcome are
drawn by every surface — the table this plugin answers with is one `View`.

## What is not here

This plugin ships a tool and a command; the wire also carries context
contributors and compaction strategies (ADR-0030), each declared at the
handshake and asked by id. Hooks, policies, surfaces and stores stay
in-process — the authority plane is not the bridge's to cross.
