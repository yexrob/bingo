# Conversation model v4 — CC parity (D103–D109)

Drafted 2026-08-16 after the user's verdict on v3 in real use; revised same
day after the user's three rulings (member model = CC subagent, quiet
contract removed, zoom shows the full transcript). Status: awaiting final
go-ahead; batches dispatch after it.

Evidence base: `/Users/yexrob/Episodes/Resources/research/claude-code-re` —
the leaked 2.1.88 TypeScript tree (primary source for UI shapes and
mechanics) and 2.1.221 binary strings (the direction: `main` as the
reserved recipient, `teammateMode`). Citations are into the 2.1.88 tree.

## The verdict on v3

v3's delivery layer was convergent with CC and survives nearly whole: one
SendMessage verb, `main` reserved (2.1.221 uses exactly this name),
deliver-and-wake, queue-when-busy, sender markers, no agent→user channel,
permission asks as the one escalation path. What real use rejected is v3's
**view layer**: conversations as switchable buffers spliced into one
scrollback. CC has no in-app conversation switching at all. Its answer to
"many agents, one terminal" is:

> **One conversation** (the main transcript) + **a persistent status
> layer** (agent tree, footer pills, task list) + **a zoomed view** that
> temporarily swaps the screen to one agent's transcript (typing routes to
> that agent) + optionally **real terminal panes** (tmux).

v4 replicates that interaction 1:1, with rooms as bingo's one extension,
expressed inside CC's grammar.

## The member model (user ruling: CC subagent, not CC teammate)

CC has two agent runtimes; v4 copies the **subagent** (`local_agent`) one:

- An agent is spawned by the `Agent` tool (or the crew), runs turns, and is
  **interactive throughout**: messages sent while it runs queue and drain at
  its next tool round; messages sent after it stopped **resume it** — from
  the in-memory task or from its on-disk transcript (CC
  `SendMessageTool.ts:808-866`). bingo's registry + inbox + deliver already
  implement exactly this; nothing structural changes on the domain side.
- One stable identity, one transcript, resumable across sessions — bingo's
  per-instance history already satisfies this.
- **Not copied** (teammate-only machinery, explicitly rejected): the
  always-alive idle loop, team-lead identity, file mailboxes, plan-approval
  and shutdown protocols, autonomous task claiming, idle notifications with
  peer-DM summaries. No `Team`/`team-lead` vocabulary anywhere.

## The surfaces

1. **The transcript** — the inline flow, main's conversation with the user,
   the only thing the composer addresses by default. All v3 buffer machinery
   (bar, `switch_to`, excursions, replays, dividers, `/open`) retires.
2. **The status layer** — persistent, around the composer:
   - *Agent tree* (CC `TeammateSpinnerTree` shape, populated from bingo's
     registry): a `@main` row + one row per agent + a hide row. Row shape
     `@scout: <current activity> · 12 tool uses · 8.3k tokens`; idle/stopped
     rows `Idle for 14s · enter to view` / `[stopping]`. `ctrl+t` cycles
     `none → tasks → agents` (replacing the tasks → directory cycle).
     `shift+↑/↓` selects, `enter` zooms, `k` stops, `ctrl+shift+o` toggles a
     3-line message preview per row.
   - *Footer pills* (when the tree is off): `@main @scout @writer · shift+↓
     to expand` — bold = zoomed, dim = idle, identity colors. Replaces the
     v3 conversation bar.
   - *Task list* (ctrl+t first stop): bingo's task area, plus `(@owner)` in
     the owner's color when the owner is a running agent, and `▸ blocked by
     #3` (display only — no assignment protocol, no claiming).
3. **The zoomed view** — replaces v3's DM/room buffers and the D96/D100
   observation modal. Entered from the tree/pills (`enter`), it swaps the
   screen (the alt-screen host) to one agent:
   - Header: `Viewing @scout · esc to return` + the task prompt line, name
     in identity color.
   - Body: **the agent's full transcript** (user ruling) — rendered through
     the console renderer with the D96/D100 `walk`/`Protagonist` attribution
     as backend, process rows as collapsed activity groups (D99 machinery).
   - **The composer stays live and routes to this agent**: local echo
     immediately, delivery through the existing inbox (queue mid-run, drain
     at the next round; resume if stopped — CC semantics, already bingo's).
   - `esc`: running → abort the current run only; idle/stopped → return.
     `shift+tab` cycles the *viewed agent's* permission mode. Auto-return on
     kill/failure; a finished agent's view stays open.
   - A **room zoom** is the same view over a room's log; typing posts to the
     room (auto-join with membership line). Reading any room is free.
4. **The background dialog** (on-demand modal, absorbs the D95 directory):
   sections Agents · Shells · Rooms; `↑/↓ select · Enter detail · f zoom ·
   x stop · Esc close`.

**Composer grammar** (transcript mode):

- `@scout <message>` — direct send **bypassing the model** (CC
  `parseDirectMemberMessage`): into the agent's inbox as the user, a
  transient `Sent to @scout` notice, nothing in main's history. Typeahead
  lists agents (`@scout · send message · running`).
- `#room <message>` — same shape for rooms (auto-join if needed).
- Anything else — a normal turn to main.

**What the transcript shows of agent life** (CC's exact tiering):

| Event | Rendering |
|---|---|
| dispatch (`Agent` call) | `⏺ Agent(<description>)` row; live progress = last 3 condensed messages, or one `In progress… · N tool uses · Xk tokens` line when space is short; grouped tree when one turn spawns several |
| completion | the row settles to `Done (N tool uses · Xk tokens · 1m 4s)`; the notification renders as ONE dim `● <summary>` line; main's narration follows as ordinary prose (no quiet marker — see below) |
| agent → main message | one visible line `@scout❯ <summary>` in the sender's color; full body in ctrl+o transcript mode only |
| agent failure | the v3 alert line stays (`⚠ @scout · reason` + attention) |
| state changes (running/idle/stopped) | the tree row and pill only; never a transcript line |

**The quiet contract is removed** (user ruling, strict parity): D102's
`[[quiet]]` marker, its system-prompt block and its render rule are deleted;
digest turns render as ordinary main prose, exactly as CC's leader narrates.
The D98 wake/debounce and the dim notification line remain the noise
control, as they are in CC.

**Identity**: the v3 palette/avatars are the color system — one stable color
per agent (main keeps its reserved slot), used in tree rows, pills, zoom
headers, `@name❯` lines, task owners. Kitty avatars stay as bingo's flavor
wherever a sender renders; chip fallback unchanged.

**Rooms** (the one non-CC concept, in CC grammar): a room is a named
broadcast group with a log and membership. Delivery, membership lines,
main-debounce: v3 rulings survive. Presentation: a `#room❯ <summary>`
transcript line when a room message addresses the user, the room zoom, and
`#room <msg>` in the composer.

**Pane mode** (last batch, feature-gated): `agentPanes: auto|tmux|off` —
inside tmux, agents may spawn as real panes (main left, agents tiled right,
borders in identity colors, `bingo` attached to the agent's session);
outside tmux, a detached `bingo-swarm-<pid>` session with an attach-hint
banner. In-process is default and fallback. iTerm2 out of scope.

## What retires / survives from v3

| v3 piece | Fate |
|---|---|
| SendMessage + addressing + urgent + wake + debounce (D98) | survives whole |
| user-DM runs invisible to main (D98) | survives |
| failure alert line + D79 (D98) | survives |
| projection `walk`/`Protagonist` (D96/D99/D100) | survives as the zoom renderer's backend |
| collapsed activity groups for agent process (D99) | survives, feeds the zoom body |
| identity colors + avatars incl. main's (D99) | survives, roles per CC |
| Said-unread + mention accounting (D99) | survives, feeds pills/tree badges |
| `@main` vocabulary (D101) | survives (2.1.221 agrees) |
| silence contract (D102) | **retires** (user ruling: strict parity) |
| conversation bar (D90) | **retires** → footer pills |
| buffers/excursions/switch/replay (D89) | **retires** → zoomed views |
| DM/room as flow conversations | **retires** → zoomed views |
| ctrl+t directory (D95) | **retires** → tree cycle + background dialog |
| observation modal (D96/D100) | **retires** as a surface; projection reused |
| `/open`, route receipts, empty-DM note, tab door | **retire** |

## Batches

- **D103 — the single transcript.** Bar/buffers/excursions/`/open` retire;
  composer `@name`/`#room` direct-send + typeahead + transient receipts;
  the quiet contract removed.
- **D104 — the status layer.** Agent tree (rows, selection, previews,
  ctrl+t cycle, shift+↑/↓), footer pills, task-owner display.
- **D105 — the zoomed view.** Alt-screen live view over one agent's full
  transcript: header, projection-backed body, live tail, composer routing,
  esc/stop/permission-mode semantics; room zoom included.
- **D106 — transcript tiering.** `@scout❯` message lines, dim notification
  lines, grouped dispatch trees, live-progress condensation.
- **D107 — the background dialog.** The modal absorbing the directory;
  detail views; stop/zoom actions.
- **D108 — polish + docs.** feedback-states/guide/README rewrite for the
  new grammar; v3 leftovers sweep.
- **D109 — pane mode (gated).** tmux backend behind `agentPanes`.

## Resolved forks

- Member model = CC **subagent** semantics (interactive, resumable); the
  teammate machinery (idle loop, mailbox files, team-lead, plan approval,
  autonomous claiming) is explicitly not copied.
- Quiet contract: removed, strict parity.
- Zoom body: the agent's full transcript.
- Pane mode: last, gated, tmux only.
