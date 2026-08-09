# bingo share page design (v4.0 · Claude Code app style)

> Version: v4.0 · Status: finalized (**the single source of truth = `share-page-template.html` v4.0**; this file is the summary and contract description)
> Positioning: the self-contained HTML share page exported by the `bingo share` subcommand. **Presentation follows the Claude Code app (claude.ai/code desktop application)** (user-specified direction, replacing the v3.x opencode replica).
> References: GitHub issues #48158/#51069 interface descriptions + Claude design language + bingo's existing brand semantics (`--accent:#D77757`, terracotta orange, brand-shared).

## 0. Design principles

1. **Chat-app look**: dark near-black background, centered width-limited message flow (800px), warm-grey user bubbles on the right, assistant markdown flow on the left — a session snapshot of the Claude Code app, not a terminal screenshot or a doc page.
2. **Brand restraint**: terracotta `#D77757` only for brand moments (wordmark prefix, links, tool icons, hover, selection); status semantics use green/red/orange badges without stealing the show.
3. **Factual completeness**: tool input JSON and results render verbatim, never truncated (PRD A4); bash non-command fields go into the `tool-args` grid.
4. **No-JS core**: data is server-rendered by Rust (fully escaped); JS is progressive enhancement only (tabs/anchor copy/copy buttons/thread jumps/print expand); without JS the page stays fully readable (collapsed content defaults closed but `<details>` opens natively).
5. **Unified four views**: conversation / Team thread list / DM chat flows / channel message flows, all in this style; Team keeps the thread-list shape (member recent-message preview + jump to DM).

## 1. Visual anchors

| Anchor | Decision |
|---|---|
| Background | near-black `#0D0D0F`; surface `#151518` (tool cards); code blocks `#1B1B20`; user bubble warm grey `#3A3731` |
| Message flow | centered width limit `--maxw: 800px`; user bubble `--bubble-max: 72%` |
| User messages | **bubble on the right**: warm-grey background, 14px radius (4px inner corner), `You · time` meta right-aligned |
| Assistant messages | **markdown flow on the left**: no bubble, clear contrast between body and code blocks (inline code orange tint `#E8B08F`) |
| Tool calls | **collapsible card**: icon + tool name + argument summary + **status badge** (`✓ done` green / `✗ error` red / `◐ running` orange + duration), expand to see full input/output |
| thinking | collapsible block: grey italic summary (`∴ Thinking · 88 tokens`), grey italic body |
| Top bar | sticky: brand `▸ bingo` + session title + meta info (project/model/time/mode) + four-view tabs |
| Type | system sans-serif body; monospace for code/tool names/seq/meta |
| Brand | terracotta `#D77757` used sparingly; status green `#4EBA65` / red `#FF6B80` / in-progress orange `#F0A05A` |

## 2. Tokens (summary; full set in the template `:root`)

```css
:root {
  --bg:#0D0D0F; --surface:#151518; --surface-2:#1B1B20;
  --bubble:#3A3731; --bubble-text:#F2F0EC;
  --hairline:#26262B; --hairline-strong:#36363D;
  --text:#EDEBE7; --dim:#A8A39B; --faint:#77726A; --ink:#0D0D0F;
  --accent:#D77757; --green:#4EBA65; --red:#FF6B80; --gold:#FFC107; --running:#F0A05A;
  --hue-0..5 (dark-variant member colors);
  --maxw:800px; --bubble-max:72%;
}
```
Contrast: body 15.9:1, dim 7.2:1, accent 6.2:1, all semantic colors ≥4.5:1 (print mode recalculates separately).

## 3. Four views (all chat-shaped)

| View | Shape |
|---|---|
| **Conversation** | message flow: user right bubble (`.msg-user > .bubble`) / assistant left markdown (`.msg-assistant > .content > .md`) + thinking collapsible + tool collapsible cards |
| **Team** | thread list (`.thread-list > .thread`): round avatar (member color) + name + status (●◐✗) + message count + recent-message preview + footer (time · def); the whole row `data-jump`s to the DM |
| **DM** | one chat flow per agent (`.dm-block`): header (avatar + name + status + def) + `.dm-flow` message flow (agent left / user right bubble, sender member color) |
| **Channels** | per-channel message flow (`.ch-block`): header (`◇ #name` + mode chip + member chips) + `.ch-flow` message rows (seq + sender member color + text, user right-aligned) |

Empty state: `— No … —` (`.view-empty`), always present in all four views.

## 4. Interaction (progressive enhancement)

Tab switching (hash + keys 1-4), message anchor copy `URL#msg-N` (`#` appears on hover, click copies and turns into ✓), copy buttons (created by JS on .code-block/.t-code), thread rows jumping to DM, print expanding everything + all views, reduced-motion off.

## 5. dev integration contract (Rust-side generation rules)

- Output `<html lang="en">` + the template's `<style>` block inlined wholesale + the `<script>` block inlined wholesale (JS never concatenates data).
- Full escaping (`& < > " '`); code goes into `<pre>`; images only as `data:` URIs.
- **Message part mapping**:
  - user: `<article class="msg msg-user" id="msg-N">` > `.msg-meta` (`who=You` + time + anchor `#`) + `.bubble` (plain text)
  - assistant: `.msg-assistant` > `.msg-meta` (who=Assistant + model + time + anchor) + `.content`: `.md` (markdown HTML subset) + `details.think` (summary=Thinking · N tokens) + `details.tool` (summary: `.t-icon` svg + `.t-name` + `.t-args` summary + `.t-status.ok|.err|.running` badge; body: `.t-code` (input/result pre))
  - bash A4: non-command fields → `[data-component="tool-args"]` grid (or in v4 the `.t-args` summary + full input pre retained — **final form per the template**; the input pre always carries the complete JSON)
  - Team/DM/Channels structure per §3 and the template sample.
- Markdown subset: p/ul,ol/h1-h6/strong/em/code/pre/table/blockquote/hr/a — styles built in.

## 6. Review method

`share-review.js` v4.0: **43 assertions** (tokens/structure/parts/four views/language/escaping/self-containment/a11y/print/layout contract); the template self-checks **43/43 PASS**; headless Chrome DOM re-verification (copy-button count = .code-block + .t-code count, bubble/tool-card/thread/message counts, tab behavior).

## 7. Changelog

| Date | Version | Notes |
|---|---|---|
| 2026-08-07 | v4.0 | **Claude Code app style** (user-specified, replacing the opencode replica): dark near-black background + centered 800px message flow + user right warm-grey bubbles + assistant left markdown + tool collapsible cards with status badges + grey italic thinking + sticky top bar + four chat-shaped views (Team keeps the thread list); review script 43 items; template MD5 `8c29a17b` |
| 2026-08-07 | v3.1.1 | opencode replica + A4 contract (superseded by v4.0) |
| 2026-08-07 | v3.1/v3.0 | full opencode replica + three chat-record views (superseded by v4.0) |
