# M23 — The holder on the roster, and a real join (ADR-0028)

## Goal

A room can seat its own convener: `parent` on the roster hears every
post at zero turn cost (`Hold`), is woken by `@parent`, and owes what
any mentioned member owes. And `WaitAgent` becomes a real join: several
agents, one deadline, every outcome read honestly — a never-run standby
member included.

## Bricks, in build order

**Worker N — the holder (`bingo-rooms`):**

1. **The delivery brick, pure first** — given a room (roster possibly
   naming `parent`), a post's author and text: who is delivered, and
   how — members `Wake` as today; a rostered holder `Hold`, or `Wake`
   when the text mentions `@parent` (the one ADR-0022 matcher, reused,
   never a second grammar); the author never, the holder's seat name
   being the author guard (ADR-0028 §5). Table-tested in `post.rs`.
2. **The debt and the chase** — `parent` on the roster is a member to
   the mention fold; a debt owed by `parent` is nudged at the room's
   parent session, `Wake` like every nudge; `owed` rows and `/room`
   show the seat by that name.
3. **The words** — `/room`'s receipt and `OpenRoom`'s description say
   what a rostered holder gets, one sentence each.
4. **Black-box** (`tests/cli/rooms.rs` or `mentions.rs`) — members talk
   in a room whose roster names `parent`: the root runs no turn while
   they do, and the next person message absorbs the posts in journal
   order; `@parent` wakes the root at once and opens a debt the root's
   next post closes; a room without `parent` on the roster is
   byte-identical (the standing tests stand untouched).

**Worker O — the join (`bingo-agents`):**

5. **The join brick** — `WaitArgs` takes `agents: [name, …]`: all names
   resolved before anything is waited on (one unknown fails the call
   with the roster hint, nothing waited); one deadline from
   `timeout_s`; outcomes reported per agent in the order asked; the
   result is an error iff any outcome is not an answer (failed,
   interrupted, still working, never run). One agent reads as today.
6. **The honesty** — a session with no turn behind it is reported
   "seated and nothing has woken it; write to it or post in a room it
   is in", an error — never "finished without saying anything"
   (the standby misread found in review).
7. **The words** — `SpawnAgent`'s room pattern gains: name `parent`
   among the members when you want to hear the room yourself;
   `WaitAgent`'s description says it joins several; the description
   tests pin both.
8. **Black-box** (`tests/cli/agents.rs`) — a two-agent join returns
   both replies in asked order; a timeout names who finished and who is
   still working; waiting on an unwoken standby member says seated.

## Files

N: `crates/bingo-rooms/src/{post,mentions,chase,owed,command,tool}.rs`
and their tests, `crates/bingo/tests/cli/{rooms,mentions}.rs`.
O: `crates/bingo-agents/src/{wait,watch,spawn,lib}.rs` and their tests,
`crates/bingo/tests/cli/agents.rs`.
No shared files; no new dependencies; budget unchanged.

## Exit criteria

- [x] a rostered holder hears every post `Hold` and runs no turn until
      something else opens one; absorption order is journal order
- [x] `@parent` delivers `Wake` and opens a debt the seat's next post
      closes; the chaser and the card know the seat
- [x] a holder never hears its own post; exactly-once fan-out still
      pinned with the holder counted
- [x] a room without `parent` on the roster is byte-identical to today
- [x] `WaitAgent` joins several agents under one deadline, outcomes per
      agent, error iff any is not an answer
- [x] a never-run member reads as seated, never as finished
- [x] the words teach both patterns; description tests pin them
- [x] every gate green (fmt, check, clippy, test, discipline, budget
      unchanged, deny)

## Non-goals

Rostering `parent` by default; digest timers or any stored inbox;
`@all` waking a holder; changes to the person's exemption (ADR-0025
§4) or to the serial module (absorbed `Hold` posts already count);
removing `WaitAgent` (examined against the survey — the old tree's
top-gap verdict — and kept; this milestone reshapes it instead).

## Risks

R-shadow — a member deliberately titled with the holder's signing name
(`parent` is refused as an agent name already; an agent-holder's title
is not) would shadow the holder's authorship in the guard; accepted at
this scale, said in a comment beside the guard. R-order — holder
absorption leans on the kernel's queue order; the ADR-0027 pin covers
it, lean on that test rather than re-proving. R-compat — the reshaped
`WaitArgs` sweeps every reference (`lib.rs` doc, `spawn.rs` field doc,
the cli tests); `rg WaitAgent` before and after.

## Verified (2026-09-01)

- Worker O merged `d7a8368`: `agents: [..]` with resolve-all-first (an
  unknown or duplicated name refuses the call before anything is
  waited on), one `Deadline` computed once and shared by every wait,
  outcomes in asked order, error iff any is not an answer — with the
  landed replies still in the error text. The standby misread died in
  the type: `watch::last_reply` returns `Option<Reply>`, so "never ran"
  cannot read as "finished"; the lagged-resync path inherited the fix.
- Worker N merged `b95c697`: the pure `delivered` decision (nine-row
  table over roster x author x text), `Hold` to a rostered holder and
  `Wake` when `calls_on(text, "parent")` — the one ADR-0022 matcher
  read for a delivery mode; `post::seat` is the single seat lookup the
  chaser shares; `Room::audience` deleted, one representation left.
  `serial.rs` unchanged, as the ADR predicted: absorbed Hold posts
  count at the cut with no new wiring.
- Integrator fix `4dfe8c2`: the bounce-no-debt black-box seated
  `parent` on its roster only to feed the old fold; ADR-0028 made that
  seat live, so the root absorbed the question and legitimately landed.
  The roster is now honest (`["scout"]`) and the standing debt is the
  room's `@all`, which no post of the non-member root can close — same
  bounce, same subject.
- Integrated gates on `4dfe8c2`, load 26 falling to 16: fmt / check /
  clippy / discipline / budget (302 unchanged) / deny all OK;
  `cargo test --workspace --locked --no-fail-fast` exit 0 — 69 targets,
  2667 tests, zero failures, the load-sensitive relay test included.
  No terminal-byte change in this milestone; the PTY smoke was not
  re-run.

## Carried

- An agent holder's `@parent` debt does not self-close (fold matches
  roster names; an agent holder signs its title). Delivered, woken and
  nudged correctly; the debt outlives its answer in `owed`. Recorded in
  ADR-0028's consequences; thread the holder's title into the fold only
  when an agent-held room leans on mention debts.
- R-shadow: a member deliberately titled with its holder's signing name
  shadows the holder in the author guard; costs a deliberate act,
  accepted, commented beside the guard in `post.rs`.
- Worker N's `@parent` black-box flaked once under load when the woken
  scout outran the script; fixed by spare tail responses (the repo's
  pattern), stable across three full-binary runs since.
