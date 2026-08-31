# M18 — OpenRoom (ADR-0021)

## Goal

Agents can open rooms: `OpenRoom { name, members?, shared? }` in
`bingo-rooms`, placing the room under the caller (its own workers hear it)
or, with `shared: true`, under the caller's parent (its peers hear it) —
through the same `seat::seat` door `/room` uses, with the permission card
saying where the room will hang.

## Bricks, in build order

1. **Placement brick** — pure: given the caller's summary (id, parent),
   `shared`, answer the parent-to-seat-under or the refusal (a root asking
   for `shared`; the wording carries the reason).
2. **The tool** — `OpenRoom` in `crates/bingo-rooms/src/tool.rs`: schema
   (name required; members list; shared bool default false), traits fail
   closed (not read-only, not concurrency-safe), preview/card text naming
   room, members and placement; body = resolve placement → `seat::seat` →
   the same receipt `/room` gives. Registered in `lib.rs`; manifest
   `provides` gains `tool:OpenRoom` (the manifest test moves with it).
3. **Unit** — Fleet tests: default hangs under the caller and its children
   hear a post; `shared` hangs under the parent and a sibling hears it; root
   + shared refused with the reason; a standing room is reset not duplicated;
   the name rules are `/room`'s (one word, no slashes).
4. **Black-box** (`crates/bingo/tests/cli/rooms.rs` or beside the agents
   scenarios, temp HOME, fake script) — a scripted turn spawns an agent
   whose script calls `OpenRoom` (shared) and posts with `SendMessage` to
   `#name`; the sibling's transcript holds the post with `[from … in #…]`;
   the permission card named the placement (default mode asks — drive the
   approval like the existing agent scenarios).

## Files

`crates/bingo-rooms/src/{tool,lib}.rs` (+ the placement brick's module),
`crates/bingo/tests/cli/` one file + `main.rs` mod line. Nothing else.

## Exit criteria

- [ ] an agent's OpenRoom (default) reaches its own children; `/room` on the agent lists it
- [ ] `shared: true` reaches the caller's siblings; root + shared is a worded refusal
- [ ] the permission card names room, members and placement
- [ ] a standing room is reset, not duplicated (same door as `/room`)
- [ ] black-box: agent opens shared room, posts, sibling hears it, transcript attributes it
- [ ] every gate green (fmt, check, clippy, test, discipline, budget unchanged, deny)

## Non-goals

Invite/kick verbs; ACLs; a rooms-listing tool; cross-tree rooms; changing
`/room`.

## Risks

R-seat — `seat::seat` takes the session to hang under; if it assumes "the
command's own session" anywhere (identity, cwd), the tool must pass the
placement explicitly and a unit test pins it. R-card — tool preview text is
the only place a person sees `shared`'s reach before approving; the
black-box asserts the words.
