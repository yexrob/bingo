# ADR-0043 — The binary that replaces itself

Status: accepted · 2026-09-04 · Plan: M63

## Context

bingo had no way to learn that a newer release exists and no way to
become one. Everything needed is already published: `release.yml`
builds four archives named `bingo-<target>` — `tar.gz`, and a zip on
Windows — with `checksums.txt` beside them, and cuts a GitHub Release
per `v*` tag, a *pre*-release when the tag is not on `main`, so
`releases/latest` is exactly the question "is there a newer one". What
was asked for: the welcome box says it, `bingo update` installs it. No
kernel door is opened — a surface's own row, and a command answered
before a host exists.

## Decision

1. **A library crate, `bingo-update`** (library tier, ADR-0012 §1): the
   version comparison, the release answer, the archive name per target,
   `checksums.txt`, the daily stamp, and the install half. Budget
   332 → 333 for the member and nothing else.
2. **It reaches no network.** `check` is handed the fetch and the
   command does its own downloading. `reqwest` pulls `aws-lc-sys`, whose
   build script wants `windows.h` (ADR-0041's 2026-09-04 note), and this
   is the one crate whose Windows arms — the rename dance and `tar.exe`
   — most need a compiler on any machine. `bingo-loopback` refused
   `reqwest` for the same reason (ADR-0042).
3. **Its own SHA-256, and its own three-number comparison.** `sha2` is
   in `Cargo.lock` only as a dev edge of the pty harness and `semver`
   only as a build edge of `rustc_version`, so neither is in the normal
   tree and each would have cost a crate; `aws-lc-rs` has SHA-256 and
   would have cost §2. Sixty lines of FIPS 180-4 against the standard's
   own vectors, and a comparison of the `X.Y.Z` a tag is, buy both back.
4. **Once a day, and only where there is a box to say it in.**
   `<data_dir>/update.json` holds `{checked_at, latest}` and is written
   *before* the request, so a machine whose answer never comes still
   waits a day: the API allows sixty unauthenticated requests an hour
   per address. `update.check` is claimed by the TUI, whose box says it,
   and read by the bin out of the layers as `demoUi` is. No check under
   `--print`, RPC, ACP or in a sub-session. `BINGO_UPDATE_API` replaces
   the origin, which is how the black-box test serves a release.
5. **The system `tar`, and two renames.** Windows has carried `tar.exe`
   (bsdtar, which reads zip) since 10 1803, so no archive crate is
   bought. `current_exe()` is canonicalized — a Homebrew or cargo shim
   points at the binary, and it is the binary that is replaced. The
   running file is renamed aside, the new one renamed in, and the aside
   removed: Windows lets a running image be renamed but not
   overwritten, and what it still holds open is swept by the next
   start. Nothing is ever run with elevated rights; a directory this
   process may not write is exit 1 naming `cargo install --git … --tag`.

## Consequences

- A check that fails says nothing and costs the start nothing: every
  failure is `None` and a debug line, off the loop's own thread.
- The welcome box carries the row; where it has scrolled away the
  status line says it once. Updating from inside the TUI is not
  offered — the binary being replaced is the one that is running.
- The zip half of the install is Windows' alone and is compiled, not
  run, on a unix machine: CI's `windows` job and a hands-on run are the
  backstop, as they are for macOS Gatekeeper on an unsigned release.
- `scripts/budget.toml`'s comment cites this record.

Refs: ADR-0001 (crate map), ADR-0012 §1 (library tier), ADR-0041
(the aws-lc-sys note), ADR-0042 §2 (a library may depend on a library).
