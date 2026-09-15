# M101 — The pages catch up

## Goal

User, 2026-09-15: "新增的 persona 插件貌似没有 guide-persona skill". ADR-0054
says a plugin with something to say about itself registers a page, read
by the model as `guide-<name>`; `bingo-persona` (M99) registered none.
The same session left two more gaps: the map (`bingo-skills`'s bundled
`guide`) still names `settings.json` and knows nothing of `/plugins` or
`enabledPlugins` (M97), and six plugin pages show their settings as JSONC
while the layers are TOML now (M98). After this milestone every page says
what its plugin is today, in the format a person now writes.

## Bricks, in build order

1. `bingo-persona/src/guide.rs` + `guide.md` — the `checkpoints` pattern:
   `PAGES` with one `Page { name: "persona", description, body }`,
   `contribution(registrar)`, `service:bingo.persona.pages` added to
   `provides`, registered in `register` beside the contributor. The page
   says, briefly: what the stance is and why it exists (a colleague raises
   a better path before taking it; the person decides); that it is one
   cacheable system block after the kernel's identity and before the
   project's instructions, so a project file can narrow it; `persona.text`
   replaces the whole block, `""` silences it, and the off switch is
   `enabledPlugins["bingo.persona"] = false`; a TOML example of each.
   Tests beside it as the other guides have: the page names the settings
   key and every field of `Persona`, and the phrase the black-box test
   matches on ("Never deviate silently") so the two cannot drift.
2. `bingo-skills/src/bundled/guide.md` (the map, ≤200 lines, currently 88):
   `## Settings` says the layers are `settings.toml` (user, project,
   local), that a `settings.json` beside no `.toml` is still read and is
   migrated on the next start to `.toml` + `.json.bak`, and that TOML has
   no `null`; `## Where things live` names `settings.toml`; `## Commands`
   gains `/plugins [enable|disable <name>]` (alias `/modules`) and one
   sentence that `enabledPlugins` is the key and a switch takes effect at
   the next start, with `bingo plugins list|enable|disable` as the
   terminal twin. Its existing tests keep passing; add a line to the test
   that pins the kernel commands, if one lists them.
3. The six plugin pages — `channels`, `mcp`, `experience`, `hooks-shell`,
   `schedule`, `provider-acp` — rewrite each `jsonc`/`json` settings
   example as TOML (`[mcpServers.files]`, `[[hooks.PreToolUse]]`, …),
   keeping every key and comment; where a page says "in `settings.json`"
   it says `settings.toml`. Each page's existing tests (the mcp page
   names every verb, etc.) keep passing; add to each guide's test that the
   body contains no ```` ```json ```` fence, so the format cannot drift back.
   The hooks page may keep one sentence that Claude Code's `hooks` block
   drops in unchanged, because it does: the reader reads JSON too.
4. `docs/adr/0054` gains no amendment; `docs/adr/0059` Consequences gains
   one line naming the page.

## Files

- `crates/bingo-persona/src/{lib.rs,guide.rs,guide.md}`
- `crates/bingo-skills/src/bundled/guide.md`, `crates/bingo-skills/src/guide.rs` (tests)
- `crates/bingo-{channels,mcp,experience,hooks-shell,schedule,provider-acp}/src/guide.{md,rs}`
- `docs/adr/0059-the-agent-has-a-view-of-its-own.md`

## Exit criteria

- [ ] `guide-persona` is listed under `# Skills` in a run's system prompt
      (a black-box `--print` run over the fake provider whose `when`
      matches `guide-persona`, beside `tests/cli/persona.rs`).
- [ ] the map names `settings.toml`, the migration, `/plugins` and
      `enabledPlugins`.
- [ ] no plugin page carries a JSON settings example; every page's own
      tests pass.
- [ ] every gate green (fmt, check, clippy, test, discipline, budget: no
      new dependency, count 342).

## Non-goals

- A page for the kernel's own commands beyond the map's line: the map is
  the kernel's page (ADR-0054 §3).
- The site (`../bingo-site`): separate.
- Changing what any plugin does.

## Risks

- R-tests: several guide tests assert exact substrings of their page;
  rewriting an example in TOML may move a key the test greps for. Keep
  every key name spelled the same.
- R-parallel: M100 is being built beside this and touches
  `bingo-mcp/src/command.rs`, not its `guide.md`.
