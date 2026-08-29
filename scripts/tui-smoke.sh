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

# Start bingo on a scripted provider. The script's own text never appears in
# the command line, so `await` matches the output and not the echo of itself.
start() {
  printf '%s' "$1" >"$WORK/script.json"
  keys "HOME=$WORK/home BINGO_FAKE_SCRIPT=$WORK/script.json $BIN --cwd $WORK/cwd" Enter
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

step 'a permission dialog answered y runs the tool'
start '{"responses":[
  {"steps":[{"toolCall":{"name":"Write","input":{"file_path":"note.txt","content":"written by the smoke test\n"}}}]},
  {"steps":[{"text":"Wrote it."}]}]}'
keys 'write the note' Enter
await 'Permission '
press_until 'y' 'Wrote it.'
[ -f "$WORK/cwd/note.txt" ] || { echo "tui-smoke: the approved Write wrote nothing" >&2; exit 1; }
grep -q 'written by the smoke test' "$WORK/cwd/note.txt"
finish

echo 'tui-smoke ok'
