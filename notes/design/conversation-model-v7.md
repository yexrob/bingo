# Conversation model v7 — one wake rule, one sigil, seven duties

> **Status: in effect.** Batches 0–3 have landed (D127, D128, D129, D131);
> `max_awake` is the one piece deliberately dropped — see "Not built".
> Where this file and `conversation-model-v6.md` disagree on waking, obligation
> or main's place, this one wins; v6 still holds for everything else (pages, the
> roster, the byte contracts). The decision records are in `notes/research.md`.
>
> Ruled by the user 2026-08-17, in the session that followed D124–D126.

## The verdict on v6

v6's wake gate solved the storm it was built for: one "Hi" into a crew room is
no longer N model calls. Three things it did not solve, and all three showed up
in one afternoon of using it.

1. **`@` carries three meanings at once** — address, obligation, and wake. The
   receiver has to infer which was meant from prose, and the note that teaches
   that inference is half a page of judgment calls ("if nothing in them changes
   what you are doing…"). The model has no observable signal to answer that
   question with. D124 is what one of those judgment calls cost.
2. **The gates are proxies for a question the sender can answer.**
   `ROOM_UNREAD_WAKE = 5` and `ROOM_UNREAD_MAX_AGE = 120s` exist because the
   runtime cannot tell whether a line matters — but the member who wrote it can.
   The count gate is worse than a proxy: in a room of six, one round where
   everyone speaks leaves five unread in every inbox and re-crosses the gate for
   all of them. The threshold is an amplifier at the room size it was meant to
   protect.
3. **Main is a special case in four places** (the pen, the age pump, the sweeper
   exemption, the digest) for a job every other member does with none.

The user's ruling reverses the direction of the fix: stop gating attention, and
put the obligation where the information is — on the sender.

## The model

Three levels. Each does exactly one thing, and no level implies another.

| | means | delivered | wakes idle | interrupts running | owes a reply |
|---|---|---|---|---|---|
| a plain message | read this | at once | yes | no — absorbed at the next tool boundary | no |
| `@name` | **I need your answer** | at once | yes | no | **yes** |
| `urgent` | stop what you are doing | at once | yes | **yes** | yes |

`@` no longer buys speed, because everything is fast now. It buys an answer.

### One wake rule, and it is the same for everybody

> A non-empty inbox wakes its holder, debounced. `@` decides what is owed.
> `urgent` decides what interrupts.

- **A running agent never wakes.** It absorbs its whole inbox at the next tool
  boundary — input tokens, no model call. This is what steering already is, and
  what `query_loop` already does at the top of every round: the mail drain sits
  inside the loop, so main has been absorbing at tool boundaries all along. Only
  the pen stands between that and this design.
- **An idle agent starts a run**, coalesced by the debounce below.
- **Main is a member.** No pen, no age pump, no exemption. Running → steer.
  Idle → wake. The four special cases retire into one sentence.

Two guards survive, and they are the only two:

- **Debounce (2s, 15s deadline)** — v6's digest coalescer, kept and generalised.
  One run per burst, not one per line: a question and its answer are one wake,
  not two. This is coalescing, never gating; nothing waits on a count.
- **`max_awake` per room** — how many members one event may put in flight at
  once. The rest queue and coalesce. Nothing is dropped, and the bound is
  independent of room size, which is exactly what the count gate was not.

## The seven duties

The receiving prompt collapses from a page of inference to these. The English
line under each is what `CHANNEL_NOTE` should carry.

**R1 · `@` is the only source of obligation.** Named, you answer this turn. Not
named, you owe nothing — not an acknowledgement, not a "got it". There is no
third case, and in particular there is no judgment about whether a line
"changes what you are doing".

> An `@` on your name is the only thing that owes an answer. Nothing else does.

**R2 · An answer goes to the room, and it is substantive.** Acknowledgement is
not an answer. If the answer is "already doing it", that sentence *is* the
answer — the sender needs to hear it.

> Acknowledgement is not an answer. If the answer is "already doing it", write
> that sentence.

**R3 · An answer never `@`s the person it is answering.** They are waiting and
will read it. Needing another round from them is a *new* message, on its own
budget. This is the hard form of "never answer an answer", and it is the only
door a ping-pong can come through.

> Never `@` the person you are answering — they are already waiting for it.

**R4 · `@all` asks the room, not each member; the first substantive answer
closes it.** Under immediate delivery this stops being a guess and becomes an
observation: if the answer is already in what you are holding, you are covered.

> `@all` asks the room, not each member. If an answer is already in what you
> woke with, you are covered — add a line only if yours carries something
> theirs did not.

**R5 · A name you are quoting is written without the `@`.** Reports, recaps and
summaries say `dev`; a summons says `@dev`. The sigil is a summons, not a word.
The tool result names who was put on the hook, so a misfire is visible at once.

> A name you are quoting or reporting about is written without the `@`.

**R6 · Unbidden speech is allowed, and narrow.** Speak without being named only
for what changes what someone else will do: a decision, a blocker, a hazard
someone is walking into, a result. Progress, agreement and commentary are not
that.

> Speak unbidden only for what changes what someone else will do.

**R7 · `@user` wakes main, which is the user's member in the room.** The human
owes no immediate answer and cannot be woken; main holds the obligation for
them. Three clauses make the relay honest:

- **R7a — a relay is verbatim.** D123's briefing form (own words, compressed)
  is for *room activity*. A question addressed to the user is not activity, it
  is a question: quote it, attribute it, name the room. Compressing it is
  distorting it. The rendering itself costs no model call — the harness can
  quote the line into main's page directly.
- **R7b — a relay owes a return path.** The mention creates two debts: main to
  the user (ask), and main to the room (answer). Both are tracked; the roster
  shows `waiting on you (#room)`. A forward with no way back is worse than a
  badge, because the asker waits while the answer sits in main's page.
- **R7c — main may answer for the user, and must say so.** Knows the answer:
  answer the room and tell the user it did. Does not know: escalate. **Never
  invent the user's position** — this is the one place the model's talent for
  smoothing things over is actively dangerous.

> `@user` asks the human to look. They owe nothing on a clock; main carries it.

The badge and the single ring stay as the floor: a room main is not in still
lights up, and the user's own eyes are still the pull surface.

## The ledger

Every `@` is a debt the runtime records. This is not new bookkeeping — it is
two machines that already exist, rekeyed and finally rendered.

**What exists.** `AckState` (`Queued → Delivered{run} → Answered{run}`, or
`Dropped`) with the follow-up chase behind it, and `Channel::mark_seen`, a
per-member cursor of the last message a member has read. The first serves
*direct messages only* and is keyed on `ack_timeout`, a number the sender has
to guess. The second is read by exactly one caller — the serial-mode staleness
bounce — and has never been shown to anyone.

**What changed (D131).** The debt is keyed on the `@`: `channels::Mention`
records who asked, whom, in which room, at which sequence, and when. It closes
when the named member next posts to that room — **speaking is the answer**,
because R2 already says an acknowledgement is not one and a runtime that
second-guessed the wording would be making the judgement call v7 exists to
remove. `@all` is one debt against the room, closed by the first answer from
anybody but the asker (R4). Unclosed past five minutes it is chased, up to the
same three rounds a direct message gets, with the same watch line back to the
sender.

Two departures from the sketch above. The chase is a *parallel* mechanism rather
than a rekeyed `AckState`: the direct path's record is per-instance and keyed on
`MsgId`, the room's is per-room and keyed on a sequence, and forcing one into
the other would have coupled two lifetimes that have no reason to move together.
And `ack_timeout` does not retire — it stays what it always was, the direct
path's knob — while the room's wait is a constant on purpose, because a sender
who has to remember to ask for the check is the failure the default removes.

**What it buys, at the cost of wiring and rendering only.**

*A live per-conversation state* — the slot a messenger fills with
"typing… / delivered / read", which bingo has nothing in:

```
● main
○ dev        owes #dev-team #5 · running 40s
○ qa         idle
# dev-team   waiting on @dev                •2
```

*An answer to "did they miss it"* — today a silent member is four
indistinguishable situations, and only one of them is fine:

| | today | with the cursor and the ledger |
|---|---|---|
| has not read it yet | invisible | `read to #4`, and yours is #5 |
| read it, working | invisible | `read to #7 · owes #5 · running 40s` |
| read it, not answering | invisible | `read to #7 · owes #5 · idle` — a bug you can see |
| its turn died | invisible (D124 buried four of them) | the state says so |

Rows three and four are guesses today. D124 was row four: four crew turns died,
main told the user it was a transient stream error, and it took a screenshot to
find out. With this slot filled, that shows up on the first round.

## What main says

Main obeys R6 like everyone else; its audience is the user. The tiers below are
that rule spelled out, and belong in `MAIN_CHANNEL_NOTE`.

| what it read | what it does |
|---|---|
| `@user`, or anything needing the user to decide | say it now, **verbatim** (R7a), and owe the room its answer (R7b) |
| a state change — someone blocked, a task finished, a decision made, the plan drifting | one line, own words, compressed (D123's form) |
| pure progress — someone is working on something | **say nothing, and know it** — until asked, or until it becomes one of the rows above |
| discussion, mutual answers, FYI traffic | nothing |

The third row is the point of the whole design. **Main's value is not what it
says when it wakes — it is that it always knows.** An assistant that reads every
line aloud is worse than the room page; an assistant that has to go look when
asked is worse than nothing. Continuously current and deliberately quiet is the
only useful third option, and it is only affordable because a running main
absorbs for free and an idle main coalesces.

## What retires

The design is a subtraction. Everything below goes:

- `ROOM_UNREAD_WAKE` (the count gate, and the amplifier)
- `ROOM_UNREAD_MAX_AGE` and the 15s `ROOM_WAKE_SWEEP` sweeper
- `MainPen`, `pump_main_gate`, and the three pump points
- `inbox_wakes`'s four-gate predicate → a non-empty check
- the half page of `CHANNEL_NOTE` that asks the model to infer obligation
- `@user`'s private delivery semantics (it is a mention of main now)

What is left: one wake rule, seven duties, two guards.

## Landing order

Prompt before machine, deliberately — the gates are cheap to keep and expensive
to restore, so the duties get observed under load before anything is deleted.

| # | batch | landed | scope |
|---|---|---|---|
| 0 | the page bugs | **D127** | the `ctrl+o` pager reads the active page, not always main; a speaker's name renders when avatars are off (D113's ruling, half-implemented) |
| 1 | the sigil and the duties | **D128** | `CHANNEL_NOTE` → R1–R7, `MAIN_CHANNEL_NOTE` → the four tiers, `@` semantics in the tool descriptions. **No machine change**; the gates still stand. Observe. |
| 2 | the wake rule | **D129** | delete the gates, generalise the debounce, main becomes a member with no exemptions |
| 3 | the ledger | **D131** | `@` as a tracked debt: opened by the sigil, closed by the answer, shown on the roster with the read cursor beside it, chased when it goes unanswered. `max_awake` dropped on the user's ruling. |

## Not built, deliberately

- **`to: "user"`** — proposed and dropped in the same session: the user's
  address is `main`, and `@user` reaching main is the same rule as `@dev`
  reaching dev. A second address for the same human would be two doors into one
  room.
- **member↔member DM** — unchanged from v6. Two members who need a side channel
  convene a room of two, which is how every messenger implements a DM anyway.
- **threads** — R4 is the anti-chorus clause, and immediacy turns it from a
  guess into a lookup. A second axis in the log is not paid for yet.
- **A rate limit on `@`** — considered (a wake-token bucket at the sender) and
  dropped: with obligation moved to the sender and R3 closing the ping-pong,
  the cascade terminates at depth 2 on its own.
- **`max_awake`** — dropped by the user when batch 3 was ordered ("我感觉那个没必要").
  It was the last survivor of the gating instinct v7 exists to reverse: the
  ruling is that **everyone wakes**, and a bound on how many is a gate wearing a
  number. Every member reading every line is not the storm — the storm was every
  member *speaking* — and R1 is what stops the speaking. The one thing it would
  have bought is a ceiling on concurrent model calls, which is a cost question,
  not a correctness one; the room's real latency is a member's own turn, and
  queueing members behind each other would make that worse, not better.
