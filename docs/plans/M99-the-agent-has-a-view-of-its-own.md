# M99 — The agent has a view of its own

## Goal

User, 2026-09-15: bingo should not be an agent that only follows; it has
its own view, and when it thinks the person's path is wrong, or finds a
better one while investigating, it raises that first and says why, rather
than going on down the person's line — the person may not have seen what
the model has. ADR-0059 decides: one plugin, `bingo-persona`, one cacheable
system block after the kernel's identity and before the project's
instructions, one settings key `persona.text` that replaces the block with
the person's own words (amended 2026-09-15: "人格插件可以配置覆盖人格"),
`enabledPlugins` as the off switch (ADR-0057), and the kernel's identity
untouched.

## Bricks, in build order

1. `crates/bingo-persona` — a plugin-tier crate: `Cargo.toml` (sdk,
   async-trait; `[lints] workspace = true`), registered in the workspace and
   the binary's dependency list; `scripts/budget.toml` +1 member with the
   line; `ARCHITECTURE.md`'s features row names it.
2. `src/judgement.rs` — `pub const TEXT: &str`, the block. Substance per
   ADR-0059 §3; a first draft the worker refines for voice, keeping every
   point:

   > # Judgement
   > You are a colleague, not an order-taker: you have a view of your own,
   > and the person has asked to hear it.
   > - When you think the approach you were given is wrong, or while working
   >   you find a better one, say so before you go on: what you would do
   >   instead, why it is better, and what it costs. Make the case in a few
   >   sentences; do not lecture.
   > - Then let the person decide. If they hold to their path after hearing
   >   you, take it, and take it well. If the work can wait for their answer,
   >   wait. If it cannot, take the path you believe in, say that you did,
   >   and say why.
   > - Disagree with a claim, never with a person. Be as direct about the
   >   case against your own view as for it, and say when you are guessing.
   > - Never deviate silently. A better path taken without a word is worse
   >   than the wrong path taken together: the person cannot see what you saw.
   > - Small choices — a name, an order of steps, a tool — are yours to make.
   >   Raise the ones that change the result, the cost, or what is left
   >   afterwards.

   `JudgementContributor { text: String }`: id `persona:judgement`,
   `Placement::System { order: ORDER }` with `ORDER = -20`, contributes one
   `ContextPiece::System(SystemBlock { text, cache: true })`, or nothing
   when the text is empty.
   Tests: the id and the placement; the block is cacheable; the text names
   the four moves (before, decide, never silently, small choices) by a
   phrase each.
3. `src/lib.rs` — `MANIFEST { id: "bingo.persona", provides: ["context:persona:judgement"], requires: [], config: Some(ConfigClaim { keys: [("persona", Merge::Replace)], schema }) }`;
   `Settings { persona: Persona { text: Option<String> } }` with
   `deny_unknown_fields` on `Persona` (the `experience` pattern);
   `PersonaPlugin` reads the slice and builds `JudgementContributor::new(text)`:
   `None` → the crate's `TEXT`, `Some(s)` → `s`, `Some("")` → the contributor
   answers no piece. Tests: the manifest; each of the three readings; a
   typo under `persona` fails `register` with a message naming the field.
4. bin `main.rs` — `PersonaPlugin` pushed right after `ContextPlugin`.
5. Black-box `tests/cli/persona.rs` — a fake-provider script whose response
   `when` matches a phrase of the block ("Never deviate silently"), so the
   run proves the block reached the model's request; and, if M97 has
   landed on `dev` by then, a second run with `enabledPlugins["bingo.persona"] = false`
   in `.bingo/settings.json` whose script matches only when the phrase is
   absent — else leave that test for the merge. A third run seeds
   `persona.text = "You are Bingo the pirate."` and matches that phrase,
   and its script refuses the request if "Never deviate silently" is there.
6. `docs/adr/0006-context-budget.md` unchanged: the block is a plugin's, as
   the ADR says plugins' blocks are.

## Files

- `Cargo.toml`, `crates/bingo/Cargo.toml`, `scripts/budget.toml`, `ARCHITECTURE.md`
- `crates/bingo-persona/{Cargo.toml,src/lib.rs,src/judgement.rs}`
- `crates/bingo/src/main.rs`, `crates/bingo/tests/cli/{main.rs,persona.rs}`

## Exit criteria

- [ ] the block is in the system prompt after the kernel's blocks and before
      `context:instructions` (a core `turn/contributors` ordering test or the
      fake provider's request record).
- [ ] the fake provider matches the phrase in a black-box run.
- [ ] every gate green; `budget.sh` +1 member and nothing else;
      `check_discipline.sh` sees the crate as a plugin (sdk only).

## Non-goals

- A `/persona` command, a file-path knob, a tone knob; a per-project text is the project layer's `persona.text`.
- Rewording the kernel's identity block.

## Risks

- R-voice: the text is model-facing prose; keep it under ~1,000 characters
  so the cached cost stays small.
- R-order: `-20` sorts before every existing contributor; the test pins it.
- R-parallel: M97 (`enabledPlugins`) and M98 (`settings.toml`) land beside
  this; the only shared files are `main.rs` (one `push`) and `budget.toml`.
