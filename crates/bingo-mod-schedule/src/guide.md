# Schedules panel

`/schedule` opens the panel. The top line names the runner holding the store.
The `by` column says `you`, `model`, or `unconfirmed` (ADR-0094).
A pending wake on this session appears after the schedules; `d` ends it.
`/wake` opens the panel on that row; `/wake off` still ends it directly.
The wake also appears in `/status`.
Schedules fire only while bingo runs. Another process may hold the store;
this panel can still run an enabled schedule now.

- ↑/↓ selects a row; space pauses or enables it.
- r runs it now; a disabled schedule must be resumed first.
- c confirms a `model` or `unconfirmed` entry as yours after showing its
  whole text and explicit mode; Enter accepts, Escape cancels. A changed text
  or mode is refused. Hidden control and bidi characters appear as escapes;
  those entries require a typed `/schedule confirm <id>` instead.
- d opens a confirmation; Enter forgets it, Escape cancels.
- → visits its session, or says it has not fired yet.
- Escape closes the panel.

Forgetting leaves the session and its transcript intact. A fire in another
process can add one extra turn when you run now.

With `bingo.mod.schedule` disabled, `/schedule` shows the plain table and
`/wake` shows its text view. All command lines still work.
