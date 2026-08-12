#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

MAX_LINES="${MAX_LINES:-4000}"
FAIL=0

while IFS= read -r -d '' file; do
  lines=$(wc -l < "$file")
  if [ "$lines" -gt "$MAX_LINES" ]; then
    echo "::error file=$file::${lines} lines exceeds the ${MAX_LINES}-line cap; split the file"
    FAIL=1
  fi
done < <(find src tests -name '*.rs' -print0)

if ! cargo clippy --locked --bins -- \
  -D clippy::unwrap_used \
  -D clippy::expect_used; then
  FAIL=1
fi

if [ "$FAIL" -ne 0 ]; then
  echo "discipline gate failed" >&2
  exit 1
fi
echo "discipline gate ok (line cap ${MAX_LINES}; no unwrap/expect outside tests)"
