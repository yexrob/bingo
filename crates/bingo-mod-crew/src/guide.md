# Crew

`ctrl+n` or `/crew` opens the crew panel. `/crew agents`, `/crew rooms`,
`/crew tasks` and `/crew team` open a tab directly. `]` switches between tabs;
a disabled owner plugin hides its tabs. Tab moves focus. Up and down select
rows. Escape closes the panel and drops its live follow.

Agents show their first request, model, token usage and known cost. The
selected agent shows its current tool and latest line. A missing price is
`$—`, never a guessed zero. Enter steps into the session. Left or right
selects the model line; Enter opens that session's model picker.

`s` asks before stopping a running child; `x` asks before dismissing an idle
child and deleting its journal.
Enter confirms; Escape cancels and stays in the list.
Only its starter may do either. `m` fills `/agents message <name> ` in the
composer; finish and send it without changing sessions. For an interrupted
agent it fills `/agents reopen <name> ` instead. An existing draft is kept.

Rooms show their purpose, seats and holder. Enter opens the room's chat;
Escape returns from the chat as it does from an agent. `p`, `a` and `u` fill
the commands for purpose, seating and unseating. `c` asks before closing; Enter confirms,
Escape cancels and stays in the list. Only the holder or opener may change a room.

Tasks show subject and owner. `+` fills `/tasks add `; append `@owner` to
assign it. Space toggles done/open. `o` fills `/tasks assign <id> `.

Team lists every declared role, its agent definition and its seated session,
including roles that are not seated. It is read-only, like the `/team` command.

Bare `/agents`, `/room` (or `/rooms`), `/tasks` and `/team` also open their crew
tabs in the terminal. Other surfaces retain their plain tables.
Typed verbs remain available: `/agents stop|dismiss|message|reopen`,
`/room seat|unseat|purpose|close`, and `/tasks add|done|open|assign`.
Disable `bingo.mod.crew` in `enabledPlugins` to remove its key binding;
the crew strip then opens the palette's conversation group.
