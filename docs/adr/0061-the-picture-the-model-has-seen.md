# ADR-0061 — The picture the model has seen

Status: accepted · 2026-09-16 · Plan: M103 · Amends: ADR-0006 §3, ADR-0048 §4

## Context

A picture costs the token ruler a flat 1 600 and the wire whatever its
base64 weighs: a 2.2 MB PNG is 2.95 MB on every request that replays
it. Observed 2026-09-16 (`~/tmp/blender_demo`, Responses API through a
relay that keeps no conversation): one `Read` of a generated PNG at
round 4 of an 87-call turn, two sub-agents each reading a copy, and
from then on three 3 MB uploads per round in parallel. The relay cut
them after 55–140 s and logged "Failed to read request body"; bingo
saw `error sending request` and retried. Nothing in the budget
(ADR-0006) can see this: the count says 1 600.

Every open-source peer surveyed replays old pictures forever (codex,
gemini-cli, opencode, goose, crush, aider) and each carries open
413 / "request too large" issues for it; only cline drops attachments
from turns older than the latest. ADR-0048 §4 forbids rolling elision
of tool results on normal requests to keep the cached prefix stable.
A picture is the one payload where the trade inverts: one prefix miss
when it leaves the wire against megabytes on every round it stays.

## Decision

1. **A picture the model has answered `N` times leaves the wire.**
   Before a request is assembled, every `ContentPart::Image` that is
   followed by `N` or more assistant messages — at the top of a user
   message or inside a tool result, `N = budget::IMAGE_ROUNDS_KEPT = 3`
   — is projected to one text part:
   `[image elided: <media type> <size>]` followed by the picture's
   `whereabouts` words when it has any (ADR-0052 §3), so the model can
   `Read` the file when it wants the picture back. The size is the
   decoded bytes, decimal `KB` or `MB`, one decimal.
2. **A projection, not a record** (ADR-0006 §3's rule): items, journal,
   transcript and RPC keep the picture; a surface draws it; a reopen
   replays it. The projection runs on every normal request, after the
   no-vision projection (ADR-0040) and before the overflow elision, and
   the ruler measures what is sent.
3. **The threshold is the kernel's** and lives beside the others in
   `context::budget`; it is not a setting this round.
4. ADR-0048 §4 gains the exception: normal requests do not elide old
   tool results **except a picture past §1's age**. ADR-0006 §3's note
   family gains the image note.

## Consequences

- The prefix breaks once per picture, at the round it leaves. A turn
  that reads a picture and works on for twenty rounds uploads it three
  times, not twenty.
- A picture with no path (bytes from a wire client, a tool that made
  them) cannot be read back once elided; the note says what it was.
  The same trade ADR-0006 §3 already makes for a text result.
- A model that wants a picture again spends one `Read`; the tool's
  own cap (`Image::MAX_BYTES`) is unchanged, and shrinking a picture
  before the model sees it is a separate decision, not taken here.
- No crate or dependency is added; no wire shape changes; the fake
  provider's recorded requests are the test surface.

Refs: ADR-0006, ADR-0040, ADR-0048, ADR-0052; Plan: M103
