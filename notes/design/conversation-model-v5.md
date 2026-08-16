# Conversation model v5 — the inbox turn (D114–D116)

Agreed with the user 2026-08-16, after real-terminal use of v4 and a
three-survey research pass (CLI subagent rendering; orchestration
mission-control products; group-chat/room paradigm — Devin-in-Slack, Claude
Tag, Copilot coding agent, Slack notification engineering, MetaGPT pub-sub).
Where this file and notes/design/conversation-model-v4.md disagree, this one
wins; v4 remains the record of what was built.

## The verdict on v4

The skeleton survives whole: one transcript, a persistent status layer, the
zoomed view, the background dialog — that is exactly where the industry
converged, and the zoom's view-is-steering is ahead of most of it. What real
use rejected is the **social layer running in push mode**: agent→main
arrival lines, `●` notices for runs main did not dispatch, and room-triggered
runs stapled into main's streaming turns all write into a write-once
scrollback that can never take anything back. One "Hi" into a crew room
grew four kinds of rows.

The industry's answer, three ways independently: the main flow is an
**inbox**, not a monitor. Process lives in containers; containers announce
themselves with badges; the user pulls. Nothing interrupts but a whitelist.

## The laws (binding for every batch)

1. **The flow whitelist.** The transcript renders exactly four things: the
   user's own messages; main's prose; runs main's turn itself dispatched
   (the `◉` row, its live progress, its settle, one dim `●` notice); and
   things that need the user (`⚑` mention/question, `⚠` failure, permission
   asks). Nothing else — no arrival lines, no third-party run rows.
2. **Everything else is a container with a badge.** Agents and rooms are
   entered (zoom), never streamed at the user. Activity brightens; only
   things *about the user* count.
3. **State updates in place, never as appended rows.** Running/idle/waiting
   live in the chrome (pills, tree), redrawn every frame — the write-once
   doctrine already demands this.
4. **Interruption is a whitelist, badges have two tiers.** Activity = a
   style change, no number. About-you (@user, a question, waiting on
   permission, a failure) = number/accent/bell. One event interrupts once.
5. **Perception is not presentation.** Every cut here is view-layer: main's
   inbox, wake paths, task notifications, room deposits and the debounce are
   untouched, byte for byte. Main hears everything it heard before; the
   *user* stops being shown main's mail.

## Delivery vs. rendering (the answer to "does main still perceive?")

| Event | main's context (unchanged) | the screen (v5) |
|---|---|---|
| agent→main SendMessage | inbox → drained envelope, wake | no line; sender's pill/tree dot until its zoom is visited |
| dispatch done | task notification | `◉` settle + one dim `●` (dispatch-origin only) |
| room/DM-triggered run done | notification per `wakes_owner` (D98 rule) | tree/dialog only — no `●`, no stapled row |
| room post | member deposit, debounced digest | room pill badge; `⚑` line iff it names/asks the user |
| agent run failed | notification | `⚠` line stays, always |

## Batches

- **D114 — the gate.** Watch registrations carry a `dispatch` bit (true only
  for `Agent`-tool spawns; deliveries and continuations are false). The
  transcript staples agent watch rows and prints `●` only when it is set.
  Arrival lines retire: `drain_main_arrivals` feeds a per-sender mail count
  (badge fuel), `push_teammate_line` and the D111 streak fold with it.
  `⚠` stays unconditional. Prerequisite: `tool/agent.rs` sits at the 4000
  cap — the two NOTE constants move to `tool/agent_notes.rs` first.
- **D115 — the status layer, consolidated.** `ctrl+t` narrows to the task
  panel alone (user ruling: "ctrl+t 只和 task 展示有关"); the tree's door is
  `shift+↑/↓`, as it already is. Rooms join the tree (rows after instances;
  enter zooms the room) and the pills. Badges everywhere the store already
  counts: agents dot on unread/mail, rooms dot on unread, `•N` accent on
  user-mentions. Zoom visits clear what they show.
- **D116 — needs-you.** A `⚑` flow line when a room post names or asks the
  user (one per mention-flip, D79 attention), advertising the reply grammar
  (`#room <msg>`). Tree rows and pills show a waiting-on-permission state;
  the ask dialog already names the requesting instance.

Deferred, unnumbered: main's room-digest narration discipline (prompt layer,
extends D112) — observe D112 first. Not built, ruled out by the survey:
kanban/tree overviews, member-side delivery debounce, reviving `[[quiet]]`,
per-row diff stats, pane mode (stays gated behind the D109 fork).
