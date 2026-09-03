# ADR-0038 — A word not yet learned is read as text

Status: accepted · 2026-09-03 · Plan: M41

## Context

`bingo_sdk::View` (ADR-0013) is the one vocabulary a plugin describes
a screen with, and it is a closed serde enum tagged by `kind`: a
surface built yesterday that meets a kind added today fails to parse
the whole value — an error where a degrade belongs. The vocabulary is
also journaled (the panel lane) and crosses the JSON-RPC wire, so its
readers span versions by design. And a plugin with a niche element —
one only a richer surface will ever draw well — today has no way to
say it at all without teaching the sdk a new word first.

## Decision

1. **`View::Custom { kind, data, fold }`.** A plugin may put up an
   element the sdk has no word for: `kind` names it (namespaced by
   the plugin, e.g. `demo.sparkline`), `data` is its own shape, and
   `fold` is the mandatory text — what `--print`, a channel and every
   surface that has not learned the kind shows. A node with no
   honest fold has no business on a screen it does not control.
2. **Custom is also the catch-all.** `View` deserializes an unknown
   `kind` into `Custom`: the whole raw object as `data`, its `fold`
   field as the fold when present, else `[<kind>]`. From this change
   on, a newer speaker never breaks an older reader — a new sdk word
   lands as text until the reader learns it. (Binaries older than
   this change still error; the door helps every version after it.)
3. **Learning a kind is a surface's own affair.** A surface that
   recognises a kind renders it richly — a module inside that
   surface, invisible to the sdk and the kernel (only the TUI may
   know ratatui; a future GUI's renderer registry is its own). The
   kind's `data` shape is owned and documented by the emitting
   plugin, never by a surface.
4. **The sdk vocabulary still grows by real words.** Custom is the
   porch, not the house: an element that proves general graduates to
   a named `View` variant with a designed fold; a plugin's private
   element stays Custom forever, and that is fine.

## Consequences

- Every surface adds one arm: `Custom` renders its fold (until it
  chooses to learn kinds). The compiler walks each match.
- The committed plugin-rpc schema moves if `View` rides it;
  regenerated in the same change.
- Old journals replay unchanged; new journals holding custom kinds
  read as text on surfaces that predate the kind but not this ADR.
- No new dependency, no kernel machinery: one variant, one custom
  `Deserialize`, one fold arm.

Refs: ADR-0013, ADR-0002; Plan: M41
