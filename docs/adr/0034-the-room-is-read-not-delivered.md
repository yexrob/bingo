# ADR-0034 — The room is read, not delivered

Status: accepted · 2026-09-02 · Plan: M37

## Context

A room post is written once to the room's journal and then copied into
every member's journal as a `User { origin: room }` item (ADR-0025
§2). The copy is what wakes the member, what the serial rule counts to
say "you had read 9 of its 10 posts", and what every surface draws in
the member's transcript — so a person watching the parent sees the
room's chatter twice, and the model sees it once per member. A live
test on 2026-09-02 with four flash-class members produced a storm: the
default ear is live (ADR-0029 §1), every post woke every seat, every
woken seat posted, and the only brake was a sentence in the prompt.
The user asked for three things: the default is patient, `@` is the
wake, and a room's activity is the room's — a member's transcript, the
parent's included, shows none of it; a wake just opens a turn.

## Decision

1. **A post is written once.** The fan-out appends to the room's journal
   and copies nothing. For each seat it decides one thing: whether to
   wake it.
2. **Every seat keeps a cursor** — the room's `seq` it has read up to —
   as an extension on the *member's* session (`bingo.rooms`,
   `cursor:<room>`), journaled, so `--continue` finds it. The cursor is
   the one fact "seen" derives from.
3. **A wake is a nudge**: `Delivery::Wake`, `principal: None`, no body —
   not a post, no debt, no serial count (ADR-0029 §3's nudge). A seat is
   woken when it is `@`-named (ADR-0029 §5), when its ear is live, or
   when a patient ear's patience has elapsed with its cursor behind the
   room's head. A patient seat whose cursor is at the head is not woken.
4. **Reading happens at the head of a turn.** A `ContextContributor` in
   `bingo-rooms` folds, for each room the session sits in, the posts
   after its cursor into one user piece — `[#design, since you last
   read]` then `reviewer: …` per post — and advances the cursor to the
   head in the same step. A member reads a room only there; what it
   read is exactly what its cursor says.
5. **The serial rule reads the cursor** (ADR-0025 §3, amended): a post
   from a seat whose cursor is behind the head is bounced with what it
   missed, as today; the count comes from the cursor, not from copies.
   Mention debts (ADR-0022) are unchanged: they always folded from the
   room's journal.
6. **The default ear is patient** (ADR-0029 §1, reversed): a bare name
   on a roster is a patient ear at the chaser's 300 s; a live ear is
   asked for as `name:0`. `SpawnAgent`, `/room`, `OpenRoom` and
   `team.json` say so in their own words.
7. **A holder on the roster reads like any seat** (ADR-0028 §2,
   narrowed): `parent` is a cursor and an ear, nothing more; its
   transcript shows no post.

## Consequences

- The member's journal carries no room post; a surface has nothing to
  hide. The room's own view (`⏺ reviewer: …`) is unchanged. The session
  list may say `in #design · 3 unread` from the cursor.
- Storm bound: a patient seat opens at most one turn per patience unless
  named; a live seat is a choice written on the roster.
- `bingo-experience`'s recall reads the member's journal; a room post
  reaches its memory only through the turn that read it. Recorded as a
  behaviour change, accepted.
- Old journals holding copied posts replay as they were written; the
  contributor starts a seat with no cursor at the room's head at the
  time it was seated — a journal line (`cursor` on seating) — so a
  resumed member does not re-read a room's whole history.
- The kernel changes not at all: a contributor, an extension and a
  nudge are doors it already has.

## Supersedes

ADR-0025 §2 (the copy), ADR-0028 §2 (holder delivery), ADR-0029 §1
(the default ear) — each amended in place with a reference here.
