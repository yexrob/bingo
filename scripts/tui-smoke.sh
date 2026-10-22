#!/usr/bin/env bash
# The terminal surface through a real pty. tmux gives us the terminal a unit
# test cannot: a reply arrives, Esc interrupts a running turn, a permission
# in the dock answered ⏎ really writes the file, and ctrl+c twice hands the
# terminal back with status 0.
#
# Every wait is a bounded poll of `capture-pane`, never a fixed sleep: the
# script is as fast as the binary and fails loudly instead of flaking.
set -euo pipefail
unset OPENAI_API_KEY OPENAI_BASE_URL ANTHROPIC_API_KEY ANTHROPIC_BASE_URL

ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug/bingo"
SESSION=smoke
# 200 polls of 50 ms: ten seconds is far more than any step needs.
TRIES=200

command -v tmux >/dev/null || { echo "tui-smoke: tmux is required"; exit 1; }
[ -x "$BIN" ] || { echo "tui-smoke: build first (cargo build): $BIN"; exit 1; }

WORK=$(mktemp -d)
SOCKET="${TUI_SMOKE_SOCKET:-${TUI_SMOKE_SOCKET_PREFIX:-bingo-smoke}-${WORK##*/}}"
if tmux -L "$SOCKET" list-sessions >/dev/null 2>&1; then
  rm -rf "$WORK"
  echo "tui-smoke: socket already in use: $SOCKET" >&2
  exit 1
fi
cleanup() {
  tmux -L "$SOCKET" kill-server 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT
mkdir -p "$WORK/home/.bingo" "$WORK/cwd"
# The smoke reaches nothing outward. A run with a terminal asks once a day
# whether a newer release is out (M63); this one says no.
printf '{ "update": { "check": false } }' >"$WORK/home/.bingo/settings.json"

pane() { tmux -L "$SOCKET" capture-pane -p -t "$SESSION"; }
keys() {
  # The KeyBar changes immediately; encode Escape unambiguously so the next
  # typed letter cannot become an alt chord during the terminal's ESC timeout.
  if [ "$#" -eq 1 ] && [ "$1" = Escape ]; then
    tmux -L "$SOCKET" send-keys -t "$SESSION" -l "$(printf '\033[27u')"
  else
    tmux -L "$SOCKET" send-keys -t "$SESSION" "$@"
  fi
}

# Wait for `$1` to appear on the pane, or fail with what was there instead.
await() {
  local needle="$1" i=0
  while [ "$i" -lt "$TRIES" ]; do
    if pane | grep -qF -- "$needle"; then return 0; fi
    sleep 0.05
    i=$((i + 1))
  done
  echo "tui-smoke: timed out waiting for: $needle" >&2
  echo "--- pane ---" >&2
  pane >&2
  return 1
}

step() { printf '  %s\n' "$1"; }

# Wait for `$1` to leave the pane. Motion takes frames, so this polls like
# `await` rather than looking once.
vanish() {
  local needle="$1" i=0
  while [ "$i" -lt "$TRIES" ]; do
    if ! pane | grep -qF -- "$needle"; then return 0; fi
    sleep 0.05
    i=$((i + 1))
  done
  echo "tui-smoke: it never left: $needle" >&2
  echo "--- pane ---" >&2
  pane >&2
  return 1
}

# A reply of eighty numbered rows: more transcript than the screen holds.
# They are list items, so markdown keeps them one to a row.
long_reply() {
  local text="" i
  for i in $(seq 1 80); do text="$text- line $i\\n"; done
  printf '{"responses":[{"steps":[{"text":"%s"}]}]}' "$text"
}

# What a terminal sends when its window takes the focus or loses it, once
# focus reporting is on: `\033[I` and `\033[O`.
focus() {
  tmux -L "$SOCKET" send-keys -t "$SESSION" -l "$(printf '\033[%s' "$1")"
}

# One SGR mouse report: button, column, row (1-based, as a terminal sends it).
mouse() {
  tmux -L "$SOCKET" send-keys -t "$SESSION" -l "$(printf '\033[<%s;%s;%sM' "$1" "$2" "$3")"
}

# A real picture on disk, since a picture reaches a transcript by being read.
# Two blue pixels: the smallest thing the fs tool will call `image/png`.
SHOT=iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEElEQVR42mO4WR4ORAwQCgAwWgadFBKCuwAAAABJRU5ErkJggg==
picture() { printf '%s' "$SHOT" | base64 -d >"$WORK/cwd/shot.png"; }

# Start bingo on a scripted provider. The script's own text never appears in
# the command line, so `await` matches the output and not the echo of itself.
# `$2` is any extra environment the step wants, `$3` any extra flags.
start() {
  printf '%s' "$1" >"$WORK/script.json"
  keys "HOME=$WORK/home XDG_DATA_HOME=$WORK/data ${2:-} BINGO_FAKE_SCRIPT=$WORK/script.json $BIN --cwd $WORK/cwd ${3:-}" Enter
  await 'fake-1'
}

# Press `$1` until `$2` shows. A dialog ignores keys until its guard has
# passed (the kernel's, not ours), so the first press may land on nothing.
# An ask holds its keys for 400 ms after it opens (M197); a key sent into
# that window is dropped on purpose, so the script waits it out once.
guarded() {
  sleep 0.5
}

press_until() {
  local i=0
  while ! pane | grep -qF -- "$2"; do
    if [ "$i" -ge "$TRIES" ]; then
      echo "tui-smoke: pressing '$1' never produced: $2" >&2
      echo "--- pane ---" >&2
      pane >&2
      return 1
    fi
    keys "$1"
    sleep 0.05
    i=$((i + 1))
  done
}

# Leave, and prove the shell has the terminal back with the status we expect.
# Wait until the shell holds the pane again. A key typed while bingo is
# still leaving goes to bingo, not to the shell, and is never seen again.
gone() {
  local i=0
  while [ "$(tmux -L "$SOCKET" display-message -p -t "$SESSION" '#{pane_current_command}')" = bingo ]; do
    if [ "$i" -ge "$TRIES" ]; then
      echo "tui-smoke: bingo never left the pane" >&2
      echo "--- pane ---" >&2
      pane >&2
      return 1
    fi
    sleep 0.05
    i=$((i + 1))
  done
}

# Each leaving is numbered: the pane keeps the last one's line, and a wait
# for the same words would match it before this shell has even come back.
LEFT=0
left() {
  gone
  LEFT=$((LEFT + 1))
  keys "echo \"left $LEFT with: \$?\"" Enter
  await "left $LEFT with: 0"
}

# A reply may be visible before its turn finishes. Exit is a persistent
# readiness check; the idle notice scenario below covers ctrl+c twice.
finish() {
  keys C-u C-d
  left
}

HOME="$WORK/home" XDG_DATA_HOME="$WORK/data" XDG_CONFIG_HOME="$WORK/config" SHELL=/bin/sh tmux -L "$SOCKET" -f /dev/null new-session -d -s "$SESSION" -c "$WORK/cwd" -x 120 -y 40 \
  "PS1='smoke\$ ' /bin/sh -i"
keys 'echo "shell: $((1 + 1))"' Enter
await 'shell: 2'

step 'a reply reaches the transcript'
start '{"responses":[{"steps":[{"text":"Hello from the smoke test."}]}]}'
keys 'say hello' Enter
await 'Hello from the smoke test.'
if pane | grep -Eq '# (Tasks|Experience|Memories)|Context injection|context:memory'; then
  echo 'tui-smoke: internal context appeared in the conversation' >&2
  pane >&2
  exit 1
fi
finish

step 'the model picker opens and gives the composer back'
start '{"responses":[{"steps":[{"text":"nothing to do"}]}]}'
keys '/model' Enter
# The model sheet opens in the dock, and its keys replace the composer's.
await '↑↓ model'
keys Escape
vanish '↑↓ model'
finish

step 'esc interrupts a turn that is still waiting'
start '{"responses":[{"steps":[{"delay":{"ms":60000}},{"text":"too late"}]}]}'
keys 'wait for it' Enter
await 'esc stop'
keys Escape
await 'stopped'
finish

# The same keypress before a single byte has come back: the request is still
# being established, which is where a long context spends its seconds.
step 'esc interrupts a turn whose request has not answered yet'
start '{"responses":[{"steps":[{"delayBeforeStream":{"ms":60000}},{"text":"too late"}]}]}'
keys 'wait for the first byte' Enter
await 'esc stop'
keys Escape
await 'stopped'
finish

step 'a page up releases the tail, and the foot takes it back'
start "$(long_reply)"
keys 'say a lot' Enter
await ' line 80'
keys PPage
await ' line 20'
vanish ' line 80'
keys NPage
await ' line 80'
finish

step 'the wheel scrolls the transcript'
start "$(long_reply)"
keys 'say a lot' Enter
await ' line 80'
for _ in 1 2 3 4 5 6 7 8 9 10; do mouse 64 10 5; done
vanish ' line 80'
finish

step '/browse reads full blocks and esc gives the composer back'
start "$(long_reply)"
keys 'say a lot' Enter
await ' line 80'
keys '/browse' Enter
await 'browse · turn'
keys 'line 42'
await 'line 42'
keys Enter
await '1/1'
keys Escape
vanish '1/1'
# The shortcut hint is also visible in Browse; even an absent header can be
# an intermediate redraw. Require a persistent composer echo before finish.
# CSI-u keeps this Escape separate from the probe under a delayed tty read.
keys -l "$(printf '\033[27u')"
keys 'smoke-ready'
await 'smoke-ready▍'
finish

step 'ctrl+f finds drawn cells with Browse disabled'
printf '[update]\ncheck = false\n[enabledPlugins]\n"bingo.mod.browse" = false\n' >"$WORK/home/.bingo/settings.toml"
start "$(long_reply)"
keys 'say a lot' Enter
await ' line 80'
keys C-f 'line 79'
await '1 of 1'
vanish 'browse · turn'
keys Escape
finish
printf '[update]\ncheck = false\n' >"$WORK/home/.bingo/settings.toml"

step 'detail opens a read well in place and gives the input back'
printf 'first\nsecond\nthird\n' >"$WORK/cwd/detail.txt"
start '{"responses":[{"steps":[{"toolCall":{"name":"Read","input":{"file_path":"detail.txt","offset":2}}}]},{"steps":[{"text":"Read finished."}]}]}'
keys 'read the file' Enter
await 'Read finished.'
keys C-o
await 'step'
keys Enter
await '2   second'
keys Escape
vanish '2   second'
await 'fake-1'
finish

step 'an @ offers the paths under the session and enter takes one'
printf '[package]\nname = "smoke"\n' >"$WORK/cwd/Cargo.toml"
start '{"responses":[{"steps":[{"text":"nothing to do"}]}]}'
keys '@Car'
await '▎ Cargo.toml'
keys Enter
# The caret follows the separator the pick leaves after the path.
await '@Cargo.toml ▍'
finish

step 'the keys sheet opens on ? and closes on esc'
start '{"responses":[{"steps":[{"text":"nothing to do"}]}]}'
keys '?'
# The sheet takes the composer's place while open: wait for its own head.
await '/help lists every command'
keys Escape
await 'fake-1'
vanish '/help lists every command'
finish

step 'ctrl+r opens the sessions sheet and esc closes it'
start '{"responses":[{"steps":[{"text":"nothing to do"}]}]}'
keys 'ask me' Enter
await 'nothing to do'
keys C-r
await 'type to filter by title or directory'
keys Escape
vanish 'type to filter by title or directory'
await 'fake-1'
finish

step 'a permission in the dock answered enter runs the tool'
start '{"responses":[
  {"steps":[{"toolCall":{"name":"Write","input":{"file_path":"note.txt","content":"written by the smoke test\n"}}}]},
  {"steps":[{"text":"Wrote it."}]}]}'
keys 'write the note' Enter
await '↑↓ choose'
await '▎ Yes'
guarded
keys Enter
await 'Wrote it.'
[ -f "$WORK/cwd/note.txt" ] || { echo "tui-smoke: the approved Write wrote nothing" >&2; exit 1; }
grep -q 'written by the smoke test' "$WORK/cwd/note.txt"
finish

step 'a notice expires and ctrl+c twice exits the idle composer'
start '{"responses":[{"steps":[{"text":"nothing to do"}]}]}'
keys C-c
await 'ctrl+c again to quit'
# It holds its window and goes on its own: nothing is pressed to dismiss it.
vanish 'ctrl+c again to quit'
await 'fake-1'
keys C-c C-c
left

step 'a question on a window nobody watches notifies and nothing else'
start '{"responses":[
  {"steps":[{"toolCall":{"name":"Write","input":{"file_path":"away.txt","content":"written while away\n"}}}]},
  {"steps":[{"text":"Wrote it."}]}]}'
# The window loses the focus, so the question that follows goes to the desktop
# as well as to the screen — and not one byte of it may land on the pane.
focus O
keys 'write the note' Enter
await '↑↓ choose'
guarded
keys Enter
await 'Wrote it.'
[ -f "$WORK/cwd/away.txt" ] || { echo "tui-smoke: the approved Write wrote nothing" >&2; exit 1; }
if pane | grep -qE 'notify|777|Ptmux'; then
  echo 'tui-smoke: a notification was printed onto the screen' >&2
  pane >&2
  exit 1
fi
focus I
finish

# tmux is a terminal of its own, so the graphics probe is not asked through it
# (M46) and every picture here takes design §5's degrade. That is the half of
# the row this script can prove on a real terminal: the words are right and no
# escape of a protocol the terminal never agreed to reaches the screen. The
# other half — a terminal that answers the probe and is sent the picture — is
# driven by `crates/bingo/tests/pty/`, which can answer it.
step 'a picture is named where the terminal does not draw pictures'
picture
start '{"responses":[
  {"steps":[{"toolCall":{"name":"Read","input":{"file_path":"shot.png"}}}]},
  {"steps":[{"text":"That is the picture."}]}]}'
keys 'look at it' Enter
# Each step keeps its row; its picture shows when that output is opened.
await '¶ read    shot.png'
await 'That is the picture.'
keys C-o
await 'esc close'
keys Enter
await '[image: image/png]'
await 'That is the picture.'
if pane | grep -qE '_Ga=|10eeee'; then
  echo 'tui-smoke: a graphics sequence reached a terminal that was never asked' >&2
  pane >&2
  exit 1
fi
finish

step 'BINGO_GRAPHICS=off asks nothing and still names the picture'
picture
start '{"responses":[
  {"steps":[{"toolCall":{"name":"Read","input":{"file_path":"shot.png"}}}]},
  {"steps":[{"text":"That is the picture."}]}]}' 'BINGO_GRAPHICS=off'
keys 'look at it' Enter
# Each step keeps its row; its picture shows when that output is opened.
await '¶ read    shot.png'
await 'That is the picture.'
keys C-o
await 'esc close'
keys Enter
await '[image: image/png]'
finish

step 'BINGO_ASCII=1 and NO_COLOR leave a terminal nothing it cannot draw'
start '{"responses":[{"steps":[{"text":"Hello in ascii."}]}]}' 'BINGO_ASCII=1 NO_COLOR=1'
keys 'say hello' Enter
await 'Hello in ascii.'
if pane | grep -q '⏺'; then
  echo 'tui-smoke: a glyph outside the ascii table survived BINGO_ASCII=1' >&2
  pane >&2
  exit 1
fi
finish

for size in '80 24' '120 40' '200 50'; do
  read -r width height <<<"$size"
  tmux -L "$SOCKET" resize-window -t "$SESSION" -x "$width" -y "$height"
  step "placed board and activity at ${width}x${height} with a room open"
  start '{"responses":[
    {"steps":[{"toolCall":{"name":"HelloProgress","input":{"value":0}}}]},
    {"steps":[{"delay":{"ms":1000}},{"toolCall":{"name":"HelloProgress","input":{"value":100}}}]},
    {"steps":[{"delay":{"ms":500}},{"text":"The bar has run."}]}]}' 'BINGO_MOTION=off BINGO_FAKE_HELLO=1'
  keys '/room design' Enter
  await '#design'
  keys '/hello-board' Enter
  await '[ 1 Tick ]'
  keys Tab
  await '❯ Board'
  keys '1'
  await 'running'
  # The removed shortcut neither opens a sheet nor takes focus from the row.
  keys C-p
  keys '2'
  await 'pending'
  await '❯ Board'
  keys Tab
  keys 'run the bar' Enter
  await '0 %'
  await '100 %'
  await 'The bar has run.'
  finish
  step "${width}x${height}: board Tick/Reset and Activity ok; ctrl+p unbound"
done

mkdir -p "$WORK/home/.bingo/plugins/counter"
cp "$ROOT/crates/bingo/tests/fixtures/mods/counter/"{plugin.json,main.ts} "$WORK/home/.bingo/plugins/counter/"
for width in 80 200; do
  tmux -L "$SOCKET" resize-window -t "$SESSION" -x "$width" -y 24
  step "pulled counter at ${width}x24"
  start '{"responses":[{"steps":[{"text":"Ready."}]}]}' 'BINGO_MOTION=off'
  keys '/counter' Enter
  await 'Count: 0'
  keys 'i'
  await 'Count: 1'
  keys Tab
  step "${width}x24: editing Counter Input"
  keys 'Ada'
  await 'Name: Ada'
  keys Escape
  vanish 'Count: 1'
  step "${width}x24: reopening Counter to verify saved Input"
  keys '/counter' Enter
  await 'Count: 1'
  await 'Name: Ada'
  keys Escape
  vanish 'Count: 1'
  finish
  step "${width}x24: Counter 0→1; Input Ada persisted; Escape closed"
done

step 'M141 tool-call, band and pane examples'
for example in tool-call band pane; do
  mkdir -p "$WORK/$example/types"
  cp "$ROOT/examples/plugins/$example/"{plugin.json,register.ts,README.md} "$WORK/$example/"
  cp "$ROOT/examples/plugins/$example/types/index.d.ts" "$WORK/$example/types/"
done
printf 'original' >"$WORK/cwd/protected.txt"
start '{"responses":[
  {"steps":[{"toolCall":{"name":"Edit","input":{"file_path":"protected.txt","old_string":"original","new_string":"changed"}}}]},
  {"steps":[{"toolCall":{"name":"Bash","input":{"command":"echo before"}}}]},
  {"steps":[{"toolCall":{"name":"Read","input":{"file_path":"protected.txt"}}}]},
  {"steps":[{"text":"All done."}]}]}' 'BINGO_MOTION=off' "--plugin-dir $WORK/tool-call --plugin-dir $WORK/band --plugin-dir $WORK/pane --allowed-tools 'Edit,Bash(echo:*),Read'"
await 'A small band above your prompt'
keys M-h
vanish 'Hide band'
keys '/ledger' Enter
await 'No calls yet'
keys Escape
vanish 'Tool ledger'
keys 'Run the example calls' Enter
await 'All done.'
await 'Example calls: 2'
[ "$(cat "$WORK/cwd/protected.txt")" = original ]
keys '/ledger' Enter
await 'Tool ledger'
await 'Bash ·'
await 'Read ·'
keys Escape
vanish 'Tool ledger'
finish
step 'M141: band hidden; Edit denied; Bash rewritten; two pane rows; Escape closed'

# Every sealed fixture runs in this harness's socket and disposable home.
sealed_card() {
  local fixture="$1" width="$2" height="$3" answer="$4"
  tmux -L "$SOCKET" resize-window -t "$SESSION" -x "$width" -y "$height"
  rm -f "$WORK/cwd/sealed.txt"
  local installed="$WORK/$fixture"
  mkdir -p "$installed"
  cp "$ROOT/crates/bingo/tests/fixtures/mods/$fixture/"{plugin.json,main.ts} "$installed/"
  step "M160: $fixture at ${width}x${height}, answer $answer"
  start '{"responses":[
    {"steps":[{"toolCall":{"name":"Write","input":{"file_path":"sealed.txt","content":"sealed input"}}}]},
    {"steps":[{"text":"Sealed done."}]}]}' 'BINGO_MOTION=off' "--plugin-dir $installed"
  keys 'write sealed input' Enter
  # The dock draws a permission itself; a sealed site reshapes nothing (M197).
  await '↑↓ choose'
  await '▎ Yes'
  if pane | grep -qF 'SEALED FRAME'; then
    echo "tui-smoke: $fixture drew over the dock's permission" >&2
    pane >&2
    exit 1
  fi
  guarded
  if [ "$answer" = Escape ]; then
    keys Escape
    await 'said no'
    await 'stopped'
  else
    keys Enter
    await 'Sealed done.'
  fi
  # No Permission site is pulled, so no fixture's `ui.render` note appears.
  if pane | grep -qF "$fixture (ui.render)"; then
    echo "tui-smoke: $fixture rendered a permission the dock owns" >&2
    exit 1
  fi
  if [ "$answer" = y ]; then
    [ "$(cat "$WORK/cwd/sealed.txt")" = 'sealed input' ]
  else
    [ ! -f "$WORK/cwd/sealed.txt" ]
  fi
  finish
}
for fixture in sealed-frame sealed-rewrite sealed-omit sealed-clip sealed-duplicate sealed-hide sealed-zero; do
  sealed_card "$fixture" 80 24 y
done
sealed_card sealed-frame 80 24 Escape
sealed_card sealed-frame 200 50 y
sealed_card sealed-frame 200 50 Escape


step 'M169: changing a command checks the replacement again'
start '{"responses":[
  {"steps":[{"toolCall":{"name":"Bash","input":{"command":"echo before"}}}]},
  {"steps":[{"text":"Changed done."}]}]}' 'BINGO_MOTION=off'
keys 'run the command' Enter
await 'Run this command?'
guarded
keys Down Down Down
await '▎ Change it first'
keys Enter
await '⏎ send'
keys C-u
keys 'echo after' Enter
# The kernel gates the changed command again, so it asks once more.
await '$ echo after'
await '▎ Yes'
guarded
keys Enter
await 'Changed done.'
await 'changed by you'
pane
finish

step 'M197: the built-in question picks with the arrows'
start '{"responses":[
  {"steps":[{"toolCall":{"name":"AskUserQuestion","input":{"questions":[{"question":"Which route?","header":"Route","options":[{"label":"Alpha"},{"label":"Beta"}]}]}}}]},
  {"steps":[{"text":"Question done."}]}]}' 'BINGO_MOTION=off'
keys 'pick a route' Enter
await 'Which route?'
await 'Something else'
guarded
keys Down
await '▎ Beta'
keys Enter
await 'Question done.'
await 'Beta'
if ! pane | grep -qE '^[[:space:]]+\? ask[[:space:]]+Beta[[:space:]]*$'; then
  echo 'tui-smoke: the question step must end in the answer alone' >&2
  pane >&2
  exit 1
fi
if pane | grep -qF '⏺ Form'; then
  echo 'tui-smoke: question answer has a second Form receipt' >&2
  exit 1
fi
pane
finish

step 'M197: Escape on a question stops the turn'
start '{"responses":[
  {"steps":[{"toolCall":{"name":"AskUserQuestion","input":{"questions":[{"question":"Which route?","header":"Route","options":[{"label":"Alpha"},{"label":"Beta"}]}]}}}]},
  {"steps":[{"text":"Skipped question; continued."}]}]}' 'BINGO_MOTION=off'
keys 'choose for me' Enter
await 'Which route?'
await 'Something else'
guarded
keys -l "$(printf '\033[27u')"
await 'stopped'
if pane | grep -qF 'Skipped question; continued.'; then
  echo 'tui-smoke: escaping the question let its turn go on' >&2
  exit 1
fi
finish

echo 'tui-smoke ok'
