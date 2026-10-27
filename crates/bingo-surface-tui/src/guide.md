# TUI

The full-screen terminal surface: what `bingo` opens when stdin and stdout are
both a terminal. It holds no session state — it folds the same frames every
other client folds and derives what it draws at render time — so what a person
sees here, a `--print` run or an IM chat on the same session can see too.

The screen has three bands: the stream, the dock with its state edge and input,
and the footer. The footer names who needs you, the model, non-default mode,
context from 70%, active agents and shells, with the available keys on the right.

## The keys

- `ctrl+f` — Browse by turns; line search when the Browse mod is disabled.
- `enter` — send the message, or open the focused block.
- `shift+enter` — a newline. `ctrl+j`, `alt+enter` and `\` then `enter` do the
  same, for terminals that cannot tell the first one apart from `enter`.
- `esc` — one ordered stack, innermost first: a sheet, then the card that is
  asking, then the dropdown, then the running turn. A question or form is skipped
  and the model continues; a permission ask is denied and stops the turn. From
  a draft set aside, Escape returns to the ask. **One `esc` on a running turn ends it**: every call in flight is dropped
  where it stands, a shell command's process group with it, and the activity
  row says `stopping` from the frame the key was pressed.
- `esc esc` — on an empty composer, the turns of this transcript newest first,
  and `⏎` to rewind to one. Offered only where the session has a `/rewind`.
- `ctrl+c` — interrupt a running turn; with nothing running, clear the input;
  with nothing to clear it says how to leave, and again within the moment
  leaves. `ctrl+d` — exit on an empty input.
- `tab` — a second ordered stack: take the dropdown's row, else queue the line
  for after the running turn, else move the ring to the next placed row that answers
  keys. `⏎` while a turn runs *steers* it — the turn absorbs the line at its
  next barrier — and `tab` *queues*; the box says so.
- `up` — on an empty box, edit the newest line you queued.
- `up/down` — your own prompt history, at the first and last line of the box.
  `down` keeps the history gesture; `ctrl+r` opens sessions in the palette.
  `ctrl+g` returns to a pending ask.
- `ctrl+a/e` — start and end of the line (`home` and `end` too). `alt+b/f` —
  one word. `ctrl+w/u/k` — delete a word, to the line start, to the line end.
- `pgup/pgdn` — scroll the transcript. While it is held back, the row under it
  says `↓ 37 lines below`; `end`, or a click on that row, follows the foot again.
- `v · drag` — select: from the focused block or the first line on the screen
  with the key, from wherever the press landed with the mouse. `↑↓←→` or the
  drag itself take the far end of the run and the view follows it, so a run
  reaches what has scrolled away; held past the top or the foot of the
  transcript, a drag scrolls it a line at a time until you come back inside or
  let go. `y`, `ctrl+c` or letting the button up copies through OSC 52 and
  lets the run go; `esc` lets it go and copies nothing. What is copied is what
  was drawn: the cells as you saw them, trailing spaces trimmed. More than
  100 KiB is refused out loud rather than half-copied.
- `ctrl+t` — show and hide the task list and activity views.
- Plugins draw where they place a view: a transcript row, the activity band,
  or the status line. Legacy panel and live payloads remain data until placed.
- `shift+tab` — cycle the permission mode.
- `up/down · enter` — answer the open ask; a letter starts the no and its reason.
- `ctrl+o` — open the newest fold one rung further, and again until it is
  whole. In the pager, `esc`, `q` or `ctrl+o` closes it without answering an ask.
- `ctrl+b` — background the running command.
- `/ · !` — `/` opens commands in the palette; `!` runs a shell line in the session's directory.
- `click` — on a picture, open it. Mouse reporting has to reach us at all:
  inside tmux that wants `set -g mouse on`.
- `?` — keys in the palette; type to search commands, skills, sessions and keys together.

## The composer

`/clear` and `/exit` (`/quit`) are this surface's own and reach no kernel.
The palette mod owns `/help` and `/resume [id]`. Every other `/name` and every `!line` is submitted
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

### Experimental Windows image Ctrl+V

Native Windows interactive TUI only: set `$env:BINGO_WINDOWS_IMAGE_PASTE = '1'`
in PowerShell before launching `bingo` to opt in; remove that variable to disable.
Default runs and non-Windows/headless runs start no companion. This does not
require changing Ctrl+C/Ctrl+V or Windows Terminal settings.

The opt-in starts a persistent system Windows PowerShell process and compiles
bundled managed C# at runtime. It installs a desktop keyboard hook, but only
physical Ctrl+V with an image and a proven live, raw, focused Bingo console/pane
may be translated into console input. Ordinary text, Ctrl+C, other modifiers,
injected keys and uncertain focus pass through untouched. Mixed text+image
clipboards preserve normal text paste. UIA reads only
non-document focus metadata, not terminal content. Clipboard bytes still use
Bingo's existing image reader; the helper never changes the clipboard or logs keys.

An identifying marker is visible in the title while the helper is active; the
terminal restores the previous title on exit. The helper stops with the TUI or
its owner/parent, even if the owner dies during compilation. Startup waits at
most eight seconds, plus bounded shutdown. The launcher uses inline Add-Type
over extracted C# source, not a .ps1; execution policy settings are unchanged.
Constrained Language/AppLocker, missing UI Automation, inaccessible console or
ambiguous/stale pane focus can disable interception; normal terminal paste
continues unchanged. No policy setting change, elevation or settings change is attempted.
WSL, remote/multiplexed terminals and other terminal hosts are not supported.
This is experimental: automated isolated-console tests do not establish real
Windows Terminal split-pane correctness.

## Pictures

Whether this terminal draws pixels is asked at start-up, never assumed from
`TERM`. A terminal draws a picture here only if it speaks the kitty graphics
protocol, says how big a cell is, and names itself kitty 0.28, Ghostty 1.0,
iTerm2 3.5.6 or Rio 0.5.27 or newer, or a herdr pane (`libghostty`) — the list
is a list because the protocol's own query carries no answer for it, and two
terminals that claim the key ignore it. Everywhere else a picture is
`[image: …]`, and a click still opens it in whatever this system opens pictures
with. In herdr before 0.9, pictures need
`[experimental] kitty_graphics = true` in its config, and a herdr pane without
it is told so once, when the run opens; 0.9 and newer draw them by default.

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

A mod pane sits above or beside the transcript, or floats over it. Click
`<title> ▸` in status to reopen a collapsed pane as an overlay; click a waiting
pane to open it. On a focused pane, `alt+p` or its title’s `place ▾` cycles
places that fit. Your moves last until this TUI exits. The pane has its own
wheel scrolling and drag-to-copy selection.

## Choosing a model

Submit `/model` to open the model picker with the current model selected;
`/model gpt` or `/model openai/` opens it filtered. ↑↓ selects, ←→ sets
effort, * toggles the pin, Tab keeps, Enter uses, Escape backs out; Alt+M cycles pins, Alt+T efforts.
Unpinning keeps the same model selected in its provider group and shows a brief toast.
Enter on a filter nothing matches uses that name, on the current model's
provider when it names none. `/login` opens the provider list with an unsigned
provider selected when available; one Escape closes it. `/guide models` explains setup.
`/model openai/gpt-5.4` tries a model in this session; `--keep` also saves it for
new sessions. The footer shows model and effort, with `this session` for a temporary choice.
Project and command-line settings win; `--model` and `--provider` write none.

`/think high` changes the requested effort for this session. Add `--keep`
to save it for new sessions. `/think off` omits the reasoning parameter;
the provider's default then applies. Model reasoning metadata is informative
and does not prohibit a requested effort.

`/model pin openai/gpt-5.4` adds an offered model to the ordered `pinned`
setting. `/model unpin openai/gpt-5.4` removes it. Both preserve unrelated
user settings and TOML comments, and neither changes the active model.

The model catalogue supplies context and output limits, prices per million
tokens (input, output and cache reads), image input, chat suitability, dated
aliases, distinct supported effort levels and pin state. Missing prices mean
unknown. Unknown models remain visible as chat candidates. Facts come from
the bundled snapshot; listing facts never fetches prices from a provider.

If the current context exceeds the target model's compaction threshold, the
switch waits behind the active turn and compacts on the current model first.
The outcome reports the before and after token counts. Failed or insufficient
compaction leaves the model unchanged. Switching to a model without image
input keeps pictures in the transcript and warns that the model cannot see them.

For instance settings and credential rules, see `/guide tui-providers`.

`?` or `/help` opens the keys sheet. `alt+m` opens models and `alt+t` changes effort.
`ctrl+v` pastes an image from the clipboard.
