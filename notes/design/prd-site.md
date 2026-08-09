# PRD: bingo website (project showcase site)

> Status: v1 definition draft
> Related task: push the new site to a public yexrob repository → deploy via dokploy nginx

## 1. Goal

The website is bingo's public face, serving two kinds of visitors:

- **Potential users**: understand "what is bingo, what does it do, how do I install it" within 30 seconds; leave willing to try `cargo install`.
- **Potential contributors**: quickly grasp the project's philosophy and architecture (the harness mental model), find the repository and contribution entry points.

**One-line positioning (Hero anchor)**: bingo is a local agent CLI written in Rust — the model produces intent, the harness gates every side effect.

**Success metric**: visitors can navigate from the homepage to the repo / README / install command, completing the minimal "understand → try" loop.

## 2. Content structure (section list)

In-site navigation (fixed at top, no more than 8 items):

| # | Section | Key points |
|---|---|---|
| 1 | **Hero (above the fold)** | one-line positioning + install command (`cargo install --git https://github.com/yexrob/bingo --locked`) + two CTAs: GitHub repo / quick start |
| 2 | **Features** | 8 feature cards, each with one sentence + one concrete behavior example: streaming main loop, unified permission gate, tool set (Tool trait), sub-agent teams (hub-and-spoke), serial channels, slash commands, Hooks extension points, Experience reuse mechanism, MCP, Skills (pick 8-10 items; restrained cards: icon + title + 1-2 lines) |
| 3 | **How it works (philosophy)** | one paragraph nailing the harness mental model: "the model only produces intent; permissions, parallelism, side effects, compaction, memory and UI are handled by the local harness" + a simplified architecture diagram (text/SVG, no JS) |
| 4 | **Quick start** | 3 steps: configure an API key → launch `bingo` → first instruction; plus a `--print` headless example; link to the full README |
| 5 | **Share sample** (`/share/`) | showcase the session HTML produced by `bingo share` (single file, offline-ready, naturally embeddable in a static site). v1 uses a **realistic session sample** (with sub-agent/channel activity, hand-built or excerpted from a real session), honestly labeled as a placeholder; can be wholesale-replaced once the CLI team produces real `bingo share` output |
| 6 | **Docs / documentation entry** | point to the GitHub README, README.zh-CN, and design docs (error-code contract, feedback-states); do not duplicate full text on the site — the static site only indexes |
| 7 | **Contributing** | project philosophy (Rust 2024, no unsafe, subtract by default) + the worktree workflow + verification commands (build/clippy/test) + commit conventions; link to the repo's CONTRIBUTING or AGENTS.md |
| 8 | **Footer** | repo link, License (MIT? per the repo), acknowledgements/references (goose, iocraft, etc.) |

## 3. Copy tone

**Suggestion: an English main site + a Chinese excerpt page (`/zh/` or a single-page Chinese summary)**, not site-wide bilingualism.

Rationale:

1. **Target audience**: a GitHub open-source project — potential users and contributors are mostly in the English ecosystem; the README body is already English, so an English site stays consistent with the repo at the lowest maintenance cost.
2. **Existing assets**: README.zh-CN.md already exists and is high quality — the Chinese excerpt page can directly excerpt/translate/link that file instead of reinventing it.
3. **The Chinese community is a real secondary audience** (the docs have a Chinese version, issues have Chinese discussion), so offering a Chinese excerpt is a low-cost, high-goodwill signal.
4. **Pure-static constraint**: site-wide bilingualism needs a language-switching mechanism (URL prefix or JS toggle); v1 subtracts — English as the main site, a single `/zh/` page with a Chinese summary + link to the Chinese README.

Copy style: short sentences, concrete, no marketing-speak; technical terms are not translated (harness, permission gate, sub-agent).

## 4. Technical constraints

- **Pure static**: HTML + CSS + minimal vanilla JS (optional, preferably zero JS); no build step, no framework, no package manager — static files live directly in the repo, edit and deploy.
- **Deployment**: dokploy nginx static serving; the `public/` directory (or repo root) maps directly to the site root.
- **Responsive**: readable on mobile (users of a terminal tool may read the README on their phone).
- **Extensible**: share samples (single HTML files) drop straight into the site's `/share/` directory as standalone pages (main's ruling: v1 uses realistic samples, replaceable by CLI output); the site structure already reserves the `/share/` path.
- **No external dependencies**: no CDN fonts/libraries (works offline, loads fast, no tracking). Inline SVG for icons.
- **Visual implementation**: dev implements strictly per `site-visual-direction.md` (token table + component list); the `/share/` sample page shares the same source and tokens as the CLI share page (see `share-page-design.md` / `share-page-template.html`).

## 5. Acceptance criteria

### A. Content completeness
- A1. All eight sections implemented; Hero carries the install command and two CTAs.
- A2. Feature cards cover the product shape: streaming main loop, Tool protocol, unified permission gate, Hooks, sub-agents (hub-and-spoke), serial channels, slash commands, error-code contract, Experience mechanism — each with one sentence + one behavior example (distilled from the README, not copied).
- A3. "How it works" contains a one-sentence mental model and a simplified architecture diagram.
- A4. The three Quick start steps are independently completable; all links (repo, README, README.zh-CN) resolve (click each one during acceptance).
- A5. The `/share/` sample page exists: it shows the four views (conversation/sub-agents/channels/tasks) or at least conversation + sub-agent + channel activity, with realistic content (no fabricated flashy fake data); the page structure can be wholesale-replaced by a real `bingo share` product without changing the site layout.

### B. Responsive
- B1. At 375px (phone) and 1440px (desktop): no horizontal scroll, no text overflow, CTAs clickable.
- B2. Navigation collapses to a usable menu form on narrow screens (or simplifies to a single-column stack).

### C. Loading
- C1. No external resource requests (the page opens fully readable offline except font icons); CSS/inline SVG embedded or same-domain.
- C2. No render-blocking resources above the fold (Hero); restrained total page size (ideally < 200KB of text content).

### D. SEO basics
- D1. `<title>` + `<meta description>` + Open Graph (og:title/og:description/og:image may be omitted or placeholders).
- D2. Semantic HTML (`<header>/<main>/<nav>/<h1-h3>/<footer>`), body text as real text (not images).
- D3. `robots.txt` and `sitemap.xml` (add once the domain is finalized, containing the site URL).

### E. Deployment
- E1. After dokploy deployment: `bingo.ruobin.dev` reachable, HTTP 200, static assets load same-domain, the 404 page is not ugly (nginx default acceptable; write our own in P2).
- E2. Push the repo to a public yexrob repository (separate from the main bingo repo; the site is its own repo).
- E3. **License (main's ruling 2026-08-07)**: License = MIT — the site footer marks MIT; the site repo contains a LICENSE file (full MIT text); the main repo's LICENSE is handled by the CLI side. Must be added before pushing.
- E4. **Deployment domain (main's ruling)**: `bingo.ruobin.dev` (already resolved to dokploy; changeable later); the placeholder domain `bingo.example.com` in sitemap.xml / OG is replaced with the official domain before deployment.

### F. Quality
- F1. The site repo has a README stating "this is the website; content lives at X; deployment goes through dokploy nginx".
- F2. HTML passes basic validation (w3c validator with no error-level issues, or a manual spot-check for unclosed tags).

## 6. Priorities

| Priority | Content |
|---|---|
| **P0** (must for v1) | Hero, Features (≥8 cards), How it works, Quick start, Footer, navigation; responsive basics; SEO title/description/OG; no external resources; push to a public repo; dokploy deployment reachable |
| **P1** (immediately after) | Contributing section, Docs index (linking the EN/CN README), `/share/` sample page (realistic session sample with sub-agent/channel activity), `/zh/` Chinese excerpt page, sitemap/robots, 404 page |
| **P2** (only when there's a real need) | replace `/share/` samples with real `bingo share` CLI output, theme switching, blog/changelog, more languages |

## 7. Dependencies and order

1. Confirm the domain/deployment target (create the site on dokploy → get the URL) → 2. content and pages (P0 first) → 3. push to a public repo → 4. dokploy deployment + acceptance (groups A/E) → 5. P1 completion.

## 8. Risks and open items

- **Domain settled**: `bingo.ruobin.dev` (main's ruling 2026-08-07, already resolved to dokploy); the sitemap/OG placeholder domain is replaced before deployment.
- **License settled**: MIT (main's ruling 2026-08-07); add the LICENSE file + MIT footer before pushing the site repo.
- **Site and repo separated**: the site is its own repo (e.g. `yexrob/bingo-site`), so site changes never pollute the main repo's commit history.
