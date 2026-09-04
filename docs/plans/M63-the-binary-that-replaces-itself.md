# M63 — The binary that replaces itself

## Goal

User, 2026-09-04: bingo has no way to learn that a newer release
exists and no way to become it. Wanted: **at start, asynchronously,
find out whether a newer release is out and say so in the welcome
box; and `bingo update` to install it.** Nothing else changes about
starting: a check that fails, or a box with no network, is a run that
says nothing and starts exactly as fast.

The release line already exists: `release.yml` builds
`bingo-<target>.tar.gz` (Linux, macOS ×2) and `bingo-<target>.zip`
(Windows) plus `checksums.txt` (sha256) for every `v*` tag, and
publishes a GitHub Release — a *pre*-release when the tag is not on
`main`. `/repos/yexrob/bingo/releases/latest` returns the newest
non-prerelease, which is the right answer for "is there a newer one".

## Bricks

1. **A library crate `bingo-update`** (library tier, like
   `bingo-pictures`; ADR line in a new short ADR-0043). Pure bricks,
   each tested on fixtures:
   - `version::newer(current: &str, latest: &str) -> bool` on semver
     (the `semver` crate is already in the tree — a transitive of
     something the workspace builds; check `cargo tree -i semver`
     and use it directly, budget unchanged apart from the member).
   - `release::latest(json: &str) -> Result<Release{version, assets:
     Vec<Asset{name, url}>}>` from the API's JSON (a fixture file
     copied from a real response of the repo's v0.4.2 release).
   - `asset::name(target: &str) -> String` — the archive name for
     this build's target triple (`env!("TARGET")`-equivalent: the
     workspace's `build.rs` pattern, or `std::env::consts::{ARCH,
     OS}` mapped once to the four names `release.yml` uses; the test
     pins all four).
   - `checksums::expected(text: &str, name: &str) -> Option<[u8;32]>`
     from `checksums.txt`; `sha2` is likewise already in the tree.
   - `stamp`: `<data_dir>/update.json` `{checked_at, latest}` — one
     file, the fact "when we last asked and what we heard". `due(
     stamp, now, EVERY)` says whether to ask again; `EVERY` = 24 h
     as a named constant.
2. **The check.** `check(client, current, data_dir, now) -> Option<
   String>`: returns the newer version if the stamp is fresh and
   says so, or asks the API (5 s timeout, `User-Agent: bingo/<ver>`)
   when due and writes the stamp. Never errors out: any failure is
   `None` and a debug trace. `update.check = false` in
   `~/.bingo/settings.json` turns it off (one setting, read where the
   surface reads its others). No check under `--print`, RPC, ACP or
   in a sub-session — only a run that has a welcome box to say it in.
3. **The welcome box says it.** The TUI spawns the check at start on
   its reply channel (`Reply::Update(String)` beside `Reply::Linked`);
   the reply lands in `Ui` as terminal-side state (`ui.update:
   Option<String>`), and `welcome::lines` draws one more row under
   the help line, dim with the spark's colour on the version:
   `  ↑ v0.5.0 is out · bingo update`. A frame, like any reply. If
   the welcome box has already scrolled off, a status notice (`Level::
   Info`) says the same once. Snapshot both.
4. **`bingo update`.** A `clap` subcommand: prints the current and
   latest version; if newer, downloads this target's asset and
   `checksums.txt` to a temp dir under `data_dir`, verifies sha256,
   unpacks with the system `tar` (`tar -xzf`; on Windows `tar.exe` is
   in the box since 10 1803 and reads zip — no archive crate), and
   replaces `std::env::current_exe()` atomically: rename the running
   binary to `bingo.old` beside it, rename the new one in, remove
   `.old` (Windows lets a running exe be renamed but not overwritten
   — this is the whole reason for the two renames; remove `.old` on
   the next start if it could not be removed now). `--check` only
   reports. A binary installed by `cargo install` is the same file
   at `~/.cargo/bin/bingo`, so the same path. Exit codes: 0 updated
   or already newest, 1 could not (network, checksum, permission —
   say which, and say `cargo install --git … --tag vX` as the way
   round on a permission failure). Nothing is run with elevated
   rights, ever.
5. **Records.** ADR-0043 (≤60 lines: why a crate, why the system tar,
   why the stamp, why no self_update crate), `budget.sh` max +1 for
   the member, `docs/design/tui.md` §5 welcome entry + dated line,
   the README/site line if one exists for installing.

## Files

`crates/bingo-update/` (new), `Cargo.toml` + `Cargo.lock` (member),
`crates/bingo/src/main.rs` (+ `update.rs`), `bingo-surface-tui/src/{
welcome.rs,run.rs,ui.rs}` + snapshots, `bingo-core/src/settings.rs`
fixture (`update.check`), `docs/adr/0043-*.md`, `scripts/budget.sh`
config, `docs/design/tui.md`.

## Exit criteria

- [ ] `version`, `release`, `asset`, `checksums`, `stamp` bricks each
      have fixture tests; the four asset names match `release.yml`.
- [ ] A stamp younger than 24 h is not asked again (counting fetch
      seam); a failed fetch leaves the stamp as it was.
- [ ] Welcome box snapshot with the row, and without it; the notice
      path tested.
- [ ] `bingo update --check` black-box: exit 0, stdout is the two
      versions, nothing else; with a fake API on loopback (the pty
      tests already start local servers) the full update replaces a
      copy of the binary and the copy prints the new version.
- [ ] All gates; `cargo check -p bingo-update -p bingo --all-targets
      --target x86_64-pc-windows-msvc` (rename dance, `tar.exe`).
- [ ] Hands-on: appended by the parent.

## Non-goals

Updating from the TUI itself (a `/update` command — the binary that
is running is the one being replaced; a restart is the person's);
delta updates; signature verification beyond the checksum the
release line already writes; a changelog in the box.

## Risks

The GitHub API rate limit is 60/h unauthenticated per IP — once a day
per machine is nothing, but a stamp that fails to write would ask
every start: write the stamp *before* the request with `checked_at`
so a failure still waits a day. `current_exe()` behind a symlink
(Homebrew, cargo's own): replace the target, not the link — resolve
with `canonicalize` and say so in the ADR. macOS quarantine: a
downloaded binary may be blocked by Gatekeeper on first run if the
release is unsigned — try it hands-on and record what happens; if it
is blocked, `xattr -d com.apple.quarantine` in the update path is the
fix, done by us, not asked of the person.
