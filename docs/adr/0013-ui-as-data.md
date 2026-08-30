# 0013 — UI as data: one view vocabulary, three lanes, actions

## Context

A command answers `View::{Text, List, Table}` and the TUI's `ctrl+t` panel draws an `Extension` payload generically (ADR-0011 §2). That is the whole of what a plugin can show today. M11 wants the terminal to be rich — progress, diffs, boards, forms, images — and the user wants it extensible: a plugin, or the agent through a plugin's tool, should be able to put a live, interactive thing on the screen ("a TUI artifact") without the TUI crate knowing it. Three rules bound the answer: only the surface crate may depend on ratatui (ADR-0001), no surface defines a private mirror of kernel types (ADR-0002), and the kernel knows no plugin by name. The plan sketched a `Widget` extension point and per-plugin `*-tui` crates; that would put a second rendering path in every plugin and leave the RPC, print and IM surfaces with nothing.

## Decision

1. **One vocabulary, in the sdk.** `bingo_sdk::View` grows from three nodes into a small declarative tree. Leaves: `Text`, `Markdown`, `Code{lang, text}`, `Diff{unified}`, `List`, `Table`, `KeyValue{rows}`, `Progress{value, total, label}`, `Badge{text, tone}`, `Tree{nodes}`. Containers: `Stack`, `Columns`, `Panel{title, child}`. Interactive: `Actions{items: [{label, action: Action, key?}]}`. Every node has one text fold (`View::text()`); a surface that cannot draw a node draws its fold. Nothing in the vocabulary names a plugin or a feature; a plugin composes it from data it owns.
2. **Three lanes, told apart by durability.** *Blocks*: `ToolOutput.display: Option<View>` — what a person sees beside the `parts` the model reads; it replaces `Display::{Diff, Summary}`, which were two of the nodes. *Panels*: an `Event::Extension` whose payload parses as a `View` is drawn as one; any other payload keeps the generic fold. *Live*: `HostApi::signal(session, plugin, kind, payload)` publishes an ephemeral `Event::Signal{plugin, kind, payload}` — never journaled, folded by the reducer into `SessionState.signals` as the latest payload per `(plugin, kind)`, removed by a `Null` payload, absent after a resume. A progress bar updated ten times a second costs the journal nothing.
3. **Interaction is actions and questions, both existing.** An `Actions` item carries an `Action{name, args}`; a surface that fires it submits `Input::Action` (ADR-0008 §1), which runs a plugin's command. Whatever must stop a turn and wait for a person is an `Interaction` opened through `ToolHost::ask`, as before. Nothing new reaches the kernel's dispatch.
4. **Placement, focus and keys are the surface's.** A plugin names labels and, at most, a single-key hint; where a panel sits, how it gets focus, which key fires which action, when a live signal is folded away — the TUI decides once for everyone, and a GUI decides differently.
5. **Kernel touch**: `HostApi::signal`, `Event::Signal`, `SessionState.signals`, wire method `session/signal`. The kernel knows no kind and no plugin.
6. **Escape hatch, unused.** A native widget that the vocabulary provably cannot express is a *surface* crate (`tier = "surface"`, may depend on ratatui) assembled by the bin — never a plugin. The discipline rule's ratatui allowance widens to that tier the day the first one is written; none is planned.

## Consequences

- sdk touched once (2026-08-30): `View` moves to `bingo_sdk::view` and gains its nodes with `Tone`, `TreeNode` and `ActionItem`; `View::fold()` is the one degrade; `ToolOutput.display: Option<View>` replaces `Display` (touched `bingo-tool-fs`, `bingo-surface-print`, `bingo-surface-tui`); `Event::Signal` (not durable), `SessionState.signals`, `Applied::Signal`, `HostApi::signal` and wire `session/signal` are new; `schema/rpc.json` regenerated; every `HostApi` double gained `signal`.
- `View::text()` is the one degrade rule: `--print` prints it, an IM channel sends it, a GUI ignores it. A node added later ships with its fold or it does not ship.
- A plugin's UI is portable and testable without a terminal: a `View` value is asserted with `assert_eq!`, and a snapshot of the TUI proves the drawing once per node, not once per plugin.
- The `Widget` extension point and `bingo-teams-tui` in the plan are withdrawn; avatars, if ever, are a `Tree` with badges.
- Rate is the publisher's discipline: a signal is coalesced by the reducer, not throttled by the kernel; a plugin that publishes at 1 kHz makes its own subscribers lag (`Lagged` exists for that).

## Supersedes

Plan §2.10's "`View` generic rendering plus a `Widget` extension point; plugin widgets in their own `*-tui` crates". ADR-0011 §2's generic fold stands as the fallback.
