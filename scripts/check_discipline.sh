#!/usr/bin/env bash
# Engineering discipline gate (#8):
#   1. no .rs source file may exceed MAX_LINES lines (test code included —
#      oversized test blocks must move out of the file too)
#   2. non-test code must not call .unwrap()/.expect(), except lines marked
#      `// unreachable` (static regexes, compile-time-constant versions)
set -euo pipefail

cd "$(dirname "$0")/.."

MAX_LINES="${MAX_LINES:-4000}"
FAIL=0

# --- 1. per-file line cap ----------------------------------------------------
over_lines() {
  local file="$1" lines="$2"
  echo "::error file=$file::${lines} lines exceeds the ${MAX_LINES}-line cap; split the file"
  FAIL=1
}
while IFS= read -r -d '' file; do
  lines=$(wc -l < "$file")
  if [ "$lines" -gt "$MAX_LINES" ]; then
    over_lines "$file" "$lines"
  fi
done < <(find src tests -name '*.rs' -print0)

# --- 2. no unwrap/expect in non-test code -----------------------------------
check_no_panics() {
  local file="$1"
  awk '
    /^#\[cfg\(test\)\]/  { in_test = 1 }
    /^mod tests/        { in_test = 1 }
    in_test && /^}/     { in_test = 0 }
    !in_test && /\.unwrap\(\)|\.expect\(/ && $0 !~ /\/\/ unreachable/ {
      print FILENAME ":" FNR ": " $0
    }
  ' "$file"
}
while IFS= read -r -d '' file; do
  case "$file" in
    *_tests*.rs) continue ;; # #[path]-included test modules carry no `mod tests` marker
  esac
  hits=$(check_no_panics "$file")
  if [ -n "$hits" ]; then
    echo "$hits" >&2
    FAIL=1
  fi
done < <(find src -name '*.rs' -print0)

if [ "$FAIL" -ne 0 ]; then
  echo "discipline gate failed" >&2
  exit 1
fi
echo "discipline gate ok (line cap ${MAX_LINES}; no unwrap/expect outside tests)"
