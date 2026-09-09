# 0051 — The chat carries more than words

## Context

M13 (ADR-0016) put a session in a Feishu chat and named its non-goals: files,
audio and stickers are "not this surface's", a group engages on a mention and
nothing else, and the bot says nothing while it works because the card writes
itself. A comparison against hermes-agent's Feishu adapter (2026-09-09, 4 320
lines of Python) showed what a chat actually carries: rich `post` runs with
links, code and mentions; interactive cards; merged forwards; shared chats;
stickers; files, voice notes and videos in both directions; a per-group
policy of who may speak; and a "working on it" reaction. It also showed the
one thing not to copy — media requested by a `MEDIA:` tag parsed out of the
model's prose, and a capability declared as a flag.

**Does any of this open a kernel door?** No. `Input::Text { text, images }`
stays as ADR-0040 §2 settled it: a surface resolves, the kernel journals, and
the kernel does no file I/O for input. A file is a path in the text — the one
fact the model needs, readable with the fs tool it already has. A `Document`
content part would be a second representation of that path, so it is refused.
Nothing here reaches `bingo-sdk` or `bingo-core`.

## Decision

1. **Every message is heard.** One pure normaliser, `feishu::content`, turns
   `(message_type, content, mentions, me)` into `Spoken { text, resources }`
   and never answers "nothing": `text` and `post` as today, with `post` runs
   `a` → `[label](href)`, `at` → `@name` (ours removed, `@_all` → `@all`),
   `code_block` → a fence, `hr` → `---`, `emotion` → `:name:`, and
   `media`/`file`/`audio`/`video` runs as resources; `interactive` → the
   header title, the text lines, `Actions: …`; `share_chat` → `Shared chat:
   name (id)`; `sticker` and any type not listed → `[<type>]`. A
   `merge_forward` is its title plus the merged messages, fetched once
   (`GET /im/v1/messages/:id`, at-least the first 50) and flattened by the
   same normaliser under `--- forwarded ---`.
2. **An attachment lands on disk at the edge.** A `file`, `audio` or `media`
   message — or such a run in a `post` — is fetched through the resource
   endpoint (`type=file`; `audio`/`media` retried as `file`) and written to
   `<data_dir>/channels/feishu/files/<message_id>/<name>`, the name reduced to
   a safe basename. The text gains one line per attachment,
   `[file: <name> → <path>]`; a `text/plain` or `text/markdown` file of at
   most 100 KiB is also inlined under it as a fence. Voice and video get the
   path and nothing else: this surface transcribes nothing. Entries older
   than 14 days (`bingo_pictures::cache::DAYS`) are swept on each write. A
   fetch that fails drops the attachment with a warning; the words still go.
3. **Files go out through a tool, never a tag.** `files() -> Option<&dyn
   Files>` is a fourth mechanism on `ChannelAdapter`: `post(to, parent,
   Outgoing { name, bytes, caption })`. The channels plugin contributes one
   `ToolSource` whose `SendFile` tool exists only while the surface runs
   (ADR-0009 §1: answering with nothing is never wrong), and resolves the
   calling session to its conversation through the surface's own directory
   of runners; a session that is not a chat is refused in words. Feishu
   routes by sniffed type and extension: a picture → `im/v1/images` and an
   `image` message; `.ogg`/`.opus` → `im/v1/files` as `opus` and an `audio`
   message; `.mp4` → `mp4` and a `media` message; `pdf doc docx xls xlsx ppt
   pptx` by name, anything else `stream`, and a `file` message. The
   multipart body is a pure brick, hand-written and fixture-tested: no new
   dependency, no new reqwest feature. Traits: trusted, not read-only,
   `subjects` = the path, so the gate asks once and a rule can allow it.
4. **Who may speak is a pure policy, per adapter, in the surface.** `Access
   { admins, direct: Rule, group: Rule, rules: {chat: Rule} }`, `Rule {
   policy, list, mention }`, `policy ∈ open | allowlist | blocklist |
   admins | off`. An admin passes everywhere. A per-chat rule overrides
   `group`; `mention: false` engages a group on every message, amending
   ADR-0016 §4. The default is what runs today — `open` with `mention:
   true` — so nobody's bot goes quiet on upgrade. The host evaluates it
   where `engaged` was; a refused arrival is dropped without a word, at
   debug level. The adapter still stamps `addressed`; the policy reads it.
5. **"I'm on it" is a mechanism, not a flag.** `acknowledge() -> Option<&dyn
   Acknowledge>`: `begin(at) -> Mark` when the runner submits the message,
   `end(at, mark, Outcome::{Done, Failed})` when its turn ends. Feishu: a
   `Typing` reaction on the message, removed at the end, `CrossMark` added on
   failure. `Typing` stays for platforms that have one; a platform hands
   over whichever it has, and the runner uses what it is handed.

## Consequences

- No dependency moves; `Limits` and the Deliverer's reducer are untouched
  except for the one fact the turn ended, which `Acknowledge` needs.
- Scopes the runbook gains: `im:resource` (already, for pictures),
  `im:message` (the merged messages), `im:message.reactions:write_only`,
  `im:message:send_as_bot` covers the uploads.
- A path in a transcript is a path on this machine: a session resumed
  elsewhere has the line and not the file, as with any path a person typed.
- The loopback adapter gains all four mechanisms so every behaviour above is
  proven black-box before Feishu is asked.
- Telegram/Slack/Discord, when they come, implement `Files`, `Acknowledge`
  and their own resource fetch; the policy and the tool are already theirs.

## Supersedes

ADR-0016 §4's "a group engages only when the bot is mentioned" becomes the
default of a rule. M13's non-goals for files, audio and stickers are closed.
