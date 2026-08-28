#!/usr/bin/env bash
# Build-time budget: dependency count, kernel check time, relink isolation, target size.
set -euo pipefail
cd "$(dirname "$0")/.."
cfg() { grep -E "^$1\s*=" scripts/budget.toml | sed -E 's/.*=\s*//'; }
MAX_DEPS=$(cfg max_dependencies); MAX_CORE=$(cfg max_core_check_seconds); MAX_TARGET=$(cfg max_target_gb)
fail=0

deps=$(cargo tree --workspace -e normal --prefix none 2>/dev/null | awk '{print $1}' | sort -u | wc -l | tr -d ' ')
printf 'dependencies (unique, normal): %s (max %s)\n' "$deps" "$MAX_DEPS"
[ "$deps" -le "$MAX_DEPS" ] || { echo "FAIL: too many dependencies"; fail=1; }

cargo check -p bingo-core --quiet 2>/dev/null || true
start=$(date +%s); cargo check -p bingo-core --quiet; end=$(date +%s)
printf 'warm cargo check -p bingo-core: %ss (max %ss)\n' "$((end-start))" "$MAX_CORE"
[ $((end-start)) -le "$MAX_CORE" ] || { echo "FAIL: kernel check too slow"; fail=1; }

if [ -f crates/bingo-surface-tui/src/lib.rs ]; then
  touch crates/bingo-surface-tui/src/lib.rs
  out=$(cargo check -p bingo-core 2>&1 | grep -c 'Compiling' || true)
  printf 'relink isolation: touching the TUI recompiled %s crates for core (must be 0)\n' "$out"
  [ "$out" -eq 0 ] || { echo "FAIL: core is not isolated from the TUI"; fail=1; }
fi

if [ -d target/debug ]; then
  kb=$(du -sk target/debug | awk '{print $1}'); gb=$((kb / 1024 / 1024))
  printf 'target/debug: %s GB (soft max %s)\n' "$gb" "$MAX_TARGET"
  [ "$gb" -le "$MAX_TARGET" ] || echo "warn: target/debug exceeds the soft limit"
fi

printf 'test binaries: %s\n' "$(find target/debug/deps -maxdepth 1 -type f -perm -u+x 2>/dev/null | grep -Ev '\.(d|rlib|rmeta|so|dylib)$' | wc -l | tr -d ' ')"
[ "$fail" -eq 0 ] && echo "budget ok" || { echo "budget FAILED"; exit 1; }
