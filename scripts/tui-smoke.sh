#!/usr/bin/env bash
# The terminal surface through a real pty. tmux gives us the terminal a unit
# test cannot: a reply arrives, Esc interrupts a running turn, a permission
# dialog answered `y` really writes the file, and ctrl+c twice hands the
# terminal back with status 0.
#
# Every wait is a bounded poll of `capture-pane`, never a fixed sleep: the
# script is as fast as the binary and fails loudly instead of flaking.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug/bingo"
SOCKET=bingo
SESSION=smoke
# 200 polls of 50 ms: ten seconds is far more than any step needs.
TRIES=200

command -v tmux >/dev/null || { echo "tui-smoke: tmux is required"; exit 1; }
[ -x "$BIN" ] || { echo "tui-smoke: build first (cargo build): $BIN"; exit 1; }

WORK=$(mktemp -d)
cleanup() {
  tmux -L "$SOCKET" kill-server 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT
mkdir -p "$WORK/home" "$WORK/cwd"

pane() { tmux -L "$SOCKET" capture-pane -p -t "$SESSION"; }
keys() { tmux -L "$SOCKET" send-keys -t "$SESSION" "$@"; }

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
  local text i
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

# Start bingo on a scripted provider. The script's own text never appears in
# the command line, so `await` matches the output and not the echo of itself.
# `$2` is any extra environment the step wants, `$3` any extra flags.
start() {
  printf '%s' "$1" >"$WORK/script.json"
  keys "HOME=$WORK/home ${2:-} BINGO_FAKE_SCRIPT=$WORK/script.json $BIN --cwd $WORK/cwd ${3:-}" Enter
  await '? for shortcuts'
}

# Press `$1` until `$2` shows. A dialog ignores keys until its guard has
# passed (the kernel's, not ours), so the first press may land on nothing.
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
finish() {
  # A retry loop may have left a stray key in the composer.
  keys C-u
  keys C-c
  await 'press ctrl+c again to exit'
  keys C-c
  keys 'echo "left with: $?"' Enter
  await 'left with: 0'
}

tmux -L "$SOCKET" kill-server 2>/dev/null || true
tmux -L "$SOCKET" -f /dev/null new-session -d -s "$SESSION" -x 120 -y 40 \
  "PS1='smoke\$ ' /bin/sh -i"
keys 'echo "shell: $((1 + 1))"' Enter
await 'shell: 2'

step 'a reply reaches the transcript'
start '{"responses":[{"steps":[{"text":"Hello from the smoke test."}]}]}'
keys 'say hello' Enter
await 'Hello from the smoke test.'
finish

step 'esc interrupts a turn that is still waiting'
start '{"responses":[{"steps":[{"delay":{"ms":60000}},{"text":"too late"}]}]}'
keys 'wait for it' Enter
await 'esc to interrupt'
keys Escape
await '[Request interrupted by user]'
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

step 'ctrl+f searches the transcript and esc gives the status line back'
start "$(long_reply)"
keys 'say a lot' Enter
await ' line 80'
keys C-f
keys 'line 42'
await '/line 42'
keys Enter
await '1/1 · n/N · esc'
keys Escape
await '? for shortcuts'
finish

step 'a focused block opens whole in the pager and gives the frame back'
start "$(long_reply)"
keys 'say a lot' Enter
await ' line 80'
# A click takes the block; the frame it is answered against is the last one
# drawn, so the drive waits for it before pressing anything.
mouse 0 10 5
sleep 0.5
keys Enter
await 'j/k · pgup/pgdn'
keys 'G'
await ' line 80'
keys '/'
keys 'line 42'
await '/line 42'
keys Enter
await 'n/N · esc'
keys Escape
keys Escape
vanish 'j/k · pgup/pgdn'
await '? for shortcuts'
finish

step 'an @ offers the paths under the session and enter takes one'
printf '[package]\nname = "smoke"\n' >"$WORK/cwd/Cargo.toml"
start '{"responses":[{"steps":[{"text":"nothing to do"}]}]}'
keys '@Car'
await '@Cargo.toml'
keys Enter
await '> @Cargo.toml'
finish

step 'the help sheet opens on ? and closes on esc'
start '{"responses":[{"steps":[{"text":"nothing to do"}]}]}'
keys '?'
await 'shift+tab'
keys Escape
await '? for shortcuts'
vanish 'shift+tab'
finish

step 'a permission dialog answered y runs the tool'
start '{"responses":[
  {"steps":[{"toolCall":{"name":"Write","input":{"file_path":"note.txt","content":"written by the smoke test\n"}}}]},
  {"steps":[{"text":"Wrote it."}]}]}'
keys 'write the note' Enter
await 'Do you want to '
# The dialog is a card: a bordered box, not rows in the transcript.
await '│'
press_until 'y' 'Wrote it.'
[ -f "$WORK/cwd/note.txt" ] || { echo "tui-smoke: the approved Write wrote nothing" >&2; exit 1; }
grep -q 'written by the smoke test' "$WORK/cwd/note.txt"
finish

step 'a notice holds the status line and then leaves it'
start '{"responses":[{"steps":[{"text":"nothing to do"}]}]}'
keys C-c
await 'press ctrl+c again to exit'
# It holds its window and goes on its own: nothing is pressed to dismiss it.
vanish 'press ctrl+c again to exit'
await '? for shortcuts'
finish

step 'a question on a window nobody watches notifies and nothing else'
start '{"responses":[
  {"steps":[{"toolCall":{"name":"Write","input":{"file_path":"away.txt","content":"written while away\n"}}}]},
  {"steps":[{"text":"Wrote it."}]}]}'
# The window loses the focus, so the question that follows goes to the desktop
# as well as to the screen — and not one byte of it may land on the pane.
focus O
keys 'write the note' Enter
await 'Do you want to '
press_until 'y' 'Wrote it.'
[ -f "$WORK/cwd/away.txt" ] || { echo "tui-smoke: the approved Write wrote nothing" >&2; exit 1; }
if pane | grep -qE 'notify|777|Ptmux'; then
  echo 'tui-smoke: a notification was printed onto the screen' >&2
  pane >&2
  exit 1
fi
focus I
finish

step 'BINGO_ASCII=1 and NO_COLOR leave a terminal nothing it cannot draw'
start '{"responses":[{"steps":[{"text":"Hello in ascii."}]}]}' 'BINGO_ASCII=1 NO_COLOR=1'
keys 'say hello' Enter
await '* Hello in ascii.'
if pane | grep -q '⏺'; then
  echo 'tui-smoke: a glyph outside the ascii table survived BINGO_ASCII=1' >&2
  pane >&2
  exit 1
fi
finish

step 'a live signal moves in the rail and leaves nothing behind'
start '{"responses":[
  {"steps":[{"toolCall":{"name":"DemoProgress","input":{"label":"cargo test"}}}]},
  {"steps":[{"text":"The bar has run."}]}]}' '' '--demo-ui'
keys 'run the bar' Enter
# The pane is 120 columns, so the card is in the rail; the bar is published
# every 200 ms for three seconds, and these are its two ends.
await '0 %'
await '100 %'
await 'The bar has run.'
finish

step 'a button on a pinned board fires its command and the table changes'
start '{"responses":[{"steps":[{"text":"nothing to do"}]}]}' '' '--demo-ui'
keys '/board' Enter
await 'board published'
# ctrl+t opens the panel sheet, ⏎ pins the board into the rail, esc closes it.
keys C-t
await 'bingo.demo.ui'
keys Enter
await 'pinned'
keys Escape
await 'Board'
# tab takes the focus, and the key the board's first button carries fires it.
keys Tab
await '❯ Board'
keys '1'
keys C-t
await 'running'
finish

echo 'tui-smoke ok'
