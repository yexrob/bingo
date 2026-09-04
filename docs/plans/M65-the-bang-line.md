# M65 — The bang line

## Goal

User, 2026-09-05: Claude Code's `! <command>` — a line that starts
with `!` runs in the shell right there, and its output lands in the
conversation so the model sees it next turn, without a model turn
being spent on it. bingo has no such line: `!ls` today is a message
to the model.

## Shape

`Input::text` whose line starts with `!` is a **shell line**, routed
in the kernel beside `/` commands (ADR-0008): no turn starts, no
model is called. The kernel runs the command through the registered
`Bash` tool as the person — the person typed it, so no permission
question is asked and no hook's `BeforeTool` fires (hooks gate the
model's calls; this is not one) — with the tool's own timeout, its
cwd the session's cwd. What comes back is one journal item the
person made:

```
ItemBody::Shell { command, output, exit: Option<i32>, cwd }
```

Contract first: the sdk variant, both schemas regenerated, the
frames fixture gains one, and every surface renders it before the
kernel produces one. To the model it is a user-role message in the
next request: `$ <command>` and the output in a fenced block, the
exit code when it is not zero — `context/transcript.rs` is where
items become messages, and this is one more arm there.

## Bricks

1. **The item.** `ItemBody::Shell` in `bingo-sdk`, serde-tested both
   ways; `schema/{rpc,plugin}.json` regenerated in the same commit;
   `SessionState::apply` needs nothing new (it is an item). The
   `--print` text renderer prints `$ cmd` + output; stream-json
   passes the item through as it does every item.
2. **The route.** In the kernel's submit path where `/` is matched:
   `!` → `shell::run(session, line)`. Empty after the bang is a
   refusal (`InvalidInput`: "nothing to run"). The tool is found by
   name (`Bash`) among the registered tools; a host with no Bash tool
   refuses with a message that says so. Output is capped as the tool
   caps it; nothing new is invented.
3. **The row.** The TUI draws the item as a person's block whose
   first row is `$ <command>` in the composer's own style, the output
   under it as a result block (the `⎿` fold, five rows peeked, the
   pager for the whole), a non-zero exit in `bad`. Snapshots at both
   sizes; the composer's `!` gets no dropdown and no completion.
   Channels render the same item as they render a tool result.
4. **Black-box.** `--print` with `--input-format stream-json` feeding
   `!echo hi` yields the item and no model request (the fake provider
   counts zero); a pty scene types `!echo hi ⏎` and reads `hi` in the
   result row.

## Files

`bingo-sdk/src/event.rs`, `schema/*.json`, `bingo-core/src/{submit
or wherever `/` is matched, shell.rs (new)}`, `bingo-context/src/
transcript.rs`, `bingo-surface-print/src/render.rs`, `bingo-surface-
tui/src/transcript/*.rs` + snapshots, `bingo-channels` rendering,
ADR-0008 dated amendment (a third kind of line), `docs/design/tui.md`
§5 (a content kind) + dated line.

## Exit criteria

- [ ] The item round-trips the wire; schemas regenerated; fixture
      frame added.
- [ ] `!cmd` runs with no model request and journals one item.
- [ ] TUI, `--print` text and stream-json, and a channel each render
      it (snapshot / golden).
- [ ] The model's next request carries `$ cmd` and the output as a
      user message.
- [ ] All gates; Windows cross-check for `bingo-core` and `bingo-sdk`
      (the shell is the Bash tool's — its Windows arm is its own).
- [ ] Hands-on: appended by the parent.

## Non-goals

Background `!` (a line runs to the tool's timeout; `&` is the
shell's business); a `!` in a room post from another agent (a person
is the only principal that may write one — refuse otherwise);
interactive commands.

## Risks

A `!` line that hangs holds the composer? No: the kernel runs it
off the loop as it runs a tool, and the TUI shows the running row
with `esc` dropping it as it drops any call. The item is a person's,
so rewind treats it as a person's turn — check `rewind::asked`
derives turns from person items and does not count this one as a
prompt the model answered.
