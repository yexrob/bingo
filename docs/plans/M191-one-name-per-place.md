# M191 — One name per place

ADR-0084 §1, ADR-0086, ADR-0095. The person (2026-10-16): "把重复的没有必要的命令
删除掉。能合并的合并". Picked: `/rewind` goes into `/browse`; `/agents`, `/tasks`,
`/room`, `/team` go into `/crew`. Also asked whether the room and sub-agent
views are gone: a sub-agent is stepped into from `/crew` (`⏎`) or its spawn row
(`⏎`/click); a room's chat is still drawn when a room is in view (M150), but
nothing in `/crew` steps into a room — the rooms tab has no `⏎`.

## Goal

In the terminal, the `/` palette shows one row per place the person can go. A
command whose TUI result was a static table, a usage line or a list of typed
verbs is folded into the panel that already shows the same thing. Every verb
still runs when typed in full, and `--print`, RPC and Feishu keep their text.

## Audit (bingo 0.6.6, fake provider, every command driven in tmux)

| Command | Today in the TUI | Goes to |
|---|---|---|
| `/agents` (`bingo-agents/src/command.rs`) | table; verbs stop/dismiss/message/reopen | `/crew` agents tab |
| `/tasks` (`bingo-tasks/src/command.rs`) | board text | `/crew` tasks tab |
| `/room` alias `/rooms` (`bingo-rooms/src/command.rs`) | table; seat/unseat/purpose/close | `/crew` rooms tab |
| `/team` (`bingo-agents/src/team/command.rs`) | declared roles, which are seated | `/crew` team tab |
| `/rewind` (`bingo-checkpoints/src/command.rs`) | text list | `/browse` (its `r` sheet already runs `/rewind <id> <mode>`) |
| `/experience` (`bingo-experience/src/command.rs`) | text list | `/memory` (its `experience` group exists, `bingo-mod-memory/src/rows.rs:69`) |
| `/wake` (`bingo-schedule/src/command/wake.rs`) | text; `off` | `/schedule` panel row |
| `/models`, `/provider` (`bingo-core/src/commands/`) | text / usage | `/model` panel |
| `/mcp` | plugins panel, cursor on built-ins | `/plugins` panel, cursor on the MCP group |
| `/permission` bare | prints the 5-mode table | palette inline list |
| `/rename` bare | prints usage | palette input row |

Stay as they are: model, login, logout, think, status, plugins, memory, schedule,
sessions, resume, crew, browse, help, compact, `!`, bash.promote, guide-*.
`/sessions` vs `/resume`: check whether both open the same thing; if they do,
fold one into the other by the rule below, else leave both.

## Bricks

1. **A fold is the panel mod's, one fact.** A mod that opens a panel names the
   commands that panel replaces, in its own registration (an sdk contribution
   next to its commands and interceptors, e.g. `Fold { into: "crew", names:
   ["agents","tasks","room","team"] }`; pick the spelling, keep the
   properties). Core's commands catalogue (`bingo-core/src/host/catalog.rs:215`)
   marks each folded entry's meta with `foldedInto: "<name>"`; nothing else
   records it. Mod switched off → no registration → no mark → the command is
   back in the palette, so the person never loses the way in. The kernel
   holds names only as data; no feature noun enters sdk/core. Contract test
   and schema first (regenerate as usual; restore `schema/plugin-2.json`).
2. **Readers.** The palette (`bingo-mod-palette/src/rows.rs:208,240`), the TUI
   help/`?` keys list and typeahead completion leave out entries marked
   `foldedInto`; an alias-style search hit on a folded name shows the panel's
   row (typing `/agents` offers `crew`). Dispatch is untouched: typing the full
   folded name still runs its command and its interceptors. The channel menu
   (`bingo-channels/src/menu.rs:83`) ignores folds (Feishu has no panels) and
   drops `family: "terminal"` entries, which it lists today by mistake
   (`/crew` reaches Feishu's menu and only prints its fallback).
3. **Folded into what panel opens.** Typed in full in the TUI, a folded
   command with no argument opens its panel at its place: `/agents` → crew
   agents tab, `/room` → rooms tab, `/rewind` → browse with the rewind sheet
   ready, `/experience` → memory on the experience group, `/wake` → schedule on
   the wake row, `/models`/`/provider` → model panel, `/mcp` → plugins on MCP.
   Same interceptor pattern as `bingo-mod-plugins/src/opening.rs` (empty args
   only; a verb with args falls through to the command). The text `View` stays
   as the `fallback`.
4. **`/crew <tab>`.** `bingo-mod-crew/src/command.rs`: `ArgSpec::Words` with
   `agents rooms tasks team`, offered inline by the palette; bare `/crew`
   opens the first tab as today. Update the fallback text (it names
   `/agents, /rooms and /tasks`).
5. **Rooms tab steps in.** `bingo-mod-crew/src/keys.rs` `fn room`: `"enter" =>
   Operation::View(row.id)` (`RoomRow.id` is the room's session,
   `bingo-services/src/rooms_control.rs:12`); key hint `⏎ open`. Esc from the
   room goes back as it does from a sub-agent. TestBackend snapshot of a room
   in view after the step.
6. **Team tab.** `Tab::Team` in `bingo-mod-crew/src/state.rs` with rows from
   what `/team` lists (roles, seated or not). Read-only unless `/team` has
   verbs; if it does, each becomes a key like the agents tab's. If the list is
   always a few lines, a section under the agents tab is fine instead — say
   which in the Verified section.
7. **Schedule wake row.** The pending wake (the schedule plugin's shelf, see
   `wake.rs` header) is a row of the schedule panel; `d` on it runs `/wake off`.
8. **Bare `/permission`, bare `/rename`.** Enter on the bare name opens the
   palette's inline level (M187 already offers both) instead of running the
   command: the five modes with the current one marked; the rename input row.
9. **`/mcp` lands on MCP** (`bingo-mod-plugins`, group "MCP servers",
   `groups.rs:121`): open with the cursor on that group's first row.
10. **Words.** Grep guide pages (`docs/guide`, `crates/*/src/guide*.md|rs`),
    skills and help text for the folded names and point them at the panel.

## Files

- `crates/bingo-sdk/src/` (the fold contribution + schema), `crates/bingo-core/src/host/catalog.rs`
- `crates/bingo-mod-{crew,browse,memory,schedule,models,plugins,palette,sessions}`
- `crates/bingo-channels/src/menu.rs`, `crates/bingo-surface-tui` (help/typeahead readers)
- Guide pages naming folded commands. The folded commands' own crates change
  only if their wording names a sibling command.

## Exit criteria

- [x] Contract test: a registry with the crew fold marks `agents tasks room team`
      `foldedInto: "crew"`; without the crew mod, no marks.
- [x] Palette test: bare `/` lists none of `agents tasks room rooms team rewind
      experience wake models provider mcp`; with each owning mod off, they return.
- [x] Each folded name typed bare opens its panel at its place (one test each);
      `/agents stop x`, `/room close x`, `/wake off`, `/rewind <id> both` still
      run their commands.
- [x] `/crew rooms` opens on rooms; `⏎` on a room steps into its chat; esc back.
- [x] Channel menu test: folded commands kept, `terminal` family dropped.
- [x] `/permission`, `/rename` bare open their inline level; `/mcp` cursor on MCP.
- [ ] tmux drive (own `tmux -L`, `-f /dev/null`, temp HOME, fake provider,
      `OPENAI_*`/`ANTHROPIC_*` unset): bare `/`, `/agents`, `/crew rooms` → `⏎`
      → esc, `/rewind`, `/mcp`; capture evidence belongs in the PR review.
- [ ] Gates: `cargo fmt --all -- --check`, `cargo check --workspace --all-targets
      --locked`, `cargo clippy … -D warnings`, `cargo test --workspace --locked`,
      `scripts/check_discipline.sh`, `scripts/budget.sh`, `cargo deny check`,
      tui-smoke, and `cargo check -p <non-TUI crates touched> --all-targets
      --target x86_64-pc-windows-msvc`.

## Non-goals

- Removing any verb or text view from `--print`, RPC or Feishu.
- Redesigning `/crew` or any panel beyond the tab, rows and keys above.
- The transcript (M185) and glyph changes.

## Risks

- The palette caches the catalogue: a mod switched off mid-session must bring
  its folded names back on the next `/`. Test it.
- Two mods folding the same name: last registration wins is a hidden order;
  refuse the second fold with a warn notice instead.
- A guide page or skill still telling the person to type `/agents`: it still
  works, but grep anyway so the docs name the panel.

## Verified

Verification evidence is in the PR review, outside the code repository.
Terminal checks remain incomplete; further Windows verification is deferred.
The unchecked exit criteria above are not claimed complete.

## Implementation decisions

- Fold contributions remain owned by their registering mod. Catalogue projection
  runs after description middleware and refuses conflicting or hidden chains.
- `/sessions` remains the all-directory site; `/resume` remains the recent and
  workspace palette plus direct-ID reopening. They serve different places.
- Team is a read-only fourth crew tab using the shared typed seating service.
- Concrete passive Event-only hooks use the existing owner running set for live
  delivery. Mixed, empty, decision and source hooks remain restart-only; the
  ADR-0088 amendment reconciles this with ADR-0090's crew live-switch intent.
- Owner-directed lifecycle callbacks replace command-string disable observers
  in crew, model and schedule (ADR-0096). Only effective disable clears transient
  panes; refused, pending and overridden switch outcomes preserve them.
- No dependencies, transcript or glyph changes were introduced. The frozen
  plugin schema remains unchanged.
