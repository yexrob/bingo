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

- [x] The item round-trips the wire; schemas regenerated; fixture
      frame added.
- [x] `!cmd` runs with no model request and journals one item.
- [x] TUI, `--print` text and stream-json, and a channel each render
      it (snapshot / golden).
- [x] The model's next request carries `$ cmd` and the output as a
      user message.
- [x] All gates; Windows cross-check for `bingo-core` and `bingo-sdk`
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

## Verified

2026-09-04, branch `m65-bang`.

### What the plan said and what landed

**The premise was stale.** `!<command>` already ran the shell: the bash
plugin has registered a `!` command since M16 (`4e199df3 feat(tool-bash):
run the ! shell line a person typed`), the actor has parsed `!` as a
command name since M0 (ADR-0008 §1), and ADR-0008 §5 assigns the line to
that plugin. What was missing is everything the plan asks for *around*
the line: the item was an untyped `Action{name: "!", …}` whose exit code
was a footer inside its own output, no surface drew it as a shell line,
and the model read it as `[!] cmd\noutput`. M65 lands as the upgrade of
an existing line rather than a new one.

**The shell run stays the bash plugin's; the routing decisions moved to
the kernel.** The plan asks the kernel to "run the registered `Bash`
tool as the person". Three things stand against that, and the third is
the repo's own gate:

1. `scripts/check_discipline.sh` §4c — *the kernel knows no tool by
   name*. `Bash` in `bingo-core` fails the run; it failed mine while I
   was writing the refusal message, which is how I found it.
2. `ToolContext` requires a `TurnId`, and a shell line spends no turn.
   The kernel would have to mint one that does not exist, or
   `ToolContext.turn` becomes `Option`, which is a wire change to
   `ToolCallParams` in `schema/plugin.json` for every plugin-rpc tool.
3. `Tool::call` applies the interactive-reject table (`less`, `vim`,
   `sudo`), which exists to keep a *turn* from being spent on a program
   waiting for keys. A person's line spends none, and
   `shell::a_line_the_tool_would_refuse_runs_for_the_person_who_typed_it`
   fixes that deliberately. Routing through the tool deletes it.

So the kernel owns the route's two decisions — **who may write one** and
**what a host with no shell is told** — and dispatches through the one
command table as ADR-0008 §1 already had it. The refusal names no tool.

**Not delivered.** The Risks section's running row: a shell line is
still journaled whole when it finishes (`CommandOutcome::Record`), so a
long one draws nothing until it ends and `esc` says "no turn is
running". Nothing regressed — that is how the line has always behaved —
but the row and the interrupt are not in this change.

**`--print` text writes to stderr**, where a tool call's report goes:
stdout is the model's answer and nobody asked the model for this.

### Gates

```
$ cargo fmt --all -- --check
clean

$ cargo check --workspace --all-targets --locked -j 2
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.84s

$ cargo clippy --workspace --all-targets --locked -j 2 -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.73s

$ cargo test --workspace --locked -j 2 --no-fail-fast
passed: 3930 failed: 0        (0 lines matching FAILED or ^error)

$ cargo test -p bingo --test pty -j 2
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 7.98s

$ scripts/check_discipline.sh
dependency direction ok / kernel names no tool / cohesion ok / discipline ok

$ scripts/budget.sh
dependencies (unique, normal): 333 (max  333)
budget ok

$ cargo deny check
advisories ok, bans ok, licenses ok, sources ok

$ cargo check -p bingo-sdk -p bingo-core --all-targets --locked -j 2 \
    --target x86_64-pc-windows-msvc
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 16.98s
```

### The tests that hold each criterion

| criterion | test |
|---|---|
| the wire | `bingo_sdk::event::tests::every_event_variant_has_a_pinned_wire_form` (frame 29, round-tripped); `schema/{rpc,plugin}.json` regenerated |
| no model request | `cli::shell::a_bang_line_journals_one_shell_item_and_spends_no_turn` — the fake provider is scripted with **no** responses, so a request would fail the run |
| the person only | `session::commands::only_the_persons_own_line_may_run_a_shell_command`, `session::tests::commands::a_shell_line_from_anyone_but_the_person_is_refused` |
| no shell here | `session::tests::commands::a_shell_line_with_no_shell_registered_says_so` |
| nothing to run | `cli::shell::a_bang_with_nothing_after_it_is_refused` |
| the model's view | `context::tests::a_shell_line_reaches_the_model_as_the_line_and_its_output` |
| TUI | `screens::ran::shell_lines` (80×24, 120×40), `shell_lines_in_ascii`, `screens::colours::{the_line_is_the_persons_own_and_only_a_bad_exit_is_coloured, a_shell_line_sits_on_the_bar_your_own_words_sit_on}`, `transcript::ran::tests::*` |
| rewind | `rewind::tests::{a_shell_line_is_not_the_prompt_a_turn_answered, a_shell_line_between_turns_opens_no_row_of_its_own}` |
| no dropdown on `!` | `commands::tests` (`suggestions("!ls", …)` empty, `local("!ls") == None`), already held |
| `--print` | `render::tests::a_shell_line_reports_beside_the_answer_and_never_in_it`, `stream_json::tests::a_shell_line_is_a_user_message_of_the_line_and_its_output`, `cli::shell::a_bang_line_reaches_a_host_as_the_user_message_it_becomes` |
| a channel | `deliver::tests::a_shell_line_the_person_ran_lands_beside_the_answer` |
| the pty | `a_bang_line_runs_the_shell_and_leaves_its_output_in_the_transcript` |
| over the wire | `rpc::a_shell_line_and_a_permission_mode_dispatch_as_commands` |
