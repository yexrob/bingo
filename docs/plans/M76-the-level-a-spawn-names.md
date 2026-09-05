# M76 — The level a spawn names

## Goal

User, 2026-09-05: "bingo 派生子agent的时候可以指定模型和 effort吗? 如果不
可以的话 让agent自己可以动态调节自己或者子agent的effort吧。以及agent可以在
派生子agent的时候动态指定模型/provider." Model and provider: already
`SpawnAgent` fields, picked with `ListModels`. Effort: not sayable
anywhere but `/think`, and only by a person, on the session in front of
them. After this milestone a spawn names a level (call, definition,
else the parent's), and a session moves its own level or a child's with
one tool, landing on the next turn (ADR-0047).

## Bricks

1. **`bingo-sdk`** — pure first.
   - `Effort` gains the spoken words: `OFF` (`"off"`), `words()` (the
     six levels lowest first, then `off` — the one list), `spoken(word)
     -> Option<Option<Effort>>` (`Some(None)` off, `None` not a word,
     any case) and `word(Option<Effort>) -> &str`. `/think`'s private
     `parse`/`name`/`levels` are deleted for these.
   - `SessionSpec.thinking: Option<Option<Effort>>`, `default`,
     `skip_serializing_if = Option::is_none`, `deserialize_with` a
     helper that makes `null` `Some(None)`. Doc: absent inherits, `null`
     is off. Contract test: absent / `null` / `"low"` round-trip and a
     spec JSON from before the field reads as absent.
   - `SessionChange` — the kernel's `Change`, moved, same three arms —
     and `HostApi::reconfigure(&self, session, SessionChange) ->
     Result<(), KernelError>` with a refusing default in the style of
     `rewind` ("this host runs no session"). Not on the JSON-RPC wire,
     not across the plugin bridge; the doc says both.
2. **`bingo-core`** — the spec is the one holder.
   - `Live.thinking` deleted; `inherited` fills `spec.thinking` from the
     live parent, else the settings; `choose_model(&spec)`,
     `model_for(&spec)`, `session_thinking` read `spec.thinking
     .flatten()`; `resume` sets `spec.thinking = Some(thinking_of(state)
     .unwrap_or(settings))`.
   - `Host::reconfigure` is the trait impl, answers `()`; `/think`,
     `/model`, `/rename` call it and read `session_model` back for their
     words. `Change` is gone; `commands/think.rs` uses the sdk words.
   - Test (`host/tests/tree.rs`, beside `a_child_inherits_the_model_
     and_effort_its_parent_stands_on`): a parent at `high`; a child opened
     with `thinking: Some(None)` sends no `reasoning`, one with
     `Some(Some(Low))` sends `low`, one with `None` sends `high` — read
     from the fake provider's recorded requests, not from a summary.
3. **`bingo-agents`** — the words at the door.
   - `Definition.thinking: Option<String>` from frontmatter `thinking:`;
     test that YAML `off` arrives as the word, not a boolean.
   - `SpawnArgs.thinking: Option<String>` with a `schemars(schema_with)`
     enum built from `Effort::words()`; `Plan.thinking:
     Option<Option<Effort>>` — call over definition; a word that is not
     one is `ToolOutput::error` naming the definition or the call and
     listing the words; the spec carries it. The `agent` field's doc
     gains the word `thinking`.
   - `thinking.rs`, new: `SetThinking{level, agent?}`. `level` is the
     same enum schema; `agent` resolves among `names::children` by
     title (absent: `cx.session`); unknown name lists the children,
     unknown word lists the words; calls `cx.host.reconfigure`. Result:
     `thinking: high for reviewer, from its next turn` / `…for this
     session, from your next turn`. Traits `crate::traits()`, subject
     `Name` when `agent` is given. Manifest, `lib.rs` doc ("six tools")
     and the manifest tests follow; `Fleet` in `tests.rs` records
     reconfigures so the tool is testable without a kernel.
   - Test on the kernel (`crates/bingo/tests` or agents' kernel-backed
     tests, wherever `SpawnAgent` is already driven end to end): a
     two-round fake script in which round one calls `SetThinking
     {level: "low"}` on itself — round two's request still carries the
     old level, the next turn's carries `low` (the boundary is the
     claim; the test pins it).
4. **`bingo` black-box** (`crates/bingo/tests/rpc.rs`): `session/open`
   with `"thinking": "low"` → the first `ConfigChanged` says
   `kernel.thinking == "low"`; with `null` → `null`.
5. **Docs**: ADR-0047, the ADR-0037 §3 note and the README line are
   written with this plan; the parent appends Verified after the merge.

## Files

- `crates/bingo-sdk/src/{model,host}.rs`, `lib.rs` re-export.
- `crates/bingo-core/src/{host.rs,host/choose.rs,host/resume.rs,
  commands/think.rs,commands/model.rs,commands/rename.rs,
  host/tests/tree.rs}`; any `impl HostApi` that must record.
- `crates/bingo-agents/src/{definition,spawn,lib,tests}.rs`, new
  `thinking.rs`.
- `crates/bingo/tests/rpc.rs`.
- `docs/adr/{0037-the-knobs-cross,README}.md`, this file.

## Exit criteria

- [ ] `SessionSpec` contract test: absent / `null` / `"low"` round-trip;
      pre-field JSON reads as absent.
- [ ] A child opened under a parent at `high`: `null` → no `reasoning`;
      `"low"` → `low`; absent → `high` (fake provider requests).
- [ ] `/think`, `/model`, `/rename`: existing tests green; `/think`
      still writes the user layer; `/think` usage lists the seven words
      from `Effort::words()`.
- [ ] `SpawnAgent{thinking: "low"}` opens the child at low; `"loud"` is
      refused with the words; a definition `thinking: off` opens off.
- [ ] `SetThinking` on itself lands on the next turn and not on the
      running one (two-round script); on a child by name; unknown name
      and unknown word each refused in words.
- [ ] `session/open` with `thinking` echoes it in `ConfigChanged`.
- [ ] Gates: `cargo fmt --all -- --check`, `cargo check --workspace
      --all-targets --locked`, `cargo clippy --workspace --all-targets
      --locked -- -D warnings`, `cargo test --workspace --locked`,
      `scripts/check_discipline.sh`, `scripts/budget.sh`, output pasted
      below.

## Non-goals

- A level that lands inside the running turn (ADR-0047 §5).
- A tool that moves a live session's model (`SetModel`); a `--think`
  CLI flag; a per-turn level on `Input`.
- The website's agents page (another repo; the parent updates it).
- Any change to how the providers encode `reasoning`.

## Risks

- `Option<Option<Effort>>` and serde: `null` folds into absent unless
  `deserialize_with` says otherwise — the contract test is the guard.
- YAML: `off` is a boolean in 1.1; serde-saphyr is 1.2 — the definition
  test pins the word.
- Fakes: `HostApi` has ten implementors; the refusing default keeps
  them compiling, and only `Fleet` (agents) needs to record.
- Model-facing text: `SpawnAgent`'s description is already long; the
  new field speaks through its own doc line, not the description.
- `SetThinking` crosses to ACP agents by the deny list's shape; that is
  intended (ADR-0047 §4), and no test drives a real agent.
