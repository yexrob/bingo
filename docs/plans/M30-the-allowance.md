# M30 — The host's doors: ask and notice (ADR-0033, first slice)

## Goal

The bridge registers the reserved `bingo.host` service and opens the
two doors that have customers today: `ask` lets a bridge tool put a
mid-run question to the person on the one interaction path, and
`notice` lets a plugin say one line any time — fixing the drain M29
carried. The allowance table and the `complete` door wait for demand
(see Deferred).

## Bricks, in build order

1. **`bingo.host`, the door service** (new `doors.rs` or in
   `service.rs`) — an in-process `WireService` the bridge registers
   through M28's `open_service` at start; methods `ask` and `notice`;
   an unknown method answers with the spoken set (the M28 rule, same
   words). Wire fixtures for both methods' params and results land
   first; the handshake changes NOT AT ALL — this is a service like
   any other, discovered by calling it.
2. **The `ask` door** — takes `{call, question}`; **nothing is
   minted**: the door validates the call against the bridge's
   existing running-call map and the calling connection (the call's
   liveness is the grant — ADR-0033 §3 as amended; a second id for a
   fact that map already carries is the ADR-0011 debt). It routes to
   the live call's own asking machinery, so the question rides the
   interaction system every tool's questions ride — no second path.
   An ended call, or another connection's, is refused in words.
3. **`notice` and the drain** — no allowance; the plugin's name;
   level clamped to the sane set. M29 carried the path's defect here:
   `Notices` drains inside `PluginTool::call`, so in a session whose
   plugin serves no tool a queued notice is logged but unsaid. Give
   the channel one tool-independent drain (the bridge holds the host
   at start); a hook's `HOOK_UNANSWERED` and this door's notices ride
   the same fixed drain — one channel, one drain, said when it
   happens.
4. **Black-box** (`stub_plugin`, a doors test module beside the
   others) — a stub tool asks mid-call and acts on the answer; an
   ended or foreign call is refused in words; a notice surfaces with
   **no tool call in flight** (the drain proven); an unknown method
   answers the spoken set. Paused clocks throughout; nothing waits
   wall.

## Deferred (user scope call, 2026-09-01)

The allowance table, the `complete` door and the compactor activation
key wait for a real external strategy to exist. ADR-0033 keeps the
design on record — the family test, the socket rule — and when the
customer appears the cost is the table plus one minting site (§5),
not a redesign. Until then an external compactor stays
extractive-only and inert in the shipped composition; accepted.
M26's compactor crossing itself stays: merged, tested, inert,
harmless.

## Files

`crates/bingo-plugin-rpc/src/{wire,doors or service,notice,tool,
bridge,manager,lib}.rs`, `crates/bingo-plugin-rpc/examples/
stub_plugin/`, a new doors module under
`crates/bingo-plugin-rpc/tests/plugin/`, `schema/plugin.json` only if
a wire struct changed. No new dependencies; budget unchanged; no sdk
or kernel change expected (`open_service` already exists).

## Exit criteria

- [x] `bingo.host` registered through `open_service`; unknown methods
      answer with the spoken set; no handshake change
- [x] `ask`: a bridge tool's mid-run question rides the one
      interaction path and returns the person's answer; an ended or
      foreign call refused in words; nothing minted for it
- [x] `notice` surfaces under the plugin's name with **no tool call
      in flight** — the drain no longer waits for one, and
      `HOOK_UNANSWERED` rides the same drain
- [x] every gate green (fmt, check, clippy, test, discipline, budget
      unchanged, deny)

## Non-goals

The allowance table, `complete` and the activation key (Deferred);
doors for sessions, deliver or store; streaming; renewing or
transferring anything; any change to the permission plane; any
"while we're at it" table machinery — deferred means absent.

## Risks

R-drain — one channel, one drain: a second surfacing path is the
ADR-0011 debt; the fix must serve the hook's notices and this door's
alike. R-ask — reuse the interaction machinery every tool question
rides; a parallel question path is the same debt. R-scope — nothing
here anticipates the deferred work; if a brick seems to want the
table, the scope drifted — stop and report.

## Verified

Worker run, every gate green, 1-minute load 4.7–11.6 (the doors'
black-box waits on no deadline and no wall clock but the harness's
existing poll; nothing was rerun):

```
$ cargo fmt --all -- --check
$ cargo check --workspace --all-targets --locked
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.90s
$ cargo clippy --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.37s
$ cargo test --workspace --locked
2854 passed; 0 failed
$ scripts/check_discipline.sh
dependency direction ok / kernel names no tool / cohesion ok
discipline ok
$ scripts/budget.sh
dependencies (unique, normal): 302 (max 302)
budget ok
$ cargo deny check
advisories ok, bans ok, licenses ok, sources ok
```

What each criterion rests on:

- **`bingo.host`, a service like any other** (`doors.rs`,
  `manager.rs::open_doors`): the manager enters one face through
  `open_service` before any process is spawned, so the key is taken
  before a plugin could claim it and a process may call the doors from
  its first line. `tests/plugin/doors.rs::the_host_s_own_service_is_in_
  the_registry_under_its_key` finds it in-process by the one lookup
  (`service::<ServiceHandle>("bingo.host")`). The handshake changed not
  at all: `PROTOCOL` is still 5, `METHODS`/`NOTIFICATIONS` are
  untouched, and `connection::tests::exactly_one_method_travels_from_a_
  process_to_the_host` still passes unchanged — the doors ride
  `service/call`.
- **The spoken set, in M28's words**: `doors::unknown` writes the same
  sentence `RemoteService::speaks` writes. `doors::tests::a_method_the_
  host_does_not_speak_is_refused_with_the_set_it_speaks` and, over a
  real child, `tests/plugin/doors.rs::a_door_the_host_does_not_have_is_
  answered_with_the_ones_it_does` both read
  `"the service bingo.host does not speak complete; it speaks ask, notice"`.
- **`ask` rides the one interaction path, and mints nothing**: the door
  calls `ToolHost::ask` on the running call's own host — the same
  `Prompter` an in-process tool reaches through `cx.call` — and the
  answer specs are read off the question (`doors::answers`, the
  `tool-fs` pattern) rather than stated beside it. `tests/plugin/
  doors.rs::a_tool_s_mid_call_question_comes_back_as_the_person_
  answered` drives it over a real process: the stub asks mid-call, the
  test's person answers `Text { "next" }`, the recorder shows the
  process's own question was what was put, and the answer crosses back.
  Nothing is minted anywhere: there is no table, no id and no grant in
  the code.
- **An ended or foreign call, refused in words**: the running-call map
  is one connection's, so `Caller::running` asks that connection and no
  other. `doors::tests::a_call_that_has_ended_is_refused_in_words`
  (drop the watch guard), `..::another_connection_s_running_call_is_
  not_this_caller_s_to_ask_on` (two live connections, one call filed on
  the first), `..::the_host_s_own_face_runs_no_call_and_says_so`, and
  end to end `tests/plugin/doors.rs::a_call_that_is_not_running_is_
  refused_in_words` plus `..::another_plugin_s_live_call_is_refused_in_
  words` — the second holds one plugin's call open (proved live by its
  own progress line arriving, not by a sleep) while the other plugin
  asks about it, and reads back
  `"the call call_one is not one the two plugin is running: …"`. The
  two cases share one sentence deliberately: a process must not learn
  that another's call exists.
- **The verdict plane is not a door**: `doors::only_a_question` refuses
  everything but `InteractionKind::Question`, so no process can open a
  permission prompt and be handed an `AllowSession`.
  `doors::tests::a_permission_prompt_is_not_a_question_a_plugin_may_
  open` (all three refused kinds) and the black-box twin.
- **One channel, one drain** (`notice.rs`): `Notices` gained a `Notify`
  and `say`, and `notice::drain` is the single task the manager starts
  with the host. `PluginTool::announce` no longer records anything —
  the tool-call drain is gone, not duplicated — so `HOOK_UNANSWERED`, a
  death, a refused handshake and the `notice` door all surface the same
  way. `tests/plugin/doors.rs::a_plugin_s_own_line_surfaces_with_no_
  tool_call_in_flight` starts the stub with `--announce`, which calls
  `bingo.host.notice` the moment the handshake is answered, and reads
  `"stub: the index is stale"` off the host — no tool is called in that
  test at all. `tests/plugin/hooks.rs::a_hook_past_its_deadline_never_
  decides_and_a_notice_names_it` now reads `HOOK_UNANSWERED` through
  the same host, and `tests/plugin/lifecycle.rs::a_killed_process_
  leaves_one_notice_…` reads `PLUGIN_DIED` there, once.
- **A line nobody can hear is kept, not lost**: `host.notice` refuses
  when no session is open, and `Notices::say` puts the unheard back in
  order. `notice::tests::a_line_nobody_is_open_to_hear_waits_where_it_
  was` and `tests/plugin/lifecycle.rs::a_plugin_whose_command_is_gone_
  …`, which runs over a host with nowhere to land and still finds the
  notice on the channel.

Three decisions the plan left open, taken here:

- **The kernel grew one door after all.** The plan expected no sdk or
  kernel change, but nothing in `HostApi` could say a line to a person
  without naming a session, and the bridge at start has none — that is
  precisely why the notice had been waiting for a tool call.
  `HostApi::notice(level, code, text)` is the smallest door that fixes
  it: a default that refuses, ~20 lines in `bingo-core` that record the
  same `ItemBody::Notice` the tool-call drain used to record, on every
  session that is open right now and never on one that is not (no
  `mailbox_of`, so nothing is reopened to be told).
- **Who is asking is bound at the face.** `WireService::call` carries
  no caller and the params must not (a process that names itself could
  name another), so `Doors` is one object and `Doors::face(caller)`
  binds who is asking: the registry holds the face bound to the host,
  each connection's hub the face bound to it. One implementation, one
  set of words; only the caller differs. `Hub::wire` answers the
  reserved key with its own bound face rather than the registry's,
  which is what makes another plugin's call refusable at all.
- **The doors' shapes live in `doors.rs`, not `wire.rs`.** They are not
  wire methods — they ride inside one `service/call`'s opaque params,
  as every other service's do — and `wire.rs` was over the 700-line
  warn with them in it. `schema/plugin.json` gained a `hostService`
  section beside `methods` for the same reason: a door a plugin author
  cannot find is a door that does not exist.

## Carried

- **A notice is said on every open session.** `HostApi::notice` names
  no session because the bridge has none to name, so a host with three
  sessions open says a plugin's line three times, once in each. That is
  right for what the bridge says today (a process died; a plugin is not
  running) and it is the only reading `bingo.host.notice` can have —
  the door takes no session — but a future door that wants one place
  would need a scoping fact ADR-0033 §5's rule would have to name.
- **The doors' `$defs` names are unqualified** (`AskParams`,
  `NoticeParams`, …) now that the Rust types are scoped by the module.
  Nothing collides today; a future sdk type of the same name would.
