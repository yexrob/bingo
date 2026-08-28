# Architecture decision records

One record per boundary decision: a trait shape, a wire format, a persisted format, a dependency, a crate split, a threshold family. Template: Context (≤10 lines) / Decision (≤15) / Consequences (≤10) / Supersedes. Hard cap 120 lines; longer material goes to `docs/design/` and is linked. Bug fixes are commit bodies, not ADRs.

- [0001 — Crate map and dependency direction](0001-crate-map.md)
- [0002 — One event stream: frames, journal, reducers, intents](0002-event-stream.md)
