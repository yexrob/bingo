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

- [x] an agent's OpenRoom (default) reaches its own children; `/room` on the agent lists it — unit-proven; the CLI depth limit leaves a spawned child childless black-box, so `shared` carries the end-to-end proof
- [x] `shared: true` reaches the caller's siblings; root + shared is a worded refusal
- [x] the permission card names room, members and placement
- [x] a standing room is reset, not duplicated (same door as `/room`)
- [x] black-box: agent opens shared room, posts, sibling hears it, transcript attributes it
- [x] every gate green (fmt, check, clippy, test, discipline, budget unchanged, deny)

## Non-goals

Invite/kick verbs; ACLs; a rooms-listing tool; cross-tree rooms; changing
`/room`.

## Risks

R-seat — `seat::seat` takes the session to hang under; if it assumes "the
command's own session" anywhere (identity, cwd), the tool must pass the
placement explicitly and a unit test pins it. R-card — tool preview text is
the only place a person sees `shared`'s reach before approving; the
black-box asserts the words.

## Verified (2026-09-01)

- Worker E merged `37f629f`; gates ran on the integrated tree together
  with M17 and the agents lag fix: `GATES_EXIT=0`, 2509 tests passed,
  this slice adds no dependency (schemars was already in the workspace —
  one lock edge).
- R-seat was a non-issue: `seat::seat` already takes the parent
  explicitly; `/room`'s only diff is a shared `receipt` helper, its tests
  untouched. 17 unit tests (placement + tool) and 3 black-box scenarios:
  the shared room's key `rooms/{root}/design` proves the placement, and
  the never-started sibling holds the post in its own on-disk journal
  with `principal: "reviewer"`, `conversation: "#design"`; the card reads
  `OpenRoom #design under the caller with reviewer, scout` exactly; root
  + `shared` is a worded refusal proven with the gate held open.
- The card is `subjects()`, not `confirm()` — reviewed and kept:
  `confirm()` sits above allow rules and bypass, which would make
  OpenRoom un-allowlistable and impossible headless (a piped stdin never
  answers). The subject doubles as the rule key, so
  `OpenRoom(#design under the caller:*)` covers a placement; the cost is
  a prose-flavoured rule string, judged worth the card guarantee.

## Carried

- Wished sdk seams: `Preview::Text` (a tool with a sentence to say has
  nowhere but the summary — would dissolve the subjects-vs-confirm trade
  whole), splitting "needs a person" from "needs a card sentence", and
  `SessionFilter { id }` (every plugin regrows its own `own()`).
- A rooms-listing tool stays deliberately unbuilt (ADR-0021 §3); revisit
  with the collaboration milestone if briefs prove insufficient.
