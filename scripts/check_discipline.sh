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

# 4b. A surface is a client of the one event stream (ADR-0002, ADR-0007): no private mirror enums.
mirrors=$(grep -rEn '^\s*(pub(\([a-z]+\))?\s+)?enum\s+[A-Za-z]*Event\b' crates/bingo-surface-*/src --include='*.rs' 2>/dev/null || true)
if [ -n "$mirrors" ]; then
  say "event mirror enum in a surface crate (surfaces fold bingo_sdk::Event, they do not redefine it):"; say "$mirrors" | head; fail=1
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

# 6. Function length: physical lines from `fn` to its closing brace (warn 60, fail 120).
python3 - <<'PY' || fail=1
import re, sys, pathlib
WARN, FAIL = 60, 120
bad = 0
def bodies(src):
    # Yield (name, start_line, end_line) for every fn with a block body.
    lines = src.splitlines()
    i = 0
    while i < len(lines):
        m = re.match(r"\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+|const\s+|unsafe\s+)*fn\s+(\w+)", lines[i])
        if not m:
            i += 1; continue
        depth, j, opened = 0, i, False
        while j < len(lines):
            code = re.sub(r'"(?:\\.|[^"\\])*"', '""', lines[j]).split("//")[0]
            if not opened and ";" in code and "{" not in code:
                break  # a trait signature, no body
            for ch in code:
                if ch == "{": depth += 1; opened = True
                elif ch == "}": depth -= 1
            if opened and depth == 0:
                yield m.group(1), i + 1, j + 1
                break
            j += 1
        i = j + 1 if opened else i + 1
for f in sorted(pathlib.Path("crates").rglob("*.rs")):
    if f.name == "tests.rs" or "tests" in f.parts or "test_support" in f.name:
        continue  # a test may be as long as its scenario
    src = f.read_text(encoding="utf-8").split("#[cfg(test)]")[0]
    for name, start, end in bodies(src):
        n = end - start + 1
        if n > FAIL:
            print(f"FAIL {f}:{start} fn {name} is {n} lines (>{FAIL})"); bad += 1
        elif n > WARN:
            print(f"warn {f}:{start} fn {name} is {n} lines (>{WARN})")
sys.exit(1 if bad else 0)
PY

[ "$fail" -eq 0 ] && say "discipline ok" || { say "discipline FAILED"; exit 1; }
