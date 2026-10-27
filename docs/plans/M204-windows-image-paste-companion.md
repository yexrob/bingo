# M204 — Experimental Windows image-paste companion

ADR-0100. Draft, opt-in compatibility work; not a default desktop hook.

## Goal

Let physical Ctrl+V reach the existing image reader in a native Windows
interactive TUI when Windows Terminal consumes image-only paste first.
Keep ordinary paste, existing bindings and terminal settings unchanged.

## Bricks

1. Pure opt-in, readiness-token and title-composition contracts with Rust tests.
2. Reviewable bundled managed C#; no Python, new Rust dependencies or unsafe
   blocks. Extract into a unique temporary directory; compile with a short inline
   PowerShell Add-Type command, not a .ps1 or changed execution policy setting.
3. A companion guard: absolute system PowerShell, no profile/policy changes,
   bounded readiness, piped lifetime. Reuse bingo-process on a dedicated worker
   for suspended spawn and OS kill-on-close JobObject before managed startup;
   cleanup before terminal restoration, normal and panic paths.
4. Terminal-owned per-run title marker, composed on every title update; retain
   the terminal's existing title save/restore stack.
5. Helper policy tests and an isolated native-console test of input injection.
   The helper scopes to live owner/parent, console membership/raw mode, foreground
   and a fresh exact focused Windows Terminal pane before inspecting bitmap formats.
6. Help describes opt-in, runtime cost, unsupported environments and failure modes.

## Files

- `crates/bingo-surface-tui/src/terminal.rs`, `terminal/image_paste.rs` and tests.
- `scripts/windows-image-paste.cs` and its managed/native-console tests.
- `crates/bingo-surface-tui/src/guide.md`, ADR-0100 and this plan.

## Exit criteria

- [x] Rust opt-in, readiness, title and cleanup tests pass.
- [x] Helper policy/native isolated-console tests pass.
- [ ] Format, workspace check/clippy/tests, discipline and budget checks pass.
- [ ] Real Windows Terminal split-pane/focus-switch behavior is manually verified.
- [ ] Cross-platform build and regression checks pass.

## Non-goals

Default-on interception, other shortcuts, clipboard mutation, text/document
inspection, key/content logging, SendKeys, settings changes, elevation, WSL,
remote sessions, multiplexers, arbitrary terminals, or replacing `clipboard.rs`
and `Effect::PasteImage`.

## Risks

A persistent Windows PowerShell process compiles C# at startup. Application
control, Constrained Language/AppLocker, missing UI Automation or inaccessible
consoles can refuse it; failure leaves terminal Ctrl+V untouched. Script execution
policy is unchanged; this launcher invokes an inline compiler, not a .ps1. Focus
proof can be unavailable on some WT versions; fail open rather than broaden
scope. Each focus proof is a fresh bounded query; never reuse an earlier
positive result, and prove focus again immediately before injection. The hook
is desktop-wide in placement even though its policy is not.

## Verified

Windows GNU, Rust 1.99.1; tools, temporary files and artifacts were kept on D.
Final-source commands:

```text
cargo fmt --all -- --check
exit 0
cargo test -p bingo-surface-tui --lib --locked terminal::image_paste -- --test-threads=1
15 passed; 0 failed; 4 fixture entries ignored
cargo check --workspace --all-targets --locked
exit 0
cargo clippy --workspace --all-targets --locked -- -D warnings
exit 0
cargo build -p bingo --locked
exit 0
```

The fixture entries are exercised by isolated subprocess wrappers (disabled,
startup owner death and actual restore/panic paths) and the separate managed
native-crossterm harness. The managed policy, mixed-format matrix, console ABI,
production launch/EOF/parent-owner lifecycle and real crossterm decoding suites
passed through an inline PowerShell harness; script policy was not changed.

Real WT preflight exposed a missing lifetime-pipe contract: Tokio `Child::wait`
closes stdin retained inside Child. The worker now holds that pipe separately.
The new persistence test was run against old code first: exit 101 with
`wait closed lifetime stdin after readiness`. Restoring the fix makes it pass.
Final-source log: `D:/Apps/Bingo/temp/clipboard-lifetime-green.log`; red proof:
`clipboard-lifetime-red.log`. A fresh isolated WT launch confirmed that the
native helper remains alive after readiness; physical user acceptance is pending.

A full TUI run before this final pipe fix passed 1569 tests. The final GNU full
TUI attempt instead crashed with `0xc0000005 STATUS_ACCESS_VIOLATION`; it is not
counted as a passing full-suite result. Log: `clipboard-lifetime-final-validation.log`.
Earlier discipline checks passed; final-source cross-platform fork CI remains
an acceptance gate. No new dependency or manifest/lockfile change is made.

Known gates are not waived: Windows dependency budget 432 exceeds 423 (also
reproduced on clean origin/dev), and deny reports baseline RUSTSEC-2026-0386.
Prior headless ACP/MCP tests had load-sensitive timeouts. No unrelated security,
budget or ACP changes are made. Real WT image/text CtrlV, CtrlC, multiple windows,
panes and focus switches remain unverified. Isolated native-console tests do not
establish real WT pane correctness. No upstream PR acceptance is claimed.
