# M84 — Worked for, the recap, and the memory rows

## Goal

User, 2026-09-08, three asks after M83, Claude Code's shapes named:

1. A `Read`/`Write`/`Edit` on a memory file draws as what it is — `⏺ Recall
   from memory(bingo-rewrite-plan)`, `⏺ Write memory(prefers-short-replies)`
   — so a person can see the model is at its memory. The memory plugin
   decides; the surface draws.
2. After a turn the activity row says what it cost: `✻ Worked for 15m 11s ·
   done 13:48`. TUI only.
3. Under it, for a long turn, a recap in the model's words: `※ recap: …`.
   Computed by a plugin, shown only in the TUI, switchable off.

## How Claude Code does it (read off the screen, not the source)

`Worked for` is pure derivation — turn start to turn end, and the wall
clock at the end — standing in the spinner's row until the next turn. The
recap is model prose about the turn, present only after a long one, with a
`/config` switch: a side question after the turn whose answer goes to the
screen, not to disk. bingo has every part: a frame's `ts`, a hook at
`Phase::End` with the turn's items and the session's provider, `signal`
for live state, and a surface that reads a plugin's kind by name (M74).

## Bricks, in build order

**A. The memory rows** (`bingo-context`, then the TUI)

1. **The plugin says where memory is, as data.** When `MemoryContributor`
   renders its blocks it also writes the journal extension
   `_bingo.context`/`memory`: `{ "user": <dir>, "project": <dir> }` —
   the same two paths its headings already carry for the model, kept once
   and rendered twice. Written on capture only, so an ordinary round
   touches nothing.
2. **`tui::memory`** reads it as `tasks.rs` reads `bingo.tasks`: the whole
   contract in one file, the payload read as data. A `Read` on a file under
   either directory is `Recall from memory(<stem>)`, a `Write` is `Write
   memory(<stem>)`, an `Edit` is `Edit memory(<stem>)`; the index keeps its
   name, `MEMORY.md`. A relative `file_path` resolves against the session's
   cwd. Anything else is what it was. `called()` and `pager::title()` both
   ask this one function.
3. `TestBackend` test and a screen snapshot; §4 row in `docs/design/tui.md`.

**B. Worked for** (sdk, then the TUI)

4. **The fold keeps the last turn.** `SessionState.last_turn` becomes
   `Option<LastTurn { id, status, started_at, ended_at, usage }>`, folded
   from the `LiveTurn` it closes and the frame's `ts`; `TurnCompleted`'s
   usage stops being dropped. `SessionState::last_status()` keeps the
   readers that only wanted the verdict short. No new event, no new field
   on the wire: every part is already in the stream.
5. **Pure words**: `worked::duration(d)` → `42s` / `15m 11s` / `1h 5m`;
   `worked::clock(ts)` → local `13:48`, with the date in front when it is
   not today. The verb follows the status: `Worked for` when completed,
   `Stopped after` when interrupted, `Failed after` when failed.
6. **The row**: `band()` falls from `working` to `waiting` to `worked` —
   `✻ Worked for 15m 11s · done 13:48`, sparkle at rest, no hint, no
   breath: nothing is at work. The task list hangs under it as it did
   under the verb (`⎿  ◼ …`), so the end of a turn changes the row's words
   and nothing else; the `3 tasks (…)` summary stands only where no turn
   has run. Snapshots move by words, not rows.

**C. The recap** (`bingo-context`, then the TUI)

7. **`context::recap`**: a `Turn`/`End` hook. The turn's span is the
   items' own clocks (`started_at` … `completed_at`); under `RECAP_AFTER`
   (2 min) it does nothing. Past it, one side question — `purpose:
   recap`, the turn as lines, capped as the extractor's was — asks for one
   or two sentences on what was done and what is next, and the answer goes
   out as `signal("bingo.context", "recap", { turn, text })`. Ephemeral by
   design: a recap is about the turn that just ended. `transcript.rs` and
   `stream::drain` come back from `d414bf62^` for it.
8. **`context.recap`** (`true` unless written `false`) is the plugin's one
   settings key; off, the hook is not registered.
9. **`tui::recap`** reads the signal by name, draws `※ recap: …` under the
   worked row only while `turn == last_turn.id`, wrapped to the band's
   width, three rows at most. It is no rail card, as the shells' set is not.
10. Tests: the hook with a scripted provider and a host double that records
    signals (below the span: no request; above: one signal; provider
    refuses: nothing); `TestBackend` for the row with and without a recap.

## Files

- `crates/bingo-sdk/src/state.rs` (+ `last_turn` readers in agents, core
  tests, tui)
- `crates/bingo-context/src/{lib,memory/mod,recap,transcript,stream}.rs`
- `crates/bingo-surface-tui/src/{activity,memory,recap,worked,transcript,
  pager,rail}.rs`, `screens.rs`
- `docs/design/tui.md`, `docs/adr/0002`, `docs/adr/0006` (dated notes)

## Exit criteria

- [x] a Read/Write/Edit under a memory directory draws with the memory
      verb; the same call elsewhere is unchanged; `TestBackend` + snapshot
- [x] `last_turn` carries id, status, clocks and usage; every gate green
- [x] the worked row after completed / interrupted / failed; the list hangs
      under it; no row before the first turn; `TestBackend` for each
- [x] a turn under two minutes asks nothing; over it, one signal; a recap
      draws under the worked row and leaves with the next turn
- [x] `context.recap = false` registers no hook
- [x] tmux hands-on drive before release (TUI-visible)

## Non-goals

- Print, RPC, ACP, channels: the worked row and the recap are the TUI's.
- A memory tool, or a kernel field naming a call's paths: the surface
  derives the row from the input and the plugin's published directories.
- A recap threshold in settings: one switch, one constant.
- Persisting recaps: `signal`, not `extend`; a resumed session has none.

## Risks

- R-band: the recap arrives seconds after the worked row and adds up to
  three rows to the band, which pushes the transcript up by that much —
  the growth the task list already takes (§3). Accepted; it arrives once.
- R-relative: a model writing a memory with a relative path draws the plain
  row if the cwd is not the session's. The prompt names absolute paths.
- R-cost: a recap is one request per long turn at the session's model.
  Bounded by the cap on the transcript and 160 output tokens; off by key.

## Verified (2026-09-08)

Two things moved from where the plan put them. **The directories are
published at session start, not on capture**: the baseline hook already
writes the plugin's journal state once per session, and a capture that
wrote two frames broke five tests that count one frame per generation —
the directories are fixed by the session's cwd, so once is the right
number. **The `Glyphs` table folded `branch`/`corner` into `tree`** to
seat the recap mark under the sixteen-field cohesion rule.

The sdk change touched every reader of `last_turn` (eleven test sites in
core and the rpc suite, one in `bingo-agents`, three in the TUI) and the
committed `schema/rpc.json`; `last_status()` kept each to one line. The
TUI's Windows check cannot run on this box (`aws-lc-sys` will not
cross-build); `bingo-context` checks clean for `x86_64-pc-windows-msvc`.

```text
== fmt / check / clippy (-D warnings)   exit 0
== bingo-context                         105 passed
== bingo-surface-tui                     1087 passed, 2 ignored
== test --workspace --no-fail-fast       88 suites, 4308 passed, 0 failed, 2 ignored
== discipline                            ok (pre-existing plan-length warns only)
== budget                                ok
== check --target x86_64-pc-windows-msvc bingo-context   ok
```

tmux drive (120×40, fake provider, harness-owned server, `BINGO_MOTION=off`,
`bypassPermissions`), read off the pane:

```text
⏺ Bash(sleep 2)
  ⎿  $ sleep 2
     [Exited with code 0]
⏺ Slept a little.
✻ Worked for 2s · done 15:02

⏺ Recall from memory(the-build)
  ⎿  …
⏺ Write memory(MEMORY.md)
  ⎿  … @@ -1 +1,2 @@ …
⏺ Recalled and noted.

⏺ Bash(sleep 125)
  ⎿  $ sleep 125
     [Killed after 120s timeout]
⏺ Done the long thing.
✻ Worked for 2m 0s · done 15:05
※ recap: Slept two minutes on purpose and confirmed the wait works. Nothing is left to push.
```

The recap arrived about a second after the worked row, from the fake's
`side` deck, and the band grew by its one row; the next turn took both
rows away. Carried: the shell tool's own 120 s ceiling ended the long
command, which is not this plan's.

### Later the same day (2026-09-08, user-directed)

Two changes after the drive. **The worked row and the recap are dim, and
neither is pinned over the composer**: both had stood in the verb's slot
above the input box; a row about what was said belongs with it, so the
worked row is now the transcript's last block, beside the failed turn's
line (`transcript::closing`), and scrolls with the answer. **The recap is
gone** — "去掉这个recap的功能吧 只保留 worked for xx": the hook, the
`context.recap` key (the plugin claims no settings again), the signal, the
TUI module and the glyph are deleted; `transcript.rs`, `stream::drain` and
`schemars` in `bingo-context` went with them. Found on the way: the recap
had measured every item the kernel hands a turn-end hook, which is the
model's whole view, so it fired after a 1m 11s turn; and a word wider than
the line — Chinese prose — dropped under the mark before it instead of
filling the line it started on (`wrap::place`, fixed and kept).
