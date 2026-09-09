# M86 — The chat carries more than words

## Goal

User, 2026-09-09, after the hermes-agent comparison: the Feishu channel
hears every kind of message, takes files in and sends files out, lets a
group say who may speak, and shows it is working. Streaming stays on
CardKit — it is already ahead of the comparison. ADR-0051 records the
boundaries; nothing reaches the kernel.

## Bricks, in build order (three slices, one worker each)

**A — inbound** (`feishu/content.rs`, `feishu/files.rs`, `feishu/ws.rs`)

1. `content::spoken(message_type, content, mentions, me) -> Spoken { text,
   resources: Vec<Resource> }` (pure). `Resource { message, key, kind:
   Picture | File { name } | Audio | Video { name } }` replaces
   `event::Picture`. Fixtures: a `post` with every tag of ADR-0051 §1, an
   `interactive` card, a `share_chat`, a `sticker`, a `file`, an `audio`, a
   `media`, and the unknown type `[<type>]`. `event.rs` keeps the envelope
   (chat, thread, mentions-me, click) and calls it; `event.rs` is over 500
   lines already, so the normaliser is its own module.
2. `Heard` gains `forwarded: Option<String>` for a `merge_forward`; the fetch
   step reads `GET /im/v1/messages/:id`, flattens each item with brick 1,
   caps at 50, and joins under `--- forwarded ---`. wiremock test.
3. `files::fetch(api, dir, &[Resource]) -> Fetched { images, lines }`:
   pictures as today through `bingo_pictures::sniffed`; the rest to
   `<dir>/<message_id>/<safe name>` with the `[file: name → path]` line and
   the ≤100 KiB text inline; the 14-day sweep; `audio`/`media` retried as
   `type=file`. `Config` gains `files: PathBuf` from `env.data_dir`. Tests:
   one of each kind against wiremock, a name with `../` and a slash, a
   refused fetch that keeps the words, a text file over the cap that is
   named and not inlined.
4. `ws::with_pictures` becomes `with_resources`; the lines are appended to
   the text after a blank line. The `feishu/snapshots` fixture of an inbound
   message with attachments is pinned.

**B — access and acknowledge** (`access.rs`, `adapter.rs`, `host.rs`,
`runner.rs`, `deliver.rs`, `settings.rs`, `loopback.rs`, `feishu/mod.rs`,
`feishu/api.rs`)

5. `access::Access` and `Rule` as ADR-0051 §4, `Access::admits(&self,
   &Conversation, principal, addressed) -> Result<(), Refused>` (pure, a
   `Refused` names its reason). Table test over every policy × direct/group
   × admin/listed/unlisted × addressed. Settings: `access` under each
   adapter's key (`channels.feishu.access`, `channels.loopback.access`),
   schema-pinned; the default parses to "open, mention". The host keeps
   `BTreeMap<adapter id, Access>` and `engaged` asks it.
6. `Acknowledge` trait and accessor. `Op` grows the one fact the turn ended
   and whether it failed (the worker picks the smallest shape; the deliver
   fixtures pin it). The runner calls `begin` on submit with the message
   that spoke, keeps the `Mark`, calls `end` when that turn's end arrives.
   Loopback records `acknowledge`/`acknowledged` ops under a config flag.
7. Feishu: `Api::delete`; `POST /im/v1/messages/:id/reactions {"reaction_type":
   {"emoji_type":"Typing"}}` → `reaction_id`; `DELETE …/reactions/:reaction_id`;
   `CrossMark` on `Failed`. wiremock test; the smoke runbook gains the scope.
8. Black-box through the real binary on loopback: a blocklisted principal
   in a group gets no session; an admin in an `off` group does; `mention:
   false` engages without the word; the acknowledge pair brackets a turn.

**C — files out** (`adapter.rs`, `tool.rs`, `directory.rs`, `lib.rs`,
`host.rs`, `runner.rs`, `loopback.rs`, `feishu/mod.rs`, `feishu/upload.rs`)

9. `Files` trait and accessor; `Outgoing { name, bytes, caption:
   Option<String> }`. `directory::Directory`: `SessionId → (adapter,
   conversation, parent)` registered by `Runner::open`, removed when the
   runner ends, shared by the surface and the tool.
10. `upload::multipart(boundary, fields, file) -> (content_type, body)`
    (pure, fixture-pinned) and `upload::route(name, bytes) -> Route {
    endpoint, file_type, msg_type }` (pure, table-tested). Feishu `Files`:
    image → `im/v1/images`; else `im/v1/files`; then the message, in thread
    where `parent` says so. wiremock test per route.
11. `tool::SendFile`: spec `{ path, caption? }`, `subjects` = the resolved
    path, traits trusted + not read-only, refuses a session the directory
    does not know, a path that does not read, or an adapter with no
    `files()`, each in words. The plugin's `ToolSource` answers it only
    while the surface runs (a flag the surface sets in `run`). Black-box
    through the real binary on loopback: the model (fake provider) calls
    `SendFile`, the loopback peer records `file` with the bytes.

## Files

`crates/bingo-channels/src/{access,directory,tool}.rs` new;
`feishu/{content,files,upload}.rs` new; the rest as listed. `docs/adr/0051`,
`scripts/feishu-smoke.md` (scopes, three new checklist lines).

## Exit criteria

- [x] every ADR-0051 §1 shape has a fixture; no message type answers `None`
- [x] file / audio / video in, path in the text, ≤100 KiB text inlined; a
      hostile name cannot leave the message's directory
- [x] `SendFile` black-boxed on loopback; Feishu routes fixture-pinned;
      multipart body byte-pinned
- [x] access table test; three black-box cases on loopback; default = today
- [x] acknowledge bracket black-boxed on loopback; Feishu reactions wiremock
- [x] `scripts/budget.sh` unchanged; `cargo deny check` green
- [x] every gate green; [ ] Windows check for `bingo-channels` (CI: `aws-lc-sys` will not cross-build on this Mac)
- [ ] live Feishu smoke per the runbook (user)

## Non-goals

- Transcribing voice or reading video: the path is handed over, no more.
- A `Document` content part, or any kernel type: ADR-0051's door stays shut.
- Webhook mode: the long connection is the transport.
- A per-user rate limit or a DM pairing flow: `allowlist` is the answer.
- Editing a sent file, deleting a message, reacting to anything but our own
  progress.

## Risks

- The merged-forward endpoint needs `im:message`, a sensitive scope on some
  tenants: a `merge_forward` without it degrades to its title, with a warning.
- `SendFile` in default mode asks once per call; a rule on the path
  subject answers it. Worth a line in the runbook.
- Three workers touch `adapter.rs`, `runner.rs`, `settings.rs`; the merge is
  the reviewer's, in the order A, B, C, with each rebased on the last.

## Verified (2026-09-09, dev `21f72c01`)

```
cargo fmt --all -- --check                                        → clean
cargo check --workspace --all-targets --locked                    → Finished
cargo clippy --workspace --all-targets --locked -- -D warnings    → Finished
cargo test --workspace --locked --no-fail-fast
  -- --skip a_proxy_with_a_token_this_run_never_minted_gets_nothing
                                                                  → 4443 passed, 0 failed
cargo test … a_proxy_with_a_token_this_run_never_minted_gets_nothing (alone)
                                                                  → 1 passed
scripts/check_discipline.sh                                       → discipline ok
scripts/budget.sh    → dependencies 335 (max 335); budget ok — no new crate
cargo deny check     → advisories ok, bans ok, licenses ok, sources ok
```

`cargo test -p bingo-channels` is 208 tests (was 124 at M13);
`cargo test -p bingo --test channels` is 13 through the real binary.

Three worker slices, merged in the order B, C, A (each rebased on the
last): B — `access.rs`, `Acknowledge`, `Op::Ended`, Feishu reactions, and
two fixes found in the user's gateway log on the way (the question card
sent a schema-1 `action` container that card JSON 2.0 refuses, so every
permission had fallen back to numbered text since the 2026-08-31 smoke;
the CardKit settings patch sent an object where the endpoint takes a
string, so no stream ever closed and every answer was posted twice);
C — `Files`/`SendFile`/`Directory`, the hand-written multipart brick;
A — `content.rs`, `attachments.rs`, `merged.rs`, `Picture` → `Resource`.

Not done here, and deliberately:

- **The Windows cross-check.** `cargo check --target x86_64-pc-windows-msvc`
  dies in `aws-lc-sys`'s C build on this Mac before any workspace crate,
  on `dev` as much as on the branches. CI's `windows` job is the check.
- **The live Feishu smoke.** Every Feishu wire fact is wiremock-pinned;
  the runbook's new lines (attachments, the working sign, `SendFile`, the
  card buttons and the closed stream) are the user's to tick.
- **The sweep's directory branch** in `attachments.rs` has no test: a
  directory's mtime cannot be set back without a dependency.
- **Windows reserved device names** (`CON.txt`) are not rewritten: such
  an attachment fails to write and is dropped with a warning.
