# PRD: version check + welcome-card notice + `bingo update` command

> Status: v1.2 (pm's finalized draft, 2026-08-09)
> **v1.2 revision (pm, 2026-08-09)**: main accepted all of differences 2-6 (#team-update item #10) — ① D2's output contract becomes the implemented Chinese output (human-readable first; the error paths already have the `[error] code=` contract as a floor); ② B2 failure rate-limiting dropped: failures don't write the cache, one background retry per startup (async, silent); ③ the welcome-card notice uses a cached-data-source model: this startup renders from the cache, the background pre-warm writes the cache for the next startup (the notice lags one startup; accepted); ④ error codes use the actual code table (reusing the generic codes OFFLINE/SERVER_ERROR/STORAGE_ERROR + short codes, no `UPDATE_*` prefix); ⑤ the F10 updateCheck toggle stays P1, not implemented. All differences closed.
> **v1.1 revision (pm, 2026-08-09)**: uiux's visual spec `update-banner.md` v1.1 is finalized (commit 607c353) — group C's visual anchors switch wholesale to that spec as the single source of truth (PRD group C only accepts, doesn't define): copy becomes `New version {v} available — run bingo update` (no ✦ prefix / no quotes around the command); motion scope = **the version and `bingo update` segments breathing in-phase** (C3 synced); the degradation chain gains `motion: off` / `BINGO_NO_MOTION` explicit opt-outs; a truncation chain (50/43/15) and a motion-scope assertion (any two frames of the rest of the welcome card are identical) were added; the identity line hardcode fix is kept (E3).
> Current anchors: bingo v0.2.1 (Cargo.toml); GitHub Releases `yexrob/bingo`, assets = `bingo-<target-triple>.tar.gz/.zip` × 4 platforms + `checksums.txt`, latest points at the newest tag.
> Welcome-card implementation location: `src/tui/chat.rs:4998` `welcome_rows` (note: the version line hardcodes `bingo v0.1.0`; this round should switch to the compile-time version `CARGO_PKG_VERSION`, same source as the check's comparison).
> Visual source of truth: [`update-banner.md`](./update-banner.md) v1.1 (layout/copy/motion/degradation/11 anchors — group C of this PRD only accepts, doesn't define).
> CLI structure: clap subcommand (`src/main.rs:84` `Command` enum; the existing `share` fast-path pattern can be reused).
> Network: `reqwest` (rustls) is already a dependency; no new HTTP dependency needed.

## 1. Goal and user scenarios

**One sentence**: the user learns about a new version asynchronously at bingo startup (welcome-card notice); `bingo update` downloads, verifies, and replaces in one command; offline/failure degrade silently and never block.

**User scenarios**:

1. **Discovery**: the user starts the TUI on a normal day; a restrained notice line appears on the welcome card `✦ v0.3.0 available — run 'bingo update'` (version colored, slight breathing), without affecting any operation.
2. **Active check**: `bingo update --check` prints the current version vs latest (`New version v0.3.0 found: run bingo update to install` / `bingo is already the latest version v0.2.1`, human-readable Chinese; error paths go through the `[error] code=...` contract).
3. **Update**: `bingo update` automatically does download → sha256 verification → atomic replacement → restart-to-take-effect notice.
4. **Failure/offline**: no network or GitHub unreachable → the check silently skips (with a TTL, no repeated retries); `bingo update` failure → clear error code + manual-download guidance.

**Non-goals**: no auto-install (only a notice, no update), no signature verification (v1 checksum suffices), no rollback command, no installer.

## 2. Scope boundaries (v1 explicitly does not do)

| Not doing | Reason |
|---|---|
| Auto-update push (silent background install) | the user needs awareness and control; v1 only notifies + explicit command |
| Signature verification (notarization / Authenticode / sigstore) | the release process has no signing chain yet; checksums already guard against corruption and tampering (in transit); signing is v2 |
| Update rollback command (`--rollback` / backup retention) | atomic replacement guarantees no half-updated state; failure falls back to the old version, no rollback feature needed |
| Windows installer (MSI/NSIS) | v1 only zip assets + in-place replacement (manual guidance when the exe is locked) |
| Incremental/resume downloads, multi-threaded download | single files are tens of MB; just download straight |
| `--target` manual platform-asset selection | auto-detection suffices; cross-downloads are P2 |
| Pre-release/old-channel selection | only track the `releases/latest` stable |
| Homebrew/package-manager integration | no formula; reconsider in P2 if a brew channel exists |
| Persistent dismiss for the welcome-card notice | the TTL cache already rate-limits; a dismiss state is extra persistence with unproven demand |

## 3. Feature list

| # | Feature | Description | Priority |
|---|---|---|---|
| F1 | startup async version check | after TUI startup, asynchronously request `releases/latest`, never blocking the first frame or any input | P0 |
| F2 | check-result TTL cache | `~/.local/share/bingo/update-check.json`: within a 24h success TTL no request is sent; failures don't write the cache | P0 |
| F3 | silent check failure | timeout/connection failure/parse failure → no notice, no error, no blocking; failure doesn't write the cache, one background retry per startup | P0 |
| F4 | welcome-card notice line | when a new version exists, the welcome card gains a notice line (copy/style/position per `update-banner.md` v1.1) | P0 |
| F5 | notice-line motion | the version and `bingo update` segments breathe in-phase (Claude-Code-thinking-like, driven by the existing TUI tick; visual spec in `update-banner.md` §2) | P0 |
| F6 | motion degradation | `motion: off` / `BINGO_NO_MOTION` → static rest; no truecolor → discrete two-step; `NO_COLOR` → static bold (the notice stays, never disappears) | P0 |
| F7 | `bingo update` command | download the current platform asset → checksum verification → extract → atomic replace → restart notice | P0 |
| F8 | `bingo update --check` | only check and print the result, no download or replace; usable headless | P0 |
| F9 | error-code contract | reuse the generic codes (OFFLINE/SERVER_ERROR/STORAGE_ERROR) + short codes (CHECKSUMS_UNAVAILABLE/CHECKSUM_MISMATCH/ARCHIVE_INVALID/UNSUPPORTED_PLATFORM) registered in `src/error.rs` + drift-guard tests | P0 |
| F10 | settings toggle `updateCheck` | master switch (default true) so sensitive/offline environments can disable the check. **v1.2 ruling: stays P1, not implemented** | P1 |
| F11 | built-in skill sync | guide.md command quick reference + diagnostic guide updated | P0 |

## 4. Design points

### 4.1 Check strategy

- **Timing**: after TUI startup, spawn an async task (Tokio `spawn`); the result arrives via a channel; `--print`/headless and subcommand fast paths like `share` **don't check** (scripted scenarios stay unbothered).
- **Data source**: `GET https://api.github.com/repos/yexrob/bingo/releases/latest` (must send a User-Agent), take `tag_name`. The API rate limit is 60/h unauthenticated — the 24h TTL protects it amply; the implementation may alternatively follow the 302 to `/releases/latest` and read the tag from `Location` (an API-free option).
- **Version comparison**: semver (strip the `v` prefix from the tag; parse failure = no new version, silent). `0.2.1 < 0.2.10`, `0.2.1 < 0.3.0`; pre-release tags (`-rc`/`-beta`) never mix with stable; anything that doesn't parse as valid semver is ignored.
- **Cache**: `update-check.json` = `{ checked_at: epoch_secs, latest_tag }`. At startup read the cache; within the TTL (24h) use the cached result directly (still show the notice when there's a new version, but no request); only after the TTL expires re-check asynchronously. Failures don't write the cache — one background retry per startup (async, silent; N startups = N requests, no risk under GitHub's 60/h limit for daily use).
- **Touchpoint (v1.2 ruling: cached-data-source model)**: the welcome card renders by reading the cache (`latest_cached`); the in-this-startup check result is **not inserted live** (avoids redrawing an already-rendered welcome card / touching the scrollback invariant); the background pre-warm (`spawn_background_check`) writes the cache for the **next startup** — the first startup (no cache) shows nothing, the second shows it; a one-startup lag is expected behavior.

### 4.2 Welcome-card notice

**The single visual source of truth = [`update-banner.md`](./update-banner.md) v1.1** (commit 607c353) — layout, copy, motion, degradation, and implementation approach all follow that spec; this PRD only defines acceptance and the appearance conditions. Spec essentials:

- **Position**: directly above the version identity line (`bingo vX.Y.Z · …`), below the cwd line (blank-line rhythm: one blank between cwd and the notice, adjacent to the identity line forming an "old vs new" contrast block).
- **Copy**: `New version v0.3.0 available — run bingo update` (no ✦ prefix, no quotes around the command). Three styled segments: the static segments (`New version ` / ` available — run `) `theme.inactive`; breathing segment ① the version `vX.Y.Z` in the breathing color; breathing segment ② `bingo update` in the breathing color + bold (in phase with ①).
- **Motion**: sinusoidal breathing (not hard blink / sweep / ANSI blink codes), sRGB linear interpolation between two brand-orange stops — dark `#D77757 ↔ #E8896B` (≥6.24:1 throughout), light `#B05227 ↔ #9A4A24` (≥4.72:1 throughout); period 3.0s (90 frames @30fps, reusing the existing TICK), 9s total (3 breaths) then settling at the rest color; phase function `t = 0.5 − 0.5·cos(2π·phase/90)` (phase 0 = rest, no jump). **The motion scope is exactly the two keyword segments on this one line; every other element of the welcome card participates in no animation on any frame; no entrance animation (silent insertion)**.
- **Degradation chain**: `motion: "off"` (new settings key, default auto) / `BINGO_NO_MOTION=1` → static rest (the notice stays, never disappears); no truecolor → discrete two-step (2s period, 400ms peak, no crash); `NO_COLOR`/monochrome → static bold; user input → stop early (P1).
- **Narrow-screen truncation chain** (`banner_line(v, width)` pure function): inner_w ≥50 full line / ≥43 drop the "available" clause / ≥15 keep only `bingo update` / <15 hidden; the command is visible in every tier (except <17 columns) and never overflows the card frame.
- **Ignorability**: the notice is just one line on the card — no interaction, no blocking, no focus stealing; the TTL caps it at once a day. No dismiss persistence (v1 subtracts).
- **Implementation constraints** (spec §3.2 option A): the 9s animation window ends well before the welcome card's settle-to-disk moment; within the window the card stays a live document row, and after expiry it settles at the rest color and persists naturally — never touching scrollback (the never-redraw-above-viewport invariant holds). Wiring: `Chat` holds `UpdateBanner { latest, anim_until_tick }`; `has_dynamic_rows()` stays dirty during the animation window; `update_color(theme, phase)` is a pure function directly unit-testable.

### 4.3 `bingo update` command

```
bingo update [--check]
```

| Argument | Description |
|---|---|
| `--check` | only check and print the result, no download or replace |

**Flow** (`bingo update`, without `--check`):

1. Request the latest tag; if the current version is already latest → output `bingo is already the latest version v0.2.1`, exit 0, no download request.
2. Platform-asset mapping (`std::env::consts` detection):

   | Platform | Asset |
   |---|---|
   | `aarch64-apple-darwin` (Apple Silicon) | `bingo-aarch64-apple-darwin.tar.gz` |
   | `x86_64-apple-darwin` (Intel Mac) | `bingo-x86_64-apple-darwin.tar.gz` |
   | `x86_64-pc-windows-msvc` | `bingo-x86_64-pc-windows-msvc.zip` |
   | `x86_64-unknown-linux-gnu` | `bingo-x86_64-unknown-linux-gnu.tar.gz` |
   | other | clear error `UNSUPPORTED_PLATFORM` |

3. Download the asset + `checksums.txt`; compare the sha256 against the line for the matching file name in `checksums.txt` (tolerate two-column order / `*` / `./` prefixes), **mismatch → refuse to install**, nonzero exit (`CHECKSUM_MISMATCH`). A missing `checksums.txt` or missing line also refuses (safety first, `CHECKSUMS_UNAVAILABLE`).
4. Extract (`tar` + `flate2` / `zip`), take the binary (`bingo` / `bingo.exe`).
5. **Atomic replacement**: target = `std::env::current_exe()`. Write a same-directory tmp (same filesystem) + rename for atomic replacement (Unix explicitly sets 0o755; Windows deletes the old first, the non-atomic window is the user's accepted risk). On success → output `bingo updated to v0.3.0` + `Install location: <path> (the new version takes effect at the next startup)`, exit 0.
6. **Permission failure**: file-operation failure (e.g. `/usr/local/bin` read-only) → report `STORAGE_ERROR`, msg carries the install path + guidance (`sudo bingo update` or manual install).
7. **Extraction failure / corrupt asset** → report the error (`ARCHIVE_INVALID`), nonzero exit.
8. On Windows a running exe can't be replaced in place → report + manual-replacement guidance (v1 has no installer).

**Output contract** (v1.2 ruling: Chinese, human-readable, main's decision; error paths still use the unified contract):
- `bingo is already the latest version v0.2.1` — already latest (exit 0)
- `New version v0.3.0 found: run bingo update to install` — `--check` with a new version (exit 0)
- `bingo updated to v0.3.0` + `Install location: … (the new version takes effect at the next startup)` — update succeeded (exit 0)
- `[error] code=... msg=...` — failure, nonzero exit (unified error-code exit; success paths aren't forced into the single-line greppable format)

**Error codes** (v1.2 ruling: reuse generic codes + short codes, main's decision; registered in `src/error.rs`, SCREAMING_SNAKE, add-only): `OFFLINE` (network / HTTP non-2xx), `SERVER_ERROR` (abnormal release response), `CHECKSUMS_UNAVAILABLE`, `CHECKSUM_MISMATCH`, `ARCHIVE_INVALID` (extraction failure / missing binary), `UNSUPPORTED_PLATFORM`, `STORAGE_ERROR` (file operations/permissions, msg carries sudo guidance).

**macOS risk note**: a downloaded non-notarized binary carries the quarantine attribute, and Gatekeeper may block the first run — after the update-success message append a guidance line (`xattr -d com.apple.quarantine <path>`); v1 doesn't clear it automatically (safety consideration, left to the user's judgment).

## 5. Acceptance criteria (each verifiable)

### A. Version-check logic
- A1. semver comparison correct: `0.2.1 < 0.2.10`, `0.2.1 < 0.3.0`; `0.3.0` vs `0.3.0` counts as already latest; tags with the `v` prefix parse (unit tests).
- A2. Tag parse failure (non-semver / empty) → silently treated as no new version, no notice, no error.
- A3. A detected new version → cache and notice share the same source (the notice line's version = the check result's version).

### B. Cache and async
- B1. TTL effective: after the first check writes the cache, a restart within 24h sends no network request (injectable clock/cache path for testing; mock server counts requests = 1).
- B2. Failure doesn't write the cache: network failure → no cache write, no notice, no error; one background retry per startup (v1.2 ruling: the 1h failure rate-limit is dropped, implementation accepted).
- B3. First frame unblocked: with mocked network latency the TUI's first frame still renders, no waiting (welcome-card rendering only reads the cache, no network).
- B4. `--print` / subcommand fast paths (`share`/`update` etc.): no check triggered, no update-related output lines.

### C. Welcome-card notice (visuals per `update-banner.md` v1.1 as the single source of truth; anchors 1-11 are the complete assertions; this group merges at the PRD level)
- C1. New version (cache or live check result) → the welcome card shows the notice line, copy = `New version {v} available — run bingo update` (three-segment styling: static segments inactive, version and `bingo update` in the breathing color with the command bold); no new version → the welcome-card layout is line-for-line identical to today (regression).
- C2. Check failure / no cached result → the welcome card is exactly as today, no notice line.
- C3. Motion scope (effect-scope assertion): across any two rendered frames, the welcome card's other rows (✻ greeting / ╭╮ border / /help / cwd / identity line) are completely identical (assert against a static-row snapshot of doc.rows); the notice line appears with no entrance animation (silent insertion).
- C4. Breathing correctness (pure function `update_color`): under truecolor phase 0 = rest, phase 45 ≈ peak (±1/255), phase 90 = rest, 0→45 monotonically rising, 45→90 monotonically falling; **the version segment and the command segment take the same Color at the same phase (in phase)**; the in-line static segments are always `theme.inactive` (unchanged at any phase); during the frame-loop animation window it stays dirty, outside the window idle returns (zero writes).
- C5. Window and stop: after 9s (270 frames) it settles at the rest color, `needs_tick()` returns false; after the welcome card persists it's the static rest color (scrollback invariant); a resize within the window → the animation continues after rehydrate, no duplicate animation copies, zero redraws above the viewport.
- C6. Degradation chain: `motion: "off"` / `BINGO_NO_MOTION=1` → static rest throughout, the notice line stays; no truecolor → discrete two-step (400ms peak / 1600ms rest) without crashing; `NO_COLOR` → static bold. User input stops it early (if P1 is implemented).
- C7. Narrow-screen truncation chain: the 50/43/15 inner_w boundaries checked tier by tier (`banner_line` pure function); `bingo update` is visible in every tier (except <17 columns), never overflows the card frame, never wraps.
- C8. Contrast: dark ≥6.24:1 on every frame, light ≥4.72:1 on every frame (settled frame = rest); the light theme must never show the `#D77757` bright-orange stop.
- C9. Layout stability and no blocking: the notice line aligns with the card border (inside the `│` wrapper); no flicker/truncation on reflow/scroll/persist; input, commands, and scrolling all work normally while it appears (no focus stealing).
- C10. No ANSI blink: the output contains no `\e[5m` (grep assertion).

### D. `bingo update`
- D1. Platform-asset mapping: the four platforms each detect the correct file name (unit test: `aarch64-apple-darwin` → `.tar.gz` etc.); unknown platform → `UNSUPPORTED_PLATFORM`.
- D2. Output contract (v1.2 ruling): `--check` with a new version prints `New version v0.3.0 found: run bingo update to install` exit 0; already latest prints `bingo is already the latest version v0.2.1` exit 0; network failure prints `[error] code=OFFLINE ...` nonzero exit.
- D3. `bingo update` when already latest: downloads nothing, prints already-latest, exit 0.
- D4. Update success: the mock server verifies the whole chain "download → sha256 match → extract → replace"; after replacement `current_exe` is the new binary (test with a fake binary + temporary install directory), output carries the new version and install location, exit 0.
- D5. Checksum mismatch: refuses to install, the existing binary is untouched, nonzero exit + `CHECKSUM_MISMATCH` (mock server returns a tampered asset).
- D6. Missing `checksums.txt` / no matching asset line: same refusal to install (safety first, `CHECKSUMS_UNAVAILABLE`), clear error.
- D7. Insufficient permissions (install directory read-only): report `STORAGE_ERROR`, msg carries the install path + sudo/manual guidance, the existing binary is untouched.
- D8. Extraction failure / corrupt asset / missing binary in the package: report `ARCHIVE_INVALID`, nonzero exit.
- D9. Replacement failure (e.g. mocked rename failure): tmp residue cleanable, the old binary stays usable, report `STORAGE_ERROR`, nonzero exit.
- D10. All update errors go through the unified error-code exit (TUI/CLI dual-exit consistent), exit=1 + `[error] code=...` single-line format.

### E. Quality and contracts
- E1. `cargo build`, `cargo clippy -- -D warnings`, `cargo test` all green; check/cache/mapping/verification logic carries inline unit tests.
- E2. `src/error.rs` registers the actual code table (OFFLINE/SERVER_ERROR/CHECKSUMS_UNAVAILABLE/CHECKSUM_MISMATCH/ARCHIVE_INVALID/UNSUPPORTED_PLATFORM/STORAGE_ERROR) + drift-guard unit tests (every variant enumerated).
- E3. The welcome-card version line switches to the compile-time version (`CARGO_PKG_VERSION`), same source as the check's comparison (incidentally fixing the v0.1.0 hardcode).
- E4. The built-in skill `src/skills/bundled/guide.md` syncs: `bingo update [--check]` command quick reference, the `motion` config row (auto/off + BINGO_NO_MOTION), the capability map's "updates" item (check/notice/update full flow).
- E5. New dependencies only the three `tar`/`flate2`/`zip` (for extraction), nothing else.

## 6. Suggested acceptance order (dependencies)

1. Check core (semver comparison + TTL cache; pure functions testable first) → 2. async wiring (spawn + unblocked first frame) → 3. welcome-card notice line (static → motion/degradation, per `update-banner.md` §5 anchors) → 4. `bingo update` (mapping/download/verify/replace/permissions) → 5. error codes + docs (guide.md) wrap-up.

## 7. Risks and open items

- **Motion vs document-model conflict (resolved)**: after the welcome card flushes into scrollback the animation stops and settles statically — uiux spec §3.2 option A gives the wiring (the 9s animation window ends well before the normal settle-to-disk moment; within the window it stays a live row, after expiry it settles statically and persists, never touching scrollback); implement per the spec, no degradation plan needed.
- **GitHub reachability (rate limit / regional network)**: triple mitigation via TTL + silent failure + the `--check` manual entry; update failures append the manual-download URL to the error.
- **checksums.txt maintenance**: manually maintained by the release process; a missing/stale file makes update refuse to install — that's the expected safety-first behavior; the release checklist must keep the checksums in sync (publisher's responsibility).
- **macOS Gatekeeper**: v1 does no signing; the quarantine-block risk is handled by the post-success guidance line; if users report it often, v2 evaluates signing.
- **`sudo bingo update` scenario**: running update as root replaces files in the root-owned install directory, but the cache/tmp area is still in the user's home — the implementation must mind the permission boundary; writing the user cache as root needs explicit handling (P2 detail).
