# PRD: `bingo share` subcommand (HTML session share page)

> Status: v1.3 revision (acceptance anchors + scope boundaries; the body's view set/data sources updated to the version dev finalized; this version wrapped up by pm)
> **v1.3 revision (pm, 2025-08-07)**: main ruled **share page v4.0 (Claude Code app style, user-specified direction, replacing the v3.x opencode replica) into this merge** — acceptance anchor switched to the **v4.0 template (MD5 8c29a17b, 43 review assertions)**; group V reviewed by uiux against the 43 assertions, groups A/C/E/F/G data-layer acceptance logic reused (a pure visual/interaction generation change; the data-source contract is unchanged; the Bash non-command-field tool-args grid contract is kept); artifact assertions updated per design.md v4.0 §5 structure (user right bubble .msg-user>.bubble / assistant left markdown flow / tool collapsible cards with status badges / grey italic thinking / sticky top bar / Team thread list, etc.); the v3.1 anchor's acceptance results (4d9b616+1b28a39) are replaced by the v4.0 regression results.
> **v1.2 revision (pm, 2025-08-07)**: main ruled v3.1 into the merge (three chat-record views + opencode replica), anchors c626cdfb→fa153a2b→e79b37aa; group V has 51 assertions.
> **v1.1 revision (pm, 2025-08-07)**: ① the view set and data sources finalized per user instruction (four views = conversation / Team roster / DMs / channels, ShareStore incremental persistence); ② interface language follows the template per the uiux-share decision (#team-share #8): lang="en", English UI labels, data content verbatim; ③ visual acceptance anchor = share-page-template.html (single source of truth); ④ images embedded as data: URIs is a mandatory acceptance item (B2).
> Related: `bingo share` already merged into dev (e689af5, v2.2); the v3.1/v4.0 iterations run on the dev branch
> Visual implementation: dev outputs per [share-page-design.md](./share-page-design.md) (HTML generation contract) + `share-page-template.html` (single source of truth, MD5 8c29a17b); this document only defines acceptance and scope

## 1. Goal and user scenarios

**One sentence**: `bingo share` exports a session into a single HTML file openable offline, so people without bingo can still read the conversation.

**User scenarios**:

1. **Retrospective**: after a complex task (multi-round tool calls + sub-agent collaboration), the user exports the session as an archive or for review.
2. **Collaboration/review**: send the session to a colleague/friend (no bingo installed, no terminal); they open it in a browser and read.
3. **Bug report / demo**: send a failed debugging process to a maintainer, or demo bingo's team-collaboration ability (conversation + Team roster + DMs + channels, four views on one page).

**Non-goals**: no online hosting, no real-time collaboration, no replay execution — the share is of **what actually happened**.

## 2. Scope boundaries (v1 explicitly does not do)

| Not doing | Reason |
|---|---|
| Multi-session comparison/merged export | one session, one file; no real user yet for combination needs |
| Online hosting/upload, shareable-link generation | needs servers and an account system; v1 is purely local generation |
| Access control (password/expiry/burn-after-reading) | the file is public as-is; v1 only warns at output time "may contain sensitive information; judge before sharing" |
| Automatic sensitive-information redaction | semantic sensitivity can't be reliably recognized; left to the user's judgment |
| Tasks view | the user's instruction finalized the v1 view set = conversation/Team/DMs/channels; the tasks view isn't in v1 (template §9: a panel can be appended if extended) |
| Interactive filtering/search/theme switching | static page, subtract by default; add light JS in P2 only if needed |
| Multi-language pages | v1 interface English (lang="en", UI labels and empty states in English, consistent with the template; data content verbatim); no internationalization |

## 3. CLI interface

```
bingo share [session name] [--output <path>] [--open]
```

| Argument | Description |
|---|---|
| `[session name]` | positional, optional. Same naming scheme as `/resume` (`{slug}-{ts}` or renamed `{slug}-{ts}-{name}`). Default = the most recent session |
| `--output <path>` | output file path. Default = `<session name>.html` in the current directory; if it already exists, overwrite directly with a hint |
| `--open` | open with the system default browser after generation (`open` / `xdg-open`) |

**Default behavior**: pick a session → read and parse → generate a single HTML file → print the output path (consistent with bingo's existing headless output style; under non-TTY prints the greppable single-line `[share] wrote <path>` format). Session missing / file parse failure → unified error-code exit (`STORAGE_ERROR` etc., existing contract in `src/error.rs`).

**Error message**: when the session name doesn't match, list the similar available sessions (reusing `/resume`'s list-presentation style).

## 4. Data sources and the four views

**Implementation (dev landed it; main's ruling)**: during the session, the runtime **incrementally persists** sub-agent instances (with full history) and channel logs through `ShareStore` into `~/.local/share/bingo/shares/<session-stem>.json` (a single JSON file, atomic write via tmp+rename, corrupt backups rebuilt; storage failures only warn, never block the session — share is an enhancement, not a contract). `bingo share` reads that document + the transcript to generate the HTML.

| View | Content | Data source | Availability |
|---|---|---|---|
| **Conversation view** | the main session message flow: user/assistant text, thinking (collapsed), tool calls (collapsed: input JSON + result), images | transcript JSONL (`~/.local/share/bingo/transcripts/<slug>-<ts>.jsonl`) | always available (falls back to an empty document when the share doc is missing; this view only) |
| **Team roster** | sub-agent instance overview (name / def / state / history count / description) | ShareDoc `agents[]` (runtime upsert: insert/finish/stop events synced) | recorded during the session; empty state otherwise |
| **DM view** | each instance's full private history (SendMessage continuation is that instance's history) | ShareDoc `agents[].history` | recorded during the session; empty state otherwise |
| **Channel view** | channel metadata (mode/members) + message flow (serial order) | ShareDoc `channels[]` (create/invite/kick/post events synced) | recorded during the session; empty state otherwise |

**The view definitions' single source of truth = `share-page-design.md` v4.0 / `share-page-template.html`** (main ruled v4.0 in: Claude Code app style — near-black dark background + centered 800px message flow + user right warm-grey bubbles / assistant left markdown flow + tool collapsible cards with status badges + grey italic thinking + sticky top bar; four chat-shaped views: Team = thread list, DMs = per-agent chat flows, channels = per-channel message flows; interaction = tabs + hash + keys 1-4; JS only switches display, never touches data; without JS the conversation panel is the default). The PRD accepts, doesn't define visuals.

**Interface language**: English, following the template — `lang="en"`, UI labels (Conversation/Team/DM/Channels/Thinking/Show result/Print etc.) and empty states (`No …`) in English; data content (session text, tool input/output) verbatim.

## 5. Acceptance criteria (each verifiable)

### A. Data completeness
- A1. Every message in the transcript appears in the conversation view, in file order (compared one by one, including thinking / tool_use / tool_result / image blocks).
- A2. Bad-line skip semantics consistent with `Transcript::load_messages`: a single corrupt line doesn't fail the whole export; good lines all render; a warning goes to stderr.
- A3. Empty sessions (0 messages) and single-message sessions both produce valid HTML, no panic, no empty output file.
- A4. Tool-call input JSON and results render **verbatim** (escaped), never truncated or lost (unlike the TUI collapse's "+N lines" truncation — the export page is the complete factual record).

### B. Four-view content
- B1. The conversation view contains thinking collapsible blocks (grey italic `∴ Thinking`) and tool collapsible cards (icon + tool name + argument summary + status badge ✓ done green / ✗ error red / ◐ running orange + duration), collapsed by default, expandable.
- B2. Messages with image blocks render embedded per the template contract: `<img src="data:{media_type};base64,{data}" alt="">` (media_type validated `image/*`, data escaped, data: URI only, visible offline).
- B3. Team view (thread-list shape): sessions with sub-agents show a thread-row per member (avatar/name/status/recent-message preview + `data-jump` to the DM anchor); no sub-agents → empty state (not an error).
- B4. DM view: every instance with history presents its complete conversation history (chat flow `.dm-flow`, agent left / user right bubble); no history → placeholder; no instances → empty state.
- B5. Channel view: sessions with channel messages render in order (`◇ #name` + mode chip + member chips; each message carries sender/seq, user right-aligned); no channel activity → empty state.
- B6. Empty states don't break the page structure (the four panels always exist; no data → English `No …` placeholder; under tab interaction without JS the conversation panel is the default, the rest hidden).

### V. Visual and structural (template alignment, v1.3 update)
- V1. The artifact's structure matches `share-page-template.html` (single source of truth, MD5 8c29a17b): Claude Code app style — near-black background #0D0D0F / centered 800px message flow / user right warm-grey bubble `.msg-user > .bubble` (14/4 radius) / assistant left markdown flow (no bubble, inline code orange tint #E8B08F) / terracotta #D77757 used sparingly (brand, tool icons, hover, selection) / all tokens present.
- V2. The parts and four views are usable in the artifact: sticky top bar (brand + session title + meta info + four-view tabs), grey italic thinking collapsible (∴ Thinking · N tokens), tool collapsible cards (icon + name + argument summary + status badge + expanded full input/output, Bash non-command fields keep the tool-args grid), Team thread list (avatar/name/status/preview + data-jump), DM chat flows (agent left / user right), channel message flows (◇ #name + mode chip + member chips + seq); progressive-enhancement JS (tabs/anchor copy URL#msg-N/copy buttons/thread jumps/print expand), fully readable without JS.
- V3. The mechanical review method is the design.md v4.0 review script `share-review.js` (43 assertions) (uiux runs the comparative review; any FAIL is bounced back with the diff line); a passed review is the evidence for group V acceptance.

### C. Escaping safety
- C1. All dynamic text (user input, model output, tool input/output, session names, channel names, member names, instance names/descriptions) is HTML-escaped; construct test sessions containing `<script>`, `<img onerror=...>`, `&"<>'` — the exported HTML's data sections have **no unescaped injection** (grep asserts no `<script>` tags, no `onerror=` originating from data).
- C2. Tool input JSON renders as `<pre>` + escaped (not parsed as HTML).
- C3. Images only allow `data:` URIs (built from base64 blocks); no external URL or html content passes through.

### D. Offline usability
- D1. The artifact is a **single** HTML file: no external CDN, no external CSS/JS/fonts, no `<iframe>` external content; opens via `file://` and renders completely (verifiable offline).
- D2. Open the artifact in a browser in a no-network environment: the four panels, images, and collapses all work (Network panel shows 0 requests).
- D3. Without JS the page is still complete: the conversation panel visible by default, the others hidden (JS only enhances panel switching, never concatenates data).

### E. Legacy-session compatibility
- E1. Old transcripts with **no** sub-agent/channel/task data and no share-related metadata still produce a complete conversation page (this is v1's main path, not a degraded path).
- E2. Missing-field tolerance: thinking without signature, tool_result without content, unknown block types beyond role — skip that block without panicking; the rest of the page stays complete.

### F. CLI behavior
- F1. No session name → most recent session (same `Transcript::latest` source as `--continue`); `--output` takes effect; `--open` calls the system opener.
- F2. Nonexistent session name → nonzero exit + unified error-code output + similar-session list hint.
- F3. `--output` pointing to an unwritable path → clear error, nonzero exit.
- F4. `bingo share --help` documents all arguments.

### G. Quality gate
- G1. `cargo build`, `cargo clippy -- -D warnings`, `cargo test` all green; related logic carries unit tests (at least one set each for parsing/escaping/view extraction).
- G2. User-visible behavior (the new subcommand, error messages) synced into the built-in skill `src/skills/bundled/guide.md`.

## 6. Suggested acceptance order (dependencies)

1. Data layer (transcript parsing + four-view extraction) → 2. HTML generation and escaping → 3. CLI assembly and error paths → 4. compatibility (group E) and security (group C) → 5. docs and guide sync.

## 7. Risks and open items

- **ShareDoc vs transcript consistency**: the share doc is an incremental runtime snapshot (an enhancement, not a contract); if the session crashes midway / storage fails, the sub-agent/channel views may lack tail data — accepted for v1 (documented: the share page's conversation view is always complete; the team/channel views follow the runtime snapshot).
- **Large-file performance**: hundreds of messages + many base64 images can produce MB-scale HTML; v1 does no streaming pagination, but parsing must be a single O(n) pass (image downsampling considered in P2).
- **Privacy warning**: at export time print to stderr "this file contains the full conversation and tool output (may include sensitive information); review before sharing".
