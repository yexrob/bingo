# ADR-0054 — The plugin writes its own page

Status: accepted · 2026-09-10 · Plan: M90

## Context

The bundled `guide` skill is one page (184 lines, capped at 200 by its own
test) that describes bingo to the model: commands, sessions, permissions,
settings, hooks, skills, MCP, where files live. It is a good page and it is
stale: it names none of agents, teams, rooms, tasks, channels, schedules,
memory, checkpoints, pictures, update, gateway or ACP, nor `/agents`,
`/room`, `/tasks`, `/team`, `/memory`, `/experience`, and its settings keys
lag the claimed ones. The cause is structural: the page lives in
`bingo-skills`, far from the code that owns each noun, so every plugin that
grows leaves it behind. A bundled skill also has no directory, so it cannot
carry reference files the model would `Read` (`${BINGO_SKILL_DIR}` is empty
for it). The user asked for the memory system's shape: a short index that is
always in hand, and one file per topic read on demand.

## Decision

1. **A page is a plugin's.** `bingo_sdk::Page { name, description, body }`
   (static strings, `include_str!`-able) and `Pages(&'static [Page])` are
   data the sdk carries and the kernel never reads. A plugin that has
   something to say about itself registers `Contribution::Service { key:
   Pages::key(manifest.id), value: Arc<Pages> }` and lists
   `service:<that key>` in `provides`. `Pages::key` is `"<plugin id>.pages"`,
   one spelling in one place. No kernel door opens: the plugin catalog
   (`CatalogKind::Plugins`) already names every loaded plugin, and a typed
   service by key is already a plugin's lane onto another's fact
   (ADR-0031 §1).
2. **Every page is a bundled skill named `guide-<name>`.** `bingo-skills`
   gathers the pages once per process — the plugin ids from the catalog,
   each id's service by key — and appends them below its own bundled skills,
   so a skill on disk of the same name still overrides one (ADR-0021's
   layering unchanged). The page's `description` is its one line in the
   system prompt's `# Skills` listing: that listing is the index, as
   `MEMORY.md` is for memory. `Skill guide-rooms` or `/guide-rooms` reads the
   page.
3. **`guide` is the map, not the manual.** It keeps what is the whole's and
   no plugin's — what bingo is, running it, sessions, the settings layers and
   kernel keys, where files live — under 80 lines, and ends with the list of
   pages found, generated from the same gathering, one line each. A plugin
   not loaded has no page and no line.
4. **A page is asserted by its owner.** Each plugin's page test names the
   commands, tools and settings keys the page must mention, next to the code
   that registers them, so a verb added without a sentence fails there. The
   skills plugin asserts only shape: a name, a one-line description, a body
   under 200 lines.
5. **A plugin that is not loaded is not described.** The demo, fake and
   store plugins and the libraries (`bingo-pictures`, `bingo-update`,
   `bingo-gateway`) write no page; what a person meets of them is said on
   the page of the plugin that shows it (the TUI's page says `tui.measure`,
   pictures and the update check).

## Consequences

- The prompt grows by one line per loaded page (about a dozen); a page
  costs context only when read. The single page the model read before for
  any question now costs a map plus one topic.
- `bingo-skills` imports no plugin and no plugin imports it: the sdk type
  and a key are the contract; a page is plain data.
- The old `guide` sections on permissions, hooks and MCP move into pages
  owned by `bingo-permissions`, `bingo-hooks-shell` and `bingo-mcp`; the
  skills section becomes `guide-skills`, owned where skills are.
- A page written in the sdk's vocabulary can drift from the code beside it
  the same way the guide did, one crate away instead of a workspace away —
  §4's test is what shortens the distance.

## Supersedes

Nothing. ADR-0021's non-goal "a rooms-listing tool" is unaffected; ADR-0031
gains a use, not a rule.
