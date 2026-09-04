# Interactive surfaces that answer back — research, 2026-09-04

What Claude Code ships for "open something a person interacts with, and
the result reaches the session", from primary sources (an Opus research
agent; URLs inline). Read for bingo's own door, not to copy.

## What exists

| surface | shipped | round trip |
|---|---|---|
| **Artifacts** — https://code.claude.com/docs/en/artifacts.md | v2.1.183 (2026-w25, beta) | a published HTML/Markdown page on `claude.ai/code/artifact/<id>`; **comments** sent to Claude (v2.1.221) and **watching** (v2.1.228: a sent comment wakes the publishing session, 60/h cap, listed in `/tasks`); a page's republish is a notice to the watcher. In the shipped binary, gated per account and undocumented: `capabilities` (`window.claude.*` — page keeps state, saves new versions of itself, asks Claude, viewer identity, files), `read_page_data`, `room_send`. |
| **Claude Design** — https://code.claude.com/docs/en/artifacts.md#draft-a-design-canvas | `/design` v2.1.234 (2026-w34, preview) | artboards on a canvas published as an artifact running the Design editor; Save = republish = the watcher's notice. Standalone at claude.ai/design. Likely what "claudeian" meant. |
| **AskUserQuestion** — https://code.claude.com/docs/en/agent-sdk/user-input.md | old | 1–4 questions × 2–4 options, `header`, `multiSelect`, "Other" free text; `preview` per option (markdown/html) rendered beside the label; SDK answers through `canUseTool`. A dialog, not a page. |
| **MCP elicitation** — https://code.claude.com/docs/en/mcp.md | v2.1.76 | a server's `elicitation/create` becomes a schema form in the terminal, or a browser URL to confirm; `Elicitation`/`ElicitationResult` hooks can auto-answer. |
| **Channels** — https://code.claude.com/docs/en/channels-reference.md | v2.1.80 (preview) | an MCP server pushes events into a running session; two-way channels carry a reply tool and permission relay. |
| **Claude in Chrome** — https://code.claude.com/docs/en/chrome.md | GA 2026-w27 | agent → browser only; observed page state returns as tool results. Not a person's submit path. |

## What bingo can reproduce

- **(a) A local page that posts back** — fully, no hosting: serve a
  page on an ephemeral localhost port, open it, hold the tool call
  until the page POSTs, emit the submission on the event stream. Loses
  sharing and multi-viewer state only.
- **(b) MCP elicitation, the client half** — fully, and it is a
  standard: render the server's schema form in the TUI (M53's form
  card is the brick), or open the URL variant; any elicitation-capable
  server then works unchanged. ADR-0039 §3 recorded this need.
- **(c) A hosted page with comments** — only with infrastructure
  (publish endpoint, versions, ACL, a push transport to wake an idle
  session). The channels contract gives the "outside world wakes the
  session" property without hosting; bingo's own channels plugin is
  already that shape.

Not verified: the `capabilities`/`read_page_data`/`room_send` findings
come from strings in the shipped CLI binary (2.1.260), not a public doc.
