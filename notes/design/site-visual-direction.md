# bingo website visual direction (Site Visual Direction)

> Version: v1.1 · Status: draft (aligned with prd-site.md v1)
> Purpose: the visual spec for the bingo website (static site, deployed via dokploy). The website is built directly from this file's token table + component list; this file shares the **same semantic color values** as the share-page design (`share-page-design.md`) (brand orange #D77757/#B05227, semantic green/red/gold/teal).
> **About the share page**: since v2.0 the share page is **light document style** (aligned with the opencode share reference, user-specified) — the website is dark terminal style (the marketing face), the share page is light document style (a reading artifact); the two are deliberately different: terminal heritage belongs to the website and the CLI, document readability belongs to the share page; what's shared is the brand color values and the component mental model (tool rows, status glyphs, collapse interactions), not the surface palette.
> **v1 key constraints (from the PRD)**: pure static, preferably zero JS, no build step; an English main site + a single `/zh/` Chinese summary page; 8 in-site sections; the `/share/` path reserved for future embedding of bingo share artifacts.

## 0. One-line positioning

**The website is bingo's "product manual", not a SaaS marketing page.** Dark-first, monospace as the backbone, restrained highlights — carrying the temperament of the agent workbench in the terminal onto the web, so a visitor's first impression is "this is a tool for people who take engineering seriously".

Hero anchor sentence (PRD §1; by design it's the only candidate for H1):

> **bingo is a local agent CLI written in Rust — the model produces intent, the harness gates every side effect.**

## 1. Brand anchors (immovable)

| Anchor | Value | Notes |
|---|---|---|
| Wordmark | `▸ bingo` | lowercase bingo + accent-orange `▸` prefix (same as the CLI prompt prefix and the share page's brand) |
| Brand orange | `#D77757` | the only "brand moment" color: CTAs, wordmark prefix, hover, selection |
| Background | dark `#0B0B0D` | near-black but not pure black, with a touch of warmth |
| Type | headings and terminal = monospace; body = system sans-serif | monospace carries the character, sans-serif carries long-form readability |
| Graphic language | terminal windows, ASCII/Unicode glyphs (`▸ ∴ ⚙ ✓ ◇`), 1px hairlines, text/SVG architecture diagrams | no illustration, photography, 3D, or icon libraries |
| Language | English main site; `/zh/` single Chinese summary page (excerpting README.zh-CN) | the visual language is identical across both versions; no bilingual-switching UI |

Forbidden: glassmorphism, large gradients, glowing neon, stacked-card shadows, radius bombardment, cartoon mascots, illustration systems.
> v1.4 exemption: the /share/ sample page reuses share-page-template v4.0 (see §4.7/v1.3); its topbar `backdrop-filter: blur(8px)` is part of the Claude Code app style and is the site's only permitted frosted-glass element; the rest of the pages stay forbidden.

## 2. Token table

### 2.1 Palette (CSS variables, same source as the share page)

```css
:root{
  /* surfaces */
  --bg:        #0B0B0D;   /* page background */
  --bg-elev:   #121215;   /* card / terminal-window background */
  --bg-sunken: #0E0E11;   /* hero background (slightly darker than the page) */
  --bg-code:   #141417;   /* code-block / terminal-content background */
  --bg-hover:  #1A1A1E;
  --hairline:  #242428;   /* separators */
  --hairline-strong:#33333A;

  /* text (contrast ratios relative to --bg) */
  --text:  #E8E8E6;   /* 17.6:1 body */
  --dim:   #A3A3A8;   /* ~7.8:1 secondary text */
  --faint: #6F6F76;   /* ~4.2:1 purely decorative only (window chrome, dot grids, separators) — must never carry text */
  --faint-strong: #7A7A80;  /* ~4.6:1 informational small text (feature examples/meta info/footnotes); added v1.2, value finalized after dev implementation checks */
  --ink:   #0B0B0D;   /* text color on accent backgrounds */

  /* semantic (all ≥ 4.5:1) */
  --accent: #D77757;   /* brand orange 6.2:1 */
  --accent-strong: #E8896B;  /* for large type/outlined states (8.0:1) */
  --teal:   #4FB3C7;   /* tools/info */
  --green:  #4EBA65;   /* success */
  --red:    #FF6B80;   /* error */
  --gold:   #FFC107;   /* warning (privacy notices etc.) */
  --periwinkle: #B1B9F9; /* secondary emphasis (mode: free etc. labels) */
  --mauve:  #AF87FF;   /* secondary emphasis */
  --pink:   #FD5DB1;   /* secondary emphasis */
}
```

Usage discipline:
- accent appears ≤ 3 times per screen (CTA included).
- Secondary emphasis is only for **glyphs and labels inside content**, never as large button fills.
- All colors go through `var()`; no light sections — the whole site is dark (if a long-form blog is ever added, light tokens get a separate evaluation; not in v1 scope).

### 2.2 Type

```css
--font-mono: ui-monospace, "SF Mono", "JetBrains Mono", "Cascadia Code",
             Menlo, Consolas, "Liberation Mono", "DejaVu Sans Mono", monospace;
--font-sans: -apple-system, "SF Pro Text", "Segoe UI", "Noto Sans SC",
             "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", sans-serif;
```

| Use | Family | Spec |
|---|---|---|
| Display headings (H1/H2) | mono | 700, `clamp(28px, 5vw, 52px)`, line height 1.15 |
| Section heading H3 | mono | 700, 18px |
| Terminal mockup / code / commands | mono | 13–14px, line height 1.6 |
| Body / feature descriptions | sans | 400, 16px, line height 1.7 |
| Meta info / labels / footer | mono | 12–13px |
| Feature numbers (`01 /`) | mono | accent, 14px |

`/zh/` page: monospace falls back to the system Chinese mono for Chinese (`PingFang SC` etc.); display headings keep the monospace feel; body uses the same sans stack.

### 2.3 Spacing / radius / motion

```css
--s1:8px; --s2:16px; --s3:24px; --s4:40px; --s5:64px; --s6:96px;   /* 8pt grid */
--radius:8px;          /* terminal windows/buttons/inputs */
--radius-sm:4px;       /* chips/labels */
--maxw:1120px;         /* content container */
--ease:cubic-bezier(.2,.6,.2,1);
--dur-1:120ms;  /* hover feedback */
--dur-2:300ms;  /* visibility transitions */
/* section vertical rhythm: hero 96px → each section 96px (halved on mobile) */
```

## 3. Typographic hierarchy (page skeleton, matching the PRD's 8 sections)

```
nav (sticky, hairline bottom, maxw container)
├── ▸ bingo … Features · How it works · Quick start · Docs · Contributing · GitHub [Install]
hero (--bg-sunken, 96px vertical padding)
├── H1 (PRD anchor sentence) + subtitle (sans 18px dim) + install command + dual CTAs
└── terminal demo mockup (flows below, sharing the hero container edge)
Features (8–10 feature grid, hairline checkerboard)
How it works (harness mental-model copy + text/SVG architecture diagram, no JS)
Quick start (3 steps + --print headless example + README link)
Share sample (reserved: #/share/ or a standalone page, v1 placeholder link)
Docs / Contributing (index columns pointing at GitHub assets, no full-text copies)
CTA (centered large heading + accent button)
footer (three columns + copyright + ▸ mark)
```

Every section opens with a mono section label (e.g. `01 / features`), giving a "documentary" feel — the biggest differentiator from competitor marketing pages.

## 4. Component list and specs

### 4.1 nav

- sticky, background `--bg` (95% opacity is enough; **no frosted glass**), 1px hairline bottom; 56px tall.
- Left: wordmark `▸ bingo`; right: section links (Features / How it works / Quick start / Docs / Contributing, sans 14px, hover accent) + the primary CTA "Install" (accent solid).
- Mobile: links fold into a `<details>` collapse menu (**natively collapsible, zero JS**) or hamburger + aria-expanded.
- `scroll-padding-top` aligns anchors; `scroll-behavior: smooth` (reduced-motion → auto).

### 4.2 hero

- Background: plain `--bg-sunken` + **one** low-opacity radial glow (`radial-gradient(60% 50% at 70% 0%, rgba(215,119,87,.08), transparent)`) — this is the only gradient anywhere; nothing else.
- May overlay a 12px monospace dot grid (dual `linear-gradient`, `rgba(255,255,255,.03)`) or stay plain — **pick one**.
- H1: the PRD anchor sentence, large monospace; keyword coloring for **one** of `harness` or `intent` only (accent) — not one more.
- Subtitle: sans 18px `--dim`, a single paragraph ≤ 2 lines (explains the harness mental model, doesn't repeat H1).
- Install command: `$ cargo install --git https://github.com/yexrob/bingo --locked` (mono code block + copy button) + dual CTAs (GitHub / Quick start).
- Bottom small text (optional): `Rust · runs locally · your key never leaves the machine` (mono 12px faint-strong).

### 4.3 Terminal demo mockup (core component, the site's visual centerpiece)

**Spec** (pure HTML/CSS, zero images, zero dependencies; zero-JS first — tabs can use `<details>` or a static two-window side-by-side):

```
┌─────────────────────────────────────────────┐
│ ● ● ●    bingo · share        mode: plan   │  ← window header: dots + title + status
├─────────────────────────────────────────────┤
│ ▸ user   Design the export contract…        │  ← message flow: gutter diamond + line header
│ ∴ thinking · 88 tokens                      │  ← collapsed summary (grey italic)
│ ⚙ Bash · git status   ✓ 0.3s                │  ← tool row (teal + green)
│ ▸ assistant  …list…                         │
│ ▾ code rust (language label)                │
│ ▸ _                                         │  ← blinking cursor (CSS steps, static fallback)
└─────────────────────────────────────────────┘
```

- Window: `--bg-elev` + 1px `--hairline` + `--radius`, no shadow (or one 24px/4% dark soft shadow — pick one).
- Message rows are **isomorphic to the share page's conversation view** (gutter line + diamond + role color + collapsible tool rows) — "what the website shows is how the product looks".
- Cursor: `▸` + block `_`, `steps(1)` 1s loop; static under `prefers-reduced-motion`.
- Multi-view switching (e.g. Team/Channels demos): prefer `<details>` tabs (zero JS); if sliding is wanted, the JS enhancement only handles the `open` mutual exclusion, under 30 lines.
- Mobile: the mockup may scroll horizontally, or degrade to showing only the first 6 lines.

### 4.4 Features (8–10 feature cards)

- Grid: three columns ≥1024px, two columns 640–1023px, single column below; `gap: 1px` + container hairline border (**checkerboard tiling**, not shadow cards).
- Each cell: mono number (`01 /`, accent) + title (sans 600 17px) + description (sans 15px dim, 1–2 lines) + one concrete behavior example (mono 12px faint-strong, e.g. `bingo --print '…'`).
- Candidate cards (listed in PRD §2, pick 8–10): streaming loop · permission gate · tool trait · sub-agents · serial channels · slash commands · hooks · experience reuse · MCP · skills.
- hover: `--bg-hover` transition 120ms; the number color stays.

### 4.5 How it works (harness mental model)

- One paragraph nailing it: **"the model produces intent; the local harness gates every side effect"** — permissions, parallelism, side effects, compaction, memory and UI are all handled by the local harness.
- Figure: **text/SVG schematic** (no JS): `model ⇄ intent ⇄ harness → [permission gate / tools / hooks / memory]`, monospace layout + hairline connectors; SVG inlined and `aria-hidden`, with the text content repeated in the body.
- Forbidden: flowchart illustrations, animated connectors, 3D models.

### 4.6 Quick start

- 3 steps (mono numbers `1 /` `2 /` `3 /`): configure API key → `bingo` → first prompt.
- Plus a `bingo --print '…'` headless example code block (`$` accent prefix + copy button).
- Bottom link "Read the README" (points to GitHub).

### 4.7 Share sample (/share/, P1)

- **Page shape**: standalone `/share/index.html`, **directly using the share-page-design.md template** (`share-page-template.html`, light document style, four views, zero dependencies, offline-ready) — tokens are naturally same-source, satisfying pm acceptance A5 "reuse share-page-design.md tokens".
- **Content**: the template's built-in realistic sample data (conversation + Team roster + DMs + channel activity, real bingo workflows, no fabricated marketing copy); once the CLI team produces a real `bingo share` file, **wholesale-replace** it (overwrite the same-named file; the site layout doesn't change).
- **Integration**: the site nav gains an "Examples" link pointing to `/share/`; the site does not iframe or nest it (the template is a complete HTML document; nesting would break the styles) — standalone page + link is enough.
- **Style relationship**: opening a light document page from within a dark site is an intentional contrast (a product artifact vs the marketing face); no mixing/adaptation.

### 4.8 CTA section

- Centered: mono large heading (≤ 12 words) + one supporting line + a single accent solid large button (padding 12px 28px, e.g. `cargo install bingo`).
- Background same as the hero (plain color + the reusable glow), bookending the page.
- hover: brighten, don't move (no bouncing).

### 4.9 Docs / Contributing (index columns)

- Two columns (or one column above the footer): Docs → GitHub README / README.zh-CN / design notes; Contributing → philosophy (Rust 2024, no unsafe, subtract by default) + the worktree workflow + `cargo build/clippy/test` verification commands.
- Style: link lists (sans 14px dim, hover accent) + one mono note per item.
- **No full-text copies on the site** — index only.

### 4.10 footer

- 1px hairline top; `--bg`.
- Left: wordmark + one sentence (mono 12px faint); middle: link column (GitHub / crates.io / Changelog / License); right: copyright + version.
- Bottom row: `Rust · MIT · zero telemetry` (mono 12px faint-strong, `zero telemetry` may be accent).

### 4.11 Common states

| Component | hover | focus-visible | disabled |
|---|---|---|---|
| Buttons | brightness/border change 120ms | `outline:2px solid var(--accent); offset:2px` | `--faint` + `cursor:not-allowed` |
| Links | accent + bold underline | same | — |
| Copy buttons | appears (opacity 120ms) | same | — |
| Code blocks | none | same | — |

## 5. Motion (all optional enhancements; the core is zero-JS)

| Scenario | Motion | Params | JS? |
|---|---|---|---|
| Anchor scrolling | smooth scroll | CSS, reduced-motion → auto | no |
| Scroll-into-view (optional) | fade + rise | opacity + translateY(12px), 300ms, once only | yes (IntersectionObserver, optional) |
| Mockup tab switching | fade (if done) | 300ms | yes (optional) |
| Cursor blink | steps blink | 1s loop, mockup only | no (CSS) |
| hover | color/border | 120ms | no |

- Without JS: no reveal, no tab switching (use `<details>` or static side-by-side), no scroll effects — **the page is still complete**.
- Rejected: parallax, wide horizontal-scroll images, marquee, button bouncing, character-by-character typewriter, infinite loops.

## 6. Responsive

| Breakpoint | Behavior |
|---|---|
| ≥1024px | three-column features, full mockup, full nav links |
| 640–1023px | two-column features, nav links collapse (details fold), hero type steps down |
| <640px | single column; hero padding 48px; mockup scrolls horizontally or truncates; CTA buttons full width |

## 7. Accessibility

- Contrast: the §2 table all passes (body 17.6:1; faint only for large type and decoration).
- Semantics: `header/nav/main/section/footer`; the mockup is `<pre>`/`<figure>` + `aria-label`, not screenshots standing in for text; if tabs are built use `role=tablist`, or native semantics with `<details>`.
- Keyboard: nav links, copy buttons all reachable; unified focus ring.
- Non-color channels: status glyphs accompany colors; links always underlined.
- Copy: CTAs start with verbs (Install / Read / View); decorative glyphs (`▸ ∴ ⚙`) are `aria-hidden` or carried by CSS pseudo-elements.

## 8. Reference temperaments (temperament references, not copies)

| Reference | What to borrow | What not to learn |
|---|---|---|
| Claude Code website/docs | near-black + restrained terracotta dark palette, the terminal-mockup "product is the page" | glassmorphism buttons and complex light effects |
| Warp website | terminal mockup as the hero's lead, the command area as content | neon gradients |
| Stripe website | typographic hierarchy and whitespace, one focus per screen | illustration systems and fussy motion |
| Ghostty website | plain colors, monospace, a confidence bordering on cool restraint | — |
| This project's share page | isomorphic message rows/collapse/palette | — |

In one sentence: **if a gradient shows up on the site that shouldn't be there, delete it.**

## 9. Do / Don't

**Do**
- Highlights only for: CTAs, the wordmark prefix, role/status glyphs, hover.
- One focus per screen; 96px whitespace between sections instead of divider bars.
- All terminal elements (mockup, code, commands) in monospace; everything else in sans.
- Feature/demo content uses real bingo output (real message flows, real channel seqs), never fabricated flashy fake data.

**Don't**
- No glassmorphism, frosted glass, big gradients, glows, neon, particles.
- No card piles (cards must be joined by 1px grid lines, not floating rounded blocks).
- No meaningless motion; no core content depending on JS.
- No illustration/emoji icons (use font glyphs like `▸ ∴ ⚙ ✓ ● ◐ ◇`).
- No sentences over 12 words crammed into monospace headings (the PRD anchor sentence is the only exception, given generous line height).

## 10. Deliverable suggestions (for the site dev/orchestration reference)

1. This file's token table becomes `tokens.css` (or a single-file `:root`).
2. Component order: nav → hero + terminal mockup → features → how it works → quick start → share placeholder → CTA → footer.
3. The mockup and the share page share the "message row" style mental model; maintain one set of semantic tokens in the repo going forward.
4. Static build: the repo holds static files directly (PRD §4); `public/` maps to the site root; the `/share/` directory is reserved.
5. Zero-JS first: build the complete no-JS version first, then add reveal/tab enhancements on demand (all with fallbacks).

## 11. Changelog

| Date | Version | Notes |
|---|---|---|
| 2026-08-07 | v1.4 | §1 exemption: the /share/ sample page's topbar backdrop-blur (Claude Code app style, built into template v4.0) is the site's only permitted frosted-glass element; the rest of the pages stay forbidden |
| 2026-08-07 | v1.2 | contrast fix: added `--faint-strong` (finalized `#7A7A80`, ~4.6:1 ≥ AA) for informational small text (feature examples/meta info/footnotes, §4.2/§4.4/§4.10 synced); `--faint` narrowed to purely decorative use, must never carry text |
| 2026-08-07 | v1.3 | /share/ sample page promoted to P1: spec clarified = directly reuse share-page-template.html (same tokens, standalone page, wholesale-replaceable) |
| 2026-08-07 | v1.2 | clarified the relationship with the share page v2.0: the website is dark terminal style / the share page is light document style; they share semantic color values, not the surface palette |
| 2026-08-07 | v1.1 | aligned with prd-site.md v1: English main site + /zh/ single page, 8-section structure (How it works / Quick start / Share placeholder / Docs / Contributing), 8–10 feature cards, zero-JS first, /share/ path reserved |
| 2026-08-07 | v1.0 | first draft: token table + component list + reference temperaments |
