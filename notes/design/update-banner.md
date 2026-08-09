# Welcome-card "new version notice" visual spec (Update Banner)

> Version: v1.1 · Status: draft (for dev implementing #53 "version check + welcome-card notice + bingo update command")
> Scope: **the version-update notice line inside the TUI welcome card** — layout, copy, the color-breathing motion spec, the ratatui-implementable approach, and acceptance anchors.
> Related docs: `site-visual-direction.md` (brand colors/contrast discipline), `feedback-states.md` (feedback-state spec/motion discipline — this spec is its "informational notice" shape concretized in the welcome-card scenario), `prd-update.md` (pm's PRD — the visual presentation uses this spec as the single source of truth, see the §6 alignment table).
> v1.1 (user-direction clarification): the effect scope is fixed as **the two keyword segments inside the notice-line text** (the version + `bingo update`) breathing in phase; the rest of the line's text and the rest of the welcome card are fully static.

## 0. One-sentence goal

**A restrained orange notice line appears on the welcome card: "New version vX.Y.Z available — run `bingo update`" — the motion applies only to the two keywords in that line (the version and the command), breathing softly like Claude Code thinking; every other element of the welcome card stays static.**

Design-decision summary (details below):

| Decision point | Conclusion |
|---|---|
| Animation type | **sinusoidal breathing** (color brightness oscillating smoothly between the two brand-orange stops), not hard blinking, not a sweep |
| **Effect scope (v1.1 finalized)** | **only the two keyword segments in the notice line**: the version `vX.Y.Z` and the command `bingo update` (in-phase breathing); the rest of the line's text static dim; the rest of the welcome card (✻ greeting, ╭╮ border, /help, cwd, identity line) **fully static** |
| Position | bottom of the welcome card, directly above the version identity line (the row above `bingo vX.Y.Z · …`) |
| Palette | dark `#D77757 ↔ #E8896B` (≥6.2:1 throughout); light `#B05227 ↔ #9A4A24` (≥4.7:1 throughout) |
| Parameters | period 3.0s (90 frames @30fps) · duration 9s (3 breaths) · then settles at the base color |
| Degradation chain | no truecolor → discrete two-step; `motion: off` / `NO_COLOR` → static (the notice stays, never disappears); user input → stop early |
| Implementation approach | frame-loop per-frame interpolation (reusing the TICK 33ms + `tick()` phase), **never touching the settled scrollback**; ANSI blink codes explicitly not used |

---

## 1. Notice-line layout

### 1.1 Position

The welcome card's current structure (`src/tui/chat.rs` `welcome_card_rows`):

```
╭────────────────────────────────────────╮
│ ✻ Welcome back!                        │
│                                        │
│   /help for help · /status for …       │
│                                        │
│   cwd: /path/to/project                │
│   bingo v0.1.0 · claude · default      │   ← version identity line
╰────────────────────────────────────────╯
```

The new-version notice line is inserted **directly above the version identity line** (same block, no blank between), keeping one blank line from the cwd above it (following the card's existing "one blank line between content groups" rhythm):

```
╭────────────────────────────────────────╮
│ ✻ Welcome back!                        │
│                                        │
│   /help for help · /status for …       │
│                                        │
│   cwd: /path/to/project                │
│                                        │
│   New version v0.3.0 available — …     │   ← breathing line (the only dynamic element)
│   bingo v0.1.0 · claude · default      │   ← version identity line (kept as-is)
╰────────────────────────────────────────╯
```

Rationale:
- the two version lines sit adjacent as an "old vs new" contrast block, read in one glance, the most natural semantics;
- the notice sits at the **bottom** of the card rather than grabbing a spot under the greeting — the card's main body (greeting + help guidance) isn't squeezed, honoring "don't upstage";
- consistent with Claude Code's welcome box putting the update notice at the bottom.

### 1.2 Copy (English interface)

- Full line: `   New version vX.Y.Z available — run bingo update`
- **In-line styling (v1.1, three semantic segments)**:
  - static segments: `New version ` and ` available — run ` → `theme.inactive` (grey, same color as the card's other info lines, not part of the animation);
  - breathing segment ①: `vX.Y.Z` (the version) → the breathing color (§2);
  - breathing segment ②: `bingo update` (the command) → the breathing color + **bold** (the command = an executable action; bold strengthens the action entry; in phase with breathing segment ①).
- The backticks aren't rendered in the terminal (`bingo update`'s code style = breathing color + bold only; don't layer theme.code()'s yellow on top — mixing a third hue into one line would break the breathing's purity).
- `X.Y.Z` comes from the version-check result; **the identity line `bingo v0.1.0` is currently hardcoded** (`welcome_rows`) — implementing #53 should also switch it to `env!("CARGO_PKG_VERSION")`; if the notice and the identity line coexist and the identity line's version is fake, the feature looks fake.
- Non-English: the welcome-card copy is all English today; the notice line stays English; translation is out of this spec's scope.

### 1.3 Width and truncation chain (narrow-screen degradation)

The existing `one_line`/`truncate` do **tail truncation** — they'd cut off the `bingo update` command at the end of the sentence, unusable. The notice line needs a truncation chain that **keeps the command visible** (a new `banner_line(v: &str, width) -> String`, pure and testable):

| Condition (inner_w = card inner width = terminal width − 2) | Presentation |
|---|---|
| `inner_w ≥ 50` | `New version v0.3.0 available — run bingo update` (full; threshold computed on the longest version v0.12.34: 3 indent + 47 text = 50) |
| `inner_w ≥ 43` | `New version v0.3.0 — run bingo update` (drop the "available" clause; 3 + 11 + 8 + 21 = 43) |
| `inner_w ≥ 15` | `bingo update` (command only, the most minimal action entry) |
| `inner_w < 15` (terminal <17 columns, practically impossible) | hide the notice line |

Uniform `   ` prefix (3-column indent, aligned with the card's other info lines); no tier wraps or overflows the card frame (every tier is still a single-line Row).

---

## 2. Color-flash effect spec

### 2.0 Effect scope (v1.1 finalized, user-direction clarification)

**The motion applies only to the two keyword segments in the notice-line text: the version `vX.Y.Z` and the command `bingo update`, both breathing in phase (sinusoidal).** The rest of the line's text (`New version`, ` available — run `) is static `theme.inactive`. Every other element of the welcome card — the ` ✻ ` greeting, the `╭╮` border, the `/help` line, the `cwd` line, the version identity line — is **fully static** and participates in no animation on any frame.

- No whole-card flashing, no synchronized breathing of the card border/greeting;
- **No entrance animation**: when the check result arrives, the notice line appears silently (no flash-in/fade-in); the card growing by one row during startup is absorbed internally, invisible to the user;
- Degradation fallback: if per-segment breathing is too costly to implement, **degrading to whole-line breathing is allowed** (broader scope, parameters and stop points completely unchanged) — the default implementation delivers per-keyword breathing.

### 2.1 Animation type: sinusoidal breathing, not blinking/sweeping

| Candidate | Verdict | Reason |
|---|---|---|
| **Sinusoidal breathing** (smooth brightness oscillation) | ✅ **adopted** | closest to Claude Code thinking's "soft glow"; no hard jumps, naturally non-harsh |
| Hard blink (on/off alternation) | ❌ | alarm semantics (blink only for missing files/crashes); text flashing risks the WCAG 2.3.1 flash threshold |
| Sweep (a light band moving across the text) | ❌ | needs per-character phase offsets; in a TUI it reads as typewriter/marquee, violating "no meaningless motion" |
| ANSI blink code `\e[5m` | ❌ | see §3.4; terminal support is inconsistent and the user can't turn it off |

### 2.2 Palette and stop points (contrast already computed)

The breathing oscillates between two brand-orange stops (trough=rest, peak=strong); **every frame of the whole cycle satisfies contrast**; no intermediate grey, no trough below the deep orange (deep orange `#B05227` on a dark background is only 3.82:1 — even a transient frame isn't worth the low-contrast screenshot risk).

| Theme | rest (settled/trough) | peak | contrast range (against the theme background) |
|---|---|---|---|
| Dark (background ~`#0B0B0D`) | `#D77757` (6.24:1 ✓) | `#E8896B` (7.70:1 ✓) | **≥ 6.24:1 throughout** |
| Light (background ~`#F5F5F5`) | `#B05227` (4.72:1 ✓) | `#9A4A24` (5.70:1 ✓) | **≥ 4.72:1 (AA) throughout** |

> Note: the brand orange `#D77757` is only 2.89:1 on a light background — **the light theme must never use the bright-orange stop**; it must fall entirely into the deep-orange stops (`#B05227`/`#9A4A24`) — same-source as the website token table (`--accent-strong #E8896B`, brand orange `#D77757/#B05227`), taken per background lightness.
> Relationship to `theme.rs` today: the `claude` token stays `#D77757` in both themes (fine for decoration); the notice line **carries text**, so under light it must be deep orange.

### 2.3 Animation parameters (shared between dark/light; only the stop points differ)

| Parameter | Value | Notes |
|---|---|---|
| Frame rate | 30fps (TICK_MS = 33, existing in `app.rs`) | reuse the existing frame loop, no new clock |
| Period | **3.0s = 90 frames** | one "breathe out" + one "breathe in"; below 2s reads anxious, above 5s reads broken |
| Phase function | `t = 0.5 − 0.5·cos(2π · phase/90)`, `phase = tick % 90` | cosine-style: phase 0 → t=0 = rest (starts at the trough, no jump); phase 45 → t=1 = peak; phase 90 → back to rest |
| Color | `color = lerp(rest, peak, t)` (sRGB per-channel linear interpolation), **applied to both the version and `bingo update` segments (in phase)** | 90-frame RGB gradient; no gamma correction (the two stops are close; perceptual difference negligible, noted as a non-goal) |
| Total animation duration | **9s = 270 frames = 3 breaths** | then settles at the rest color, the frame loop returns to idle (the zero-writes invariant resumes) |
| Settled state | the version and command segments settle at the rest color (the command segment keeps bold); the in-line static segments stay inactive | the notice **stays permanently** on the welcome card, it just stops moving |
| Early stop | the first user keypress inside the animation window (input focus moves) → settle immediately | the user's attention has been taken by input; the breathing should stop; optional P1 |

### 2.4 Discrete degradation (no truecolor / 256-color terminals)

256-color has no smooth gradient (`downgrade_to_256` maps RGB to indexed colors), interpolation is meaningless. Degrade to a **two-step discrete breathing** (same architecture, only phase quantization):

| Parameter | Value |
|---|---|
| Period | 2.0s (peak 400ms = 12 frames · rest 1600ms = 48 frames) |
| Stop points | as the table above (using the 256-color approximations: dark `Indexed(173)/…`, uniformly mapped by `downgrade_to_256`) |
| Duration | settles at rest after 9s |
| Key point | peak phase ≥ 400ms, avoiding flicker (< 3Hz, safe); both stops ≥4.5:1 |

### 2.5 Reduced-motion degradation (static display)

The TUI has no `prefers-reduced-motion` (feedback-states §5 mapping-table stance); degradation triggers **automatically** via configuration and terminal capability, inheriting §5's principle "the indicator itself never disappears, it just stops moving":

| Trigger | Behavior |
|---|---|
| Setting `motion: "off"` (new settings.json key, or env `BINGO_NO_MOTION=1`) | static rest color from t=0 (the notice line still shows) |
| Terminal without truecolor | goes through the §2.4 discrete degradation (still "moving", but the minimum tier) |
| Monochrome terminal / `NO_COLOR` present | static **bold** line (bold maintains visibility without color) |
| Tests / CI / non-TTY | static rest (the welcome card isn't rendered outside the TUI anyway; the rule is just determinism) |

The `motion` setting sits at the same layer as `theme` (the `Settings.theme` precedent), default `auto`; this is bingo's first motion switch, serving both "user opt-out" and "test determinism".

---

## 3. TUI implementation constraints and approach (ratatui has no CSS animation)

### 3.1 Current facts (they bound the approach)

- Render model: `Chat::build_rows` produces styled `Row` documents → the view layer maps them to terminal rows. **Styles are decided statically at build time**; `Line/SegStyle` has no time variable — motion can only change colors on each rebuild.
- Frame loop: `app.rs` TICK_MS=33ms (30fps); `chat.tick()` only sets dirty and rebuilds while `has_dynamic_rows()`; at idle there are **zero bytes written** (invariant).
- Spinner precedent: `activities.rs` uses `tick % N` frame numbers to drive character animation — per-frame rebuilds are an existing pattern.
- **The welcome card inside the viewport = a live document row**: the viewport renders the tail of doc.rows, and while the welcome card hasn't scrolled out it redraws with the doc every frame (inline mode's "only redraw the bottom viewport" covers it). **It only settles into scrollback after scrolling out of the viewport, and "never redraw above the viewport"** (app.rs invariant). Persistence is triggered by `pick_flush_mark` when content crosses the window top — the welcome card usually settles after a few messages.

### 3.2 Option A (adopted): frame-interpolated breathing, keeping the welcome card a live row during the animation window

Idea: **constrain the animation to the window where "the welcome card is still in the viewport"** — the 9s window ends well before the normal settle-to-disk moment, so the line settles into the scrollback at the rest color naturally, **never touching scrollback**.

Wiring points (for dev):

```
1. Chat holds UpdateBanner { latest: String, anim_until_tick: u64, phase derived from tick }
2. has_dynamic_rows() gains: || self.update_anim_active()   // frame loop doesn't sleep inside the animation window
3. inside tick(): window expiry → update_anim_active = false (settled state, no more dirty)
4. build_rows → welcome_card_rows(..., update: Option<&str>, phase: u8)
   → banner_line + update_color(theme, is_dark, phase) pure functions
5. on_key (optional P1): first keypress inside the animation window → update_anim_active = false
6. motion:"off" / NO_COLOR → update_color always returns rest / bold, and update_anim_active is always false
```

```
update_color(theme, dark, phase) -> Color:   // the returned Color applies to both the version and bingo update segments (in phase)
  if no truecolor: discrete two-step (phase quantized to 12/48 frames)
  stops = dark ? (rest #D77757, peak #E8896B) : (rest #B05227, peak #9A4A24)
  t = 0.5 − 0.5·cos(2π · phase/90)   // phase 0 = rest (trough), 45 = peak, 90 = rest
  lerp(rest, peak, t)
```

Cost and boundaries:
- a small document rebuild at 9s × 30fps, same order as the spinner's cost during tool runs, and bounded (stops on expiry);
- a resize inside the window rehydrates the welcome card back into the live document → continues within the animation phase, no duplicate animation copies, invariants intact;
- testing: `update_color` is a pure function (phase→Color); the values at phases 0/22/45 can be asserted directly without running the TUI.

### 3.3 Option B (256-color degradation): two-step discrete loop

Same architecture; `update_color`'s no-truecolor branch returns the discrete two steps (§2.4 parameters). Not a separate option — it's the degradation tier of option A; one function implements both.

### 3.4 Option C: ANSI blink code `\e[5m` — explicitly not used

| Dimension | Problem |
|---|---|
| Consistency | terminal support varies (some ignore it, some render it as highlight, tmux/kitty differ) — can't guarantee "every user sees the same", violating feedback-states general principle 1 "feedback must not depend on the environment" |
| Controllability | the user can't turn it off; `motion: off` is unimplementable; no early stop/degradation |
| Semantics | blinking is the terminal's strongest alarm signal (errors/ready notices); too heavy for a version notice, violating "don't upstage" |
| Engineering | `SegStyle` has no blink bit; ratatui `Cell` has no blink modifier — escaping the whole style system to embed raw ESC breaks style composition and the test story |
| Testability | "it blinked" can't be asserted at the unit layer; acceptance would be eyeball-only |

Conclusion: **not used**. Likewise, no per-character phase offsets (sweep) and no whole-card background-gradient breathing (one line of text is enough).

### 3.5 Explicitly not doing (anti-creep)

- no redrawing/rewriting the settled scrollback (invariant kept);
- **no touching any other welcome-card element** (✻ greeting, ╭╮ border, /help, cwd, identity line) — the motion is only the two keyword segments inside this one line;
- no "blink N times then disappear" — the notice **stays** for the welcome card's lifetime (the user needs the entry visible at all times), it just stops breathing;
- no entrance animation (the notice line inserts silently, no flash-in/fade-in);
- no mouse-hover response (no such interaction model).

---

## 4. Theme tokens and code touchpoints

- `Theme` gains 3 tokens (same values as the website's `--accent-strong`/brand deep orange, keeping the project-wide palette same-source):
  - `claude_strong: #E8896B` (dark peak)
  - `claude_deep: #B05227` (light rest)
  - `claude_deep_strong: #9A4A24` (light peak)
  - `downgrade_to_256` maps them too + the existing `rgb_downgrades_to_ansi256` test pattern extends;
- `Theme` must be able to answer "dark or light": suggest adding `is_dark: bool` (set by the `dark()`/`light()` constructors); `update_color` picks the stop points from it (if dev prefers carrying a `ThemeSetting` in the Chat layer, fine — the stop-point values stay the same);
- Pure functions and tests: `banner_line(v, width)` (truncation chain), `update_color(theme, phase)` (phase boundaries/period wrap/settled state/degradation branches) — no runtime dependency, unit-tested directly;
- the version identity line `bingo v0.1.0` hardcode switches to `env!("CARGO_PKG_VERSION")` (incidental fix in the same PR);
- Settings gains `motion: Option<String>` ("auto"/"off", default auto), `BINGO_NO_MOTION=1` equivalent.

---

## 5. Acceptable anchors (qa)

1. **Appearance condition**: a new version detected → the welcome card shows the notice line (copy = `New version {v} available — run bingo update`, three-segment styling: static segments inactive, the version and `bingo update` in the breathing color with the command bold); no new version → the welcome-card layout is line-for-line identical to today (regression).
2. **Breathing correctness**: under truecolor the `update_color(theme, phase)` pure function — phase 0 = rest, phase 45 ≈ peak (±1/255), phase 90 = rest; phases 0→45 monotonically rising, 45→90 monotonically falling (unit-test assertions); **the version segment and the command segment take the same Color at the same phase (in phase)**; the in-line static segments are always `theme.inactive` (unchanged at any phase); the frame loop stays dirty inside the window, returns to idle outside (zero writes).
3. **Window**: settles at the rest color after 9s (270 frames); `needs_tick()` returns false.
4. **Degradation**: `motion: off` / `BINGO_NO_MOTION=1` → static rest throughout, the notice line stays (the indicator never disappears); no truecolor → discrete two-step (peak 400ms/rest 1600ms) without crashing; `NO_COLOR` → static bold.
5. **Early stop** (if implemented): a keypress inside the window → settle immediately.
6. **Narrow screens**: verify the §1.3 truncation chain tier by tier (50/43/15 column boundaries); `bingo update` is visible in every tier (except <17 columns) and never overflows the card frame.
7. **Contrast**: dark ≥6.24:1 per frame, light ≥4.72:1 per frame (settled frame = rest; checkable via screenshot/color picker); the light theme must never show the `#D77757` bright-orange stop.
8. **Scrollback invariant**: after the welcome card settles, it's the static rest color; a resize inside the window → the animation continues after rehydrate, no duplicate animation copies, zero redraws above the viewport (regression on the `flush_items` test family).
9. **Effect scope (v1.1)**: across any two rendered frames, the welcome card's other rows (✻ greeting / ╭╮ border / /help / cwd / identity line) are completely identical (assertable against a static-row snapshot of doc.rows); the notice line appears with no entrance animation (silent insertion, no flash-in).
10. **No ANSI blink**: the output contains no `\e[5m` (grep-assertable).
11. **Identity line**: `bingo v{X.Y.Z}` matches `CARGO_PKG_VERSION` (no more hardcoded v0.1.0).

---

## 6. Alignment with prd-update.md (v1.1)

The visual presentation uses this spec as the **single source of truth** (team convention: PRD group C only accepts, doesn't define); the table below is for pm to sync the C-group anchors and for dev to cross-check during implementation, item by item:

| Item | PRD (pm) | This spec (source of truth) | Ruling |
|---|---|---|---|
| Position | below the `/help` line, above the version line | directly above the version identity line (blank between cwd and the notice; version contrast block) | **consistent** (both above the version line); blank-line rhythm per this spec |
| Copy | `✦ v0.3.0 available — run 'bingo update'` | `New version v0.3.0 available — run bingo update` | **per this spec** (the copy the user-direction clarification referenced; no ✦ prefix, no quotes around the command) |
| In-line styling | version colored | three-segment: static segments inactive + the version/command two segments in the breathing color (command bold) | per this spec |
| Motion scope | C3: only the version colored | the version + `bingo update` two segments breathing in phase | per this spec (a superset of the PRD keyword, equally restrained); **pm must sync C3 to "the version and bingo update two segments in phase"** |
| Appearance | insert/update the line when the result arrives | silent insertion, no entrance animation | per this spec (added detail) |
| Degradation | C4: `NO_COLOR=1`/non-color → static | consistent + `motion: off` / `BINGO_NO_MOTION` | **consistent**; this spec adds an explicit opt-out path |
| After settling | C4: animation stops after flush, static coloring kept | same (static rest color) | **consistent** |

---

## 7. Changelog

| Date | Version | Notes |
|---|---|---|
| 2026-08-07 | v1.1 | effect scope finalized per the user-direction clarification: **only the version and `bingo update` segments inside the notice-line text breathe in phase**, the rest of the line's text static inactive, everything else on the welcome card static; added "no entrance animation"; anchors gain the effect-scope assertion (#9); added the alignment table with prd-update.md (§6, pm must sync C3) |
| 2026-08-07 | v1.0 | first draft: layout/copy/truncation chain, sinusoidal-breathing spec (dark `#D77757↔#E8896B`, light `#B05227↔#9A4A24`, contrast-compliant throughout), ratatui frame-interpolation approach + discrete degradation + ANSI blink code rejected, the motion switch, qa anchors |
