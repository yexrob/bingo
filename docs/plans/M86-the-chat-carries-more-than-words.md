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

- [ ] every ADR-0051 §1 shape has a fixture; no message type answers `None`
- [ ] file / audio / video in, path in the text, ≤100 KiB text inlined; a
      hostile name cannot leave the message's directory
- [ ] `SendFile` black-boxed on loopback; Feishu routes fixture-pinned;
      multipart body byte-pinned
- [ ] access table test; three black-box cases on loopback; default = today
- [ ] acknowledge bracket black-boxed on loopback; Feishu reactions wiremock
- [ ] `scripts/budget.sh` unchanged; `cargo deny check` green
- [ ] every gate green; Windows check for `bingo-channels`
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
