#!/usr/bin/env bash
# Architecture and file-size discipline. Exit non-zero on any violation.
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
say() { printf '%s\n' "$*"; }

# 1. Dependency direction (ADR-0001), from cargo metadata.
python3 - <<'PY' || fail=1
import json, subprocess, sys
meta = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--no-deps"]))
members = {p["name"]: p for p in meta["packages"]}
def deps(name):
    return {d["name"] for d in members[name]["dependencies"] if d.get("kind") in (None, "normal")}
ws = set(members)
plugins = {n for n in ws if n not in ("bingo", "bingo-sdk", "bingo-core")}
bad = []
for n in plugins:
    for d in deps(n) & ws:
        if d == "bingo-core":
            bad.append(f"{n} -> bingo-core (plugins depend on bingo-sdk only)")
        elif d in plugins:
            bad.append(f"{n} -> {d} (plugin -> plugin; use a service trait via the sdk)")
for d in deps("bingo-core") & ws:
    if d != "bingo-sdk":
        bad.append(f"bingo-core -> {d} (the kernel never imports a plugin)")
for n in ws:
    if n != "bingo-surface-tui":
        for d in deps(n):
            if d in ("ratatui", "crossterm"):
                bad.append(f"{n} -> {d} (only bingo-surface-tui may depend on the terminal stack)")
if bad:
    print("dependency rule violations:")
    for b in bad: print("  " + b)
    sys.exit(1)
print("dependency direction ok")
PY

# 2. Kernel and sdk purity: no heavy crates anywhere in their resolved normal tree.
for crate in bingo-sdk bingo-core; do
  if cargo tree -p "$crate" -e normal --prefix none 2>/dev/null | awk '{print $1}' | sort -u | grep -Ex 'reqwest|rmcp|ratatui|crossterm|image|syntect' ; then
    say "$crate resolves a forbidden crate (see above)"; fail=1
  fi
done

# 3. File size: non-test lines per .rs file (warn 700, fail 1000).
while IFS= read -r f; do
  n=$(awk '/^#\[cfg\(test\)\]/{exit} {c++} END{print c+0}' "$f")
  if [ "$n" -gt 1000 ]; then say "FAIL $f: $n non-test lines (>1000)"; fail=1
  elif [ "$n" -gt 700 ]; then say "warn $f: $n non-test lines (>700)"; fi
done < <(find crates -name '*.rs' -not -path '*/target/*')

# 4. Feature nouns must not appear in the kernel or sdk (identifiers only, case-insensitive).
nouns=$(grep -rEinw 'room|team|hire|experience' crates/bingo-sdk/src crates/bingo-core/src --include='*.rs' 2>/dev/null | grep -vE '^[^:]+:[0-9]+:\s*//' || true)
if [ -n "$nouns" ]; then
  say "feature noun found in kernel/sdk code:"; say "$nouns" | head; fail=1
fi

# 5. Struct field count ≤ 16 and inherent impl spread ≤ 2 files per type (best effort, grep-based).
python3 - <<'PY' || fail=1
import re, sys, pathlib
bad = []
impls = {}
for f in pathlib.Path("crates").rglob("*.rs"):
    src = f.read_text(encoding="utf-8")
    src = src.split("#[cfg(test)]")[0]
    for m in re.finditer(r"pub(?:\([^)]*\))?\s+struct\s+(\w+)[^{;]*\{([^}]*)\}", src, re.S):
        fields = [l for l in m.group(2).splitlines() if re.match(r"\s*(pub(?:\([^)]*\))?\s+)?\w+\s*:", l)]
        if len(fields) > 16:
            bad.append(f"{f}: struct {m.group(1)} has {len(fields)} fields (>16)")
    for m in re.finditer(r"^\s*impl(?:<[^>]*>)?\s+(\w+)(?:<[^>]*>)?\s*\{", src, re.M):
        impls.setdefault((f.parts[1], m.group(1)), set()).add(str(f))
for (crate, ty), files in impls.items():
    if len(files) > 2:
        bad.append(f"{crate}: inherent impl of {ty} spread over {len(files)} files: {sorted(files)}")
if bad:
    print("cohesion violations:")
    for b in bad: print("  " + b)
    sys.exit(1)
print("cohesion ok")
PY

[ "$fail" -eq 0 ] && say "discipline ok" || { say "discipline FAILED"; exit 1; }
