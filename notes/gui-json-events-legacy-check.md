# Legacy CLI compatibility check — feat/gui-json-events

Verified 2026-08-10 against the main-checkout binary (0c4d431) and the worktree
binary (d1e23ae), both debug builds:

- `--version`: byte-identical (`bingo 0.3.3`).
- Non-TTY error path (`--no-such-flag`): stdout/stderr/exit identical.
- `--help`: sorted flag-set diff shows ONLY additive flags
  (`--json-events`, `--probe`, `--session`); all existing flags present.
- Full unit suite: 851 passed; protocol black-box suite: 12 passed.
- `--print` streaming byte-equivalence (protocol invariant 4) is verified in
  M1 e2e (AC-F2-2) against the same binary.
