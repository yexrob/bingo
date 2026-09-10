# TUI

The full-screen terminal surface: what `bingo` opens when stdin and stdout are
both a terminal. It holds no session state — it folds the same frames every
other client folds and derives what it draws at render time — so what a person
sees here, a `--print` run or an IM chat on the same session can see too.

The screen is the transcript, an input box that never moves, and one status
line: the permission mode, `N needs you` and `N running` while they are true,
where you are, the window used against the whole, and the model.

## The keys

- `enter` — send the message, or open the focused block.
- `shift+enter` — a newline. `ctrl+j`, `alt+enter` and `\` then `enter` do the
  same, for terminals that cannot tell the first one apart from `enter`.
- `esc` — one ordered stack, innermost first: a sheet, then the card that is
  asking (leaving it is its own cancel or denial), then the dropdown, then the
  running turn. **One `esc` ends a turn**: every call in flight is dropped
  where it stands, a shell command's process group with it, and the activity
  row says `Stopping…` from the frame the key was pressed.
- `esc esc` — on an empty composer, the turns of this transcript newest first,
  and `⏎` to rewind to one. Offered only where the session has a `/rewind`.
- `ctrl+c` — interrupt a running turn; with nothing running, clear the input;
  with nothing to clear it says how to leave, and again within the moment
  leaves. `ctrl+d` — exit on an empty input.
- `tab` — a second ordered stack: take the dropdown's row, else queue the line
  for after the running turn, else move the ring to the next card that answers
  keys. `⏎` while a turn runs *steers* it — the turn absorbs the line at its
  next barrier — and `tab` *queues*; the box says so.
- `up/down` — your own prompt history, at the first and last line of the box.
  `down` on an empty box walks the sessions, the same list `ctrl+g` opens.
- `ctrl+a/e` — start and end of the line (`home` and `end` too). `alt+b/f` —
  one word. `ctrl+w/u/k` — delete a word, to the line start, to the line end.
- `pgup/pgdn` — scroll the transcript.
- `v · drag` — select: from the focused block or the first line on the screen
  with the key, from wherever the press landed with the mouse. `↑↓←→` or the
  drag itself take the far end of the run and the view follows it, so a run
  reaches what has scrolled away; held past the top or the foot of the
  transcript, a drag scrolls it a line at a time until you come back inside or
  let go. `y`, `ctrl+c` or letting the button up copies through OSC 52 and
  lets the run go; `esc` lets it go and copies nothing. What is copied is what
  was drawn: the cells as you saw them, trailing spaces trimmed. More than
  100 KiB is refused out loud rather than half-copied.
- `ctrl+t` — show and hide the task list. `ctrl+p` — the plugin-state sheet:
  what the plugins wrote about the session on screen, and where a panel is
  pinned into the rail (`⏎` pins a row, `⏎` again takes it back).
- `shift+tab` — cycle the permission mode.
- `1-9 · y/a/n` — answer the open dialog. `ctrl+e` — expand its preview.
- `ctrl+o` — open the newest fold one rung further, and again until it is
  whole. It only ever opens; `esc` comes back.
- `ctrl+b` — background the running command.
- `/ · !` — a command, and a shell line in the session's directory.
- `click` — on a picture, open it. Mouse reporting has to reach us at all:
  inside tmux that wants `set -g mouse on`.
- `?` — the panel with this whole table in it.

## The composer

`/help`, `/clear`, `/resume [id]` and `/exit` (`/quit`) are this surface's own
and reach no kernel. Every other `/name` and every `!line` is submitted
verbatim — the session actor parses commands, not the client — so a plugin's
command and a skill's `/name` are typed here and answered there.

`@` opens a dropdown of names this session can reach and paths under its
working directory. The names are the sub-agents under this session, or, in a
room, the seats on its roster and `@all`. The paths are the directory's own
files, obeying `.gitignore`, so a repository offers its sources and not its
build. A completed mention keeps its `@`, so the line itself says which of its
words are names and which are paths.

Pasting a picture writes it to `<data>/pictures/pasted/`, named by its bytes,
and puts `[image N]` in the line. **The line is the record**: what is sent is
derived from the tokens still in it, so deleting a token takes its picture with
it, and `@shot.png` is the other spelling of the same thing — a word that names
a file.

## Pictures

Whether this terminal draws pixels is asked at start-up, never assumed from
`TERM`. A terminal draws a picture here only if it speaks the kitty graphics
protocol, says how big a cell is, and names itself kitty 0.28, Ghostty 1.0,
iTerm2 3.5.6 or Rio 0.5.27 or newer — the list is a list because the protocol's
own query carries no answer for it, and two terminals that claim the key ignore
it. Everywhere else a picture is `[image: …]`, and a click still opens it in
whatever this system opens pictures with.

A picture fetched from the web is kept under `<data>/pictures/cache/` for
`pictures.cacheDays` days — a fortnight unless a person says otherwise, and `0`
means no cache at all.

## Settings

This surface claims two keys:

- `tui.measure` — the widest a line of prose is drawn, however wide the
  terminal is; prose then wraps at `min(width, measure)`. Absent, and `0`, are
  the terminal's own width. Tables and code never wrap: they fold to the width
  and open in a sheet.
- `update.check` — whether a start asks whether a newer release is out. On
  unless it is set to `false`; the check is once a day, off the start's own
  thread, and silent whenever it cannot answer. What it found is one row of the
  welcome box — `↑ v0.5.0 is out · bingo update` — and `bingo update` is what
  takes it.

The look is chosen from the environment rather than the settings, because it is
chosen before the kernel is up: `NO_COLOR` strips colour, `BINGO_ASCII=1`
strips the glyphs, `BINGO_MOTION=off` stills every animation, `BINGO_THEME` is
`light` or `dark` where the terminal's own background cannot be read.

`--no-print-on-exit` leaves the terminal as it was; without it the last
screenful of the conversation is printed on the way out, under a line saying
how to reopen the session.
