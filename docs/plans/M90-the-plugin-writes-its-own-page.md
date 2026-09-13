# M90 — The plugin writes its own page

## Goal

User, 2026-09-10: "guide skill 里面是不是应该添加更多内建的 reference 然后精简
guide skill，让 AI 需要查什么的时候去子 reference 里面去看，类似你的 memory 系统那
样". Chosen shape: each reference is a bundled skill of its own, listed in
the prompt by its one-line description; `guide` becomes the map. And the
fix that outlasts this milestone: **a page belongs to the plugin whose noun
it describes** (ADR-0054), so a plugin that grows a verb grows its sentence
in the same crate, under a test that names the verb.

## Bricks, in build order

Slice A — the mechanism, and the pages the old guide already had:

1. `bingo-sdk`: `Page { name, description, body }`, `Pages(&'static
   [Page])`, `Pages::key(plugin_id) -> String` = `"<id>.pages"`. Fixture
   test: the key's spelling. No trait, no kernel change.
2. `bingo-skills/src/pages.rs`: `gather(host) -> Vec<Skill>` — plugin ids
   from `host.catalog(CatalogKind::Plugins)`, each id's
   `host.service::<Pages>(&Pages::key(id))`, each page a `Skill` named
   `guide-<name>` with the page's description and body, no directory.
   Gathered once per process (`OnceLock` in `Library`), appended after the
   skills plugin's own bundled skills so a disk skill of the same name still
   wins (`scan::append_bundled`). `Library::skills` learns to take the host
   it needs; the contributor, the tool and the command already hold one.
3. `bundled/guide.md` cut to the map (ADR-0054 §3, under 80 lines):
   what bingo is, running it, sessions, commands that are the kernel's or
   the surface's, settings layers and kernel keys, where things live —
   ending in a generated `## Pages` list, one line per page found:
   `- guide-rooms — <description>`. `bundled/skills.md` is the skills
   section as a page of the skills plugin's own (`guide-skills`).
4. The sections that move: `bingo-permissions` registers `guide-permissions`
   (modes, rules, fail-closed traits), `bingo-hooks-shell` registers
   `guide-hooks`, `bingo-mcp` registers `guide-mcp` — each the old text,
   read against the crate beside it and corrected where it lags, each with
   the owner's test naming its commands, tools and keys (ADR-0054 §4).
5. Tests: the listing snapshot gains the pages; `an_empty_machine_still_has
   _the_bundled_guide` and its neighbours learn the new count; a black-box
   run shows `# Skills` listing `guide-permissions` and `Skill guide-rooms`
   returning a body (only once slice B lands; until then `guide-mcp`).

Slice B — the pages the guide never had, one per plugin (after A merges):

6. `bingo-agents` (`guide-agents`: SpawnAgent, SendMessage incl. `again`,
   ListAgents, SetThinking, `/agents`, `/team`, `.bingo/team.json`, sub-agent
   rules), `bingo-rooms` (`guide-rooms`: OpenRoom/Seat/Unseat/CloseRoom,
   `/room`, mentions, ears, the serial rule, the one-purpose rule),
   `bingo-tasks` (`guide-tasks`), `bingo-schedule` (`guide-schedule`: Wake,
   schedules), `bingo-context` (`guide-memory`: memory files, `/memory`,
   AGENTS.md, `/compact`, the context budget), `bingo-checkpoints`
   (`guide-rewind`), `bingo-channels` (`guide-channels`: Feishu, gateway,
   access, files), `bingo-experience` (`guide-experience`),
   `bingo-surface-tui` (`guide-tui`: keys, pictures and paste, `tui.measure`,
   the update check), `bingo-provider-acp` (`guide-acp`), the OAuth login
   (`guide-login`, owned by whichever plugin registers `/login`).
7. Each page: under 120 lines, the crate's ADRs and plans as its sources,
   the owner's test naming every command, tool and settings key it
   registers.

## Files

- `bingo-sdk/src/plugin.rs` (or a `page.rs` beside it), `bingo-sdk/src/lib.rs`.
- `bingo-skills/src/{pages,library,scan,bundled,contributor,tool,command,lib}.rs`,
  `bingo-skills/src/bundled/{guide,skills}.md`, the listing snapshot.
- `bingo-permissions/src/{lib,guide}.rs` + `guide.md`; the same shape in
  `bingo-hooks-shell`, `bingo-mcp`, and every slice-B crate.
- `crates/bingo/tests/cli/skills.rs` (or wherever the skills black-box lives).
- ADR-0054, ADR README, `docs/design/tui.md` if the TUI page changes a rule.

## Exit criteria

- [x] `Pages::key("bingo.rooms") == "bingo.rooms.pages"`; a plugin's page
      reaches the skills plugin through the catalog and the typed service
      on the fake host, and a disk skill named `guide-<name>` overrides it
- [x] `guide` is under 80 lines and ends with the generated page list; the
      `# Skills` listing shows one line per page; `Skill guide-permissions`
      returns the permissions page
- [x] every page has an owner test naming its commands, tools and keys; the
      skills plugin's shape test holds for every page found
- [x] the old guide's permissions, hooks, MCP and skills text is on its
      owner's page, corrected against the crate, and gone from `guide`
- [x] slice B: every plugin in brick 6 has a page; a `bingo --print` run's
      system prompt lists them (black-box)
- [x] every gate green; Windows check for `bingo-sdk`, `bingo-skills`

## Non-goals

- No reference files on disk for bundled pages; no `${BINGO_SKILL_DIR}` for
  them. A page is one skill body.
- No per-page permission, model or argument fields; a page is read, not run.
- Site docs (`../bingo-site`) are not generated from pages in this
  milestone; that is the next step once pages exist.

## Risks

- A dozen prompt lines: each description must be one sentence under 250
  characters (`listing::one_line` truncates past it).
- A page written from the crate's ADRs can still be wrong about behaviour
  the ADR describes and the code does not do; the owner's test names words,
  not facts. Reading the code beside the ADR is the worker's rule.
- `Library::skills` gaining a host parameter touches three callers and the
  test fixtures; a `OnceLock` gathers once, so a plugin enabled later is
  not seen until restart — acceptable, plugins do not change at run time.

## Verified (2026-09-10, dev, `0a3f5f1b`…`86dc1875`)

Three `opus-xhigh` worktrees — slice A (sdk type, gathering, the map, the
permissions/hooks/mcp/skills pages), B1 (agents, rooms, tasks, schedule,
experience, rewind) and B2 (memory, channels, tui, acp) — each gated before
its ff merge; B2 re-gated on dev after the rebase, with the pages black-box
un-ignored and listing all fourteen pages:

```
cargo fmt --all -- --check                                      ok
cargo check --workspace --all-targets --locked                  ok
cargo clippy --workspace --all-targets --locked -- -D warnings  ok
cargo test --workspace --locked --no-fail-fast                  ok (89 targets)
scripts/check_discipline.sh                                     discipline ok
scripts/budget.sh                                               budget ok (335)
scripts/tui-smoke.sh                                            tui-smoke ok
```

Windows check ran for `bingo-sdk` and `bingo-skills` in slice A; B1 and B2
add markdown, registration lines and tests only.

Decided on the way, against the plan's letter: pages are gathered in
`Plugin::start` (a `CommandSource` holds no host); `guide-acp` registers
whether or not an adapter row exists, because the page is how a person
learns to write the row. Records the pages found lagging the code, left for
their owners: ADR-0045 (`/rewind` queues, is not refused), ADR-0019 (`Wake`
is not registered under `schedule.wakes: false`), design `tui.md` §7 (the
`esc` stack has four rungs, not five), ADR-0035 §§5–6 (permission questions
reach the person; the bridge exists), ADR-0016 §4 (`mention: false`).
`guide-acp` says "no compaction": M91 changes that sentence.
