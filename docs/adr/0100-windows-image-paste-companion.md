# ADR-0100 — An opt-in Windows image-paste companion

Status: proposed (experimental draft).

## Context

Windows Terminal can consume physical Ctrl+V before a native console TUI sees
it when the clipboard holds a bitmap, so the existing image reader is never
asked. Changing terminal bindings would change the person's established keys.
No kernel door is needed: this is input transport for the existing TUI effect.

## Decision

- Require `BINGO_WINDOWS_IMAGE_PASTE=1`, native Windows and interactive TUI I/O.
- Embed reviewable managed C# source in the TUI binary, extracted per run.
  System Windows PowerShell compiles it via a short fixed inline Add-Type
  command; no .ps1 is loaded, profile used or execution policy setting changed.
  Application control/Constrained Language may refuse compilation. Add no Rust
  dependency, Python dependency or unsafe code.
- A dedicated lifecycle worker reuses bingo-process's suspended spawn and
  kill-on-close Windows JobObject before managed startup. Owner death during
  compilation kills the helper too. Pass owner PID and a unique title marker;
  stdin EOF, parent death or lost console/raw mode also ends it. Readiness is
  exactly `BINGO_IMAGE_PASTE_READY_V1`, bounded at 8s. Publish the in-flight
  guard before waiting so restoration can cancel startup. Take it out of its
  mutex before stopping/joining; a worker-origin panic never joins itself and
  releases its JobObject on unwind.
- Defer the title marker until readiness, serialized with restoration. The
  terminal composes it on later titles and restores its original title stack;
  the helper never writes titles.
- Only physical Ctrl+V without Shift/Alt/Win is eligible. Ignore injected input.
  Prove live console membership/raw mode, foreground and fresh exact focused
  Windows Terminal pane using non-document UIA fields and the marker; only then
  inspect bitmap format availability. Any offered text format keeps normal text
  paste, including mixed text+bitmap clipboards. Keep UIA off the hook thread and bounded;
  recheck scope/focus immediately before console input injection.
- Suppress the original key only after WriteConsoleInput succeeds. Fail open
  on every uncertain proof. Transport only Ctrl+V, not clipboard bytes.

## Consequences

The ordinary `clipboard.rs` reader and `Effect::PasteImage` remain authoritative.
One persistent PowerShell process and runtime C# compilation are the explicit
cost of opting in. Policy restrictions, stale/ambiguous focus and unsupported
hosts leave normal terminal paste untouched; image paste may remain unavailable.
There is no clipboard mutation, content/key logging, SendKeys, elevation or
terminal-setting change. The hook's desktop-wide placement still warrants an
experimental warning and real WT split-pane validation before broader release.
Automated native-console tests do not prove that UIA identifies real WT panes.

## Supersedes

None. ADR-0052's picture-as-file path is unchanged.
