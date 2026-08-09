# opencode share page style reference (extracted from source)

> Source: the sst/opencode repo, `packages/web/src/components/share/` (part.tsx / part.module.css / content-*.tsx).
> Purpose: style-alignment reference for the bingo share page (user-specified). Extracted by importance; code was not copied.

## 1. Overall character

- **Light-first** (Starlight docs-site palette `--sl-color-*`), documentary, minimal, restrained: no bubble piles, no shadows, no gradients, no decorative animation.
- Medium information density: 0.875rem type (body) / 0.75rem (meta info), loose line height, 1rem block spacing.
- Semantic colors reserved for status and emphasis: `green-high` (success/copy done), `blue-high` (assistant message box, thinking box), `red` (error), `text-secondary/dimmed` (secondary text), `hairline` (separators).

## 2. Layout skeleton: left decoration column + width-limited content

Each message part is a horizontal flex: `decoration column | content`.

- **Decoration column** (flex 0 0 auto, ~18px wide):
  - An **anchor icon** at the top (18px; on hover becomes a link icon; click copies "current page URL + #message-id"; on successful copy briefly shows a checkmark + tooltip).
  - A **3px vertical line** below the icon (hairline color, 1px radius) that **runs the full height of the message** — visually stringing the messages into a timeline.
- **Content column** (flex 1):
  - **Width-limited by content type**: `--sm-tool-width` (bash/read/list/glob/grep/write tool results), `--md-tool-width` (body text, thinking, errors), `--lg-tool-width` (edit diff) — content does not span full width; a narrower column reads more focused.
  - 1rem spacing at the bottom of the content (messages are separated by the vertical line + spacing, not by card dividers).

## 3. Message part styles

| Part | Presentation |
|---|---|
| **User text** | Plain text, **no bubble, no border**, follows the left decoration column |
| **Assistant text** | `border: 1px solid blue-high` (thin light-blue frame) + `padding: 0.5rem` + `border-radius: 0.25rem` (4px small radius), 0.875rem type |
| **Thinking (reasoning)** | Same style as assistant text: small card with thin light-blue frame, a "Thinking" title line (secondary color), 0.75rem body; expanded via a "Show details" button |
| **step-start (model-switch marker)** | Provider name in **uppercase + letter-spacing -0.5px** (secondary color) + model name |
| **Tool call (tool)** | Two parts: ① a `tool-title` line = tool name (Bash/Grep/Read/Write/Edit/List/Glob/Fetch/Task…) + target argument (`"pattern"`, file path, command), secondary color, 0.875rem, 18px line height; ② `tool-result` = result block (plain-text preview / code block), with a "Show details" expander |
| **Tool error** | `<pre>` + red `Error:` marker + original text; diagnostics carry a `[line:col]` prefix (dimmed) |
| **Attachment (file)** | Small title line (ATTACHMENT, uppercase secondary color) + file name (500 weight) |
| **bash output** | Monospace, `--sm-tool-width` limit, dark block (code-block component) |
| **todo (todowrite)** | Title line + list, each item with a status color dot (in_progress/pending/completed grouped and sorted) |

## 4. Points transferable to bingo share (suggestion list)

1. **Layout**: message row = left decoration column (anchor icon + running vertical line) + content column; content width-limited by type (sm/md/lg tiers).
2. **Message anchors**: id on every message + click-to-copy link (progressive-enhancement JS; anchor jumps still work without JS).
3. **Assistant messages**: light thin-frame card (1px + 4px radius + 0.5rem padding), no bubble.
4. **User messages**: frameless plain text.
5. **Tool calls**: title line (tool name + target-argument summary) + expandable result block, not a big JSON card plastered across.
6. **Model/role meta info**: small uppercase line (secondary color).
7. **Palette**: light background + hairline separators + a single accent blue/orange + semantic green/red; site-wide `--sl-color-*`-style tokens.
8. **Restraint**: no shadows, no gradients, no card piles; separation via vertical lines, hairline, and whitespace.

## 5. Differences from bingo's current design (v1.1 draft)

| Dimension | Current draft (terminal dark) | opencode reference |
|---|---|---|
| Background | dark #0C0C0E | light doc background |
| Message presentation | line-start diamond marker + gutter vertical line | left decoration column (icon anchor + vertical line) + width-limited content |
| Assistant message | no background | light-blue thin-frame card |
| Tools | details collapsible block (⚙ name·args·status) | two-part title line + result block |
| Type | page-wide monospace | sans-serif body + monospace code |
| Model meta info | none | step-start uppercase line |

> Alignment suggestion: keep bingo's brand color (accent orange can stand in for blue-high as the emphasis color); align layout/information structure/restraint with the opencode reference wholesale; for dark/light theming, default to the reference page's light tone (brand consistency outranks terminal heritage; the terminal heritage lives on in the monospace code blocks and tool rows).
