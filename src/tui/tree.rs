//! The agent tree (D104): the status layer, drawn around the composer.
//!
//! Claude Code's answer to "many agents, one terminal" is not a switcher — it
//! is a persistent tree of who is working, rebuilt from live state every frame
//! and parked in the chrome above the prompt. This is that tree, in bingo's
//! vocabulary: `TeammateSpinnerTree.tsx` for the container and its leader and
//! hide rows, `TeammateSpinnerLine.tsx` for an instance's row, its status copy,
//! its stats and its message preview.
//!
//! **Chrome, never scrollback.** Every row here is redrawn each frame and none
//! of it is ever settled — the write-once doctrine in [`crate::tui::term`] is
//! what makes that a rule rather than a habit. The tree is built from
//! [`crate::agents::AgentRegistry::list`] at draw time, the way the dialog
//! is built from live sources, so nothing here can go stale.
//!
//! **The index space is CC's** (`useBackgroundTaskNavigation.ts:26-58`): `-1`
//! is `@main`, `0..n-1` are the instances in the order they are drawn, and `n`
//! is the hide row. Selection wraps at both ends, and it is *explicit state* —
//! with nothing selected the tree is a readout and every key still belongs to
//! the composer. Only a selected row makes `k` mean stop.
//!
//! **Enter zooms** (D105): an instance row opens
//! [`crate::tui::zoom`] over that agent, the `@main` row leaves a zoom that is
//! open, and the hide row closes the panel — CC's three answers in CC's order
//! (`useBackgroundTaskNavigation.ts:206-225`). The two hint strings D104 held
//! back because the key did nothing are back with it.

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::style::Color;

use crate::agents::{AgentState, AgentStatus};
use crate::api::types::ContentBlock;
use crate::channels::MAIN_NAME;
use crate::tui::avatar::{Gutter, Palette};
use crate::tui::chat::{Chat, Row, one_line};
use crate::tui::line::{Line, SegStyle, text_width};

/// Where the cursor is, in CC's index space: `-1` = `@main`, `0..n-1` = the
/// instances in draw order, `n` = the hide row.
pub type Sel = isize;

/// `@main`'s index. Not a registry entry — main is the host turn — but it is
/// the first row and the first thing `shift+↓` parks on, exactly as CC's
/// leader row is (`useBackgroundTaskNavigation.ts:33-41`).
pub const MAIN_SEL: Sel = -1;

/// The open tree. One field: what is selected, or nothing.
///
/// `None` is not "row zero" — it is *not selecting*, which is the state the
/// tree is in when `ctrl+t` opened it. The distinction is the whole reason
/// plain typing is never stolen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentTree {
    pub selected: Option<Sel>,
}

/// The narrowest activity column a row will fold down to before the optional
/// segments start dropping (CC `TeammateSpinnerLine.tsx:141`).
const MIN_ACTIVITY: usize = 25;

/// CC's own slack in the fit arithmetic (`TeammateSpinnerLine.tsx:151-153`).
const FIT_SLACK: usize = 5;

/// `shift + ↑/↓ to select` — `components/Spinner/teammateSelectHint.ts:1`,
/// verbatim including the spaces around the `+`.
const SELECT_HINT: &str = " · shift + ↑/↓ to select";

/// The pills' own affordance (`BackgroundTaskStatus.tsx`, the tail after the
/// last pill). Same wording, one arrow.
const EXPAND_HINT: &str = " · shift + ↓ to expand";

/// ` · enter to view` — `TeammateSpinnerLine.tsx:134`, and the leader row's own
/// copy of it at `TeammateSpinnerTree.tsx:128`. Shown on the **selected** row
/// and only while that row is not the one already on screen
/// (`isSelected && !isForegrounded`), so the key is never advertised where it
/// would do nothing. D104 left it out because Enter was unbound; D105 binds it.
const VIEW_HINT: &str = " · enter to view";

/// ` · enter to collapse` — `TeammateSpinnerTree.tsx:253`, on the hide row while
/// the cursor is on it.
const COLLAPSE_HINT: &str = " · enter to collapse";

/// Preview lines per instance, and the width one is cut to
/// (`TeammateSpinnerLine.tsx:32`, `:43`).
const PREVIEW_LINES: usize = 3;
const PREVIEW_WIDTH: usize = 80;

/// Rows indent three columns, then spend one on the cursor, two on the tree
/// glyph and one on the space after it (`<Box paddingLeft={3}>`).
const PAD: &str = "   ";

/// Push at most `budget` cells of `text` and spend what it took. Every segment
/// of every row goes through here, so a row cannot overrun the canvas however
/// the fit arithmetic above it turns out.
fn push_fit(line: &mut Line, budget: &mut usize, text: &str, style: SegStyle) {
    if *budget == 0 || text.is_empty() {
        return;
    }
    let fitted = one_line(text, *budget);
    *budget = budget.saturating_sub(text_width(&fitted));
    line.push_styled(fitted, style);
}

/// `14s` · `2m 5s` · `1h 2m 3s` — CC `utils/format.ts:34-70` without the
/// sub-second decimal, which no row here can show (the tick is one second).
pub fn duration_label(elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        return format!("{secs}s");
    }
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else {
        format!("{m}m {s}s")
    }
}

/// ` · 12 tool uses · 8.3k tokens` — CC `TeammateSpinnerLine.tsx:130`. The
/// word `tool` never pluralizes; `use` does.
///
/// **Empty when the run has produced nothing.** CC prints the segment
/// unconditionally on an instance row, but its progress is the teammate's
/// whole life; bingo's is the *current run*, so an idle instance would carry
/// `0 tool uses · 0 tokens` on every frame. CC's own leader row gates on
/// exactly this (`TeammateSpinnerTree.tsx:111`), so the gate is CC's, applied
/// one row further down.
pub fn stats_label(tool_uses: usize, tokens: u64) -> String {
    if tool_uses == 0 && tokens == 0 {
        return String::new();
    }
    format!(" · {}", stats_body(tool_uses, tokens))
}

/// `12 tool uses · 8.3k tokens` — the segment itself, ungated. The tree hangs
/// it off a row with a leading ` · ` and drops it at zero; D106's `Done (…)`
/// and `In progress…` lines print it whatever it says, because CC's own
/// completion line does (`tools/AgentTool/UI.tsx:376`). One formatter, two
/// gates.
pub fn stats_body(tool_uses: usize, tokens: u64) -> String {
    let uses = if tool_uses == 1 { "use" } else { "uses" };
    format!(
        "{tool_uses} tool {uses} · {} tokens",
        crate::context_usage::compact_tokens(tokens, 1_000)
    )
}

/// What an instance's row says it is doing — the tree's status column, and
/// the background dialog's (D107), so one ladder answers on both surfaces.
///
/// Priority is CC's (`TeammateSpinnerLine.tsx:171-194`): the stopping state
/// first, then idleness, then the activity. Two of CC's arms have no bingo
/// analogue and are not invented here — `[awaiting approval]` belongs to the
/// plan-approval protocol v4 explicitly does not copy, and the all-idle
/// past-tense verb (`Brewed for 2m 5s`) belongs to the teammate idle loop.
pub(crate) fn status_label(status: &AgentStatus, now: std::time::Instant) -> String {
    match status.state {
        // CC's `[stopping]` slot. bingo's stop is synchronous, so the state
        // this row can be in is *stopped*, not stopping — and a stopped
        // instance stays on the roster because the registry keeps it: its
        // history is intact, its record is still readable, and a direct
        // message resumes it from that history (CC subagent semantics —
        // `AgentRegistry::deliver` flips it back to idle and the delivery
        // flush respawns the run).
        AgentState::Stopped => "[stopped]".to_string(),
        AgentState::Idle => format!(
            "Idle for {}",
            duration_label(now.saturating_duration_since(status.last_active))
        ),
        AgentState::Running => {
            let activity = status
                .recent_activity
                .last()
                .map(String::as_str)
                .unwrap_or("initializing");
            if activity.ends_with('…') {
                activity.to_string()
            } else {
                format!("{activity}…")
            }
        }
    }
}

/// The last [`PREVIEW_LINES`] lines of an instance's conversation, oldest of
/// the three first — a port of CC's `getMessagePreview`
/// (`TeammateSpinnerLine.tsx:26-70`) over bingo's message store.
///
/// **The record, not the activity feed.** CC previews the teammate's
/// *messages*, walking them newest-first and taking text lines from the end of
/// each block and a tool call's own description; `recent_activity` is the same
/// information a row already carries in its activity column, so sourcing the
/// preview from it would have printed the row three more times.
pub fn preview_lines(history: &[crate::api::types::Message]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for message in history.iter().rev() {
        if out.len() >= PREVIEW_LINES {
            break;
        }
        for block in &message.content {
            if out.len() >= PREVIEW_LINES {
                break;
            }
            match block {
                ContentBlock::ToolUse { name, input, .. } => {
                    let described = ["description", "prompt", "command", "query", "pattern"]
                        .iter()
                        .find_map(|key| input.get(key).and_then(|v| v.as_str()))
                        .and_then(|text| text.lines().next())
                        .filter(|line| !line.trim().is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("Using {name}…"));
                    out.push(one_line(&described, PREVIEW_WIDTH));
                }
                ContentBlock::Text { text } => {
                    for line in text.lines().rev() {
                        if out.len() >= PREVIEW_LINES {
                            break;
                        }
                        if line.trim().is_empty() {
                            continue;
                        }
                        out.push(one_line(line, PREVIEW_WIDTH));
                    }
                }
                _ => {}
            }
        }
    }
    out.reverse();
    out
}

impl Chat {
    /// Everyone the tree lists, in the order it lists them. The registry sorts
    /// by name, which is CC's order too (`getRunningTeammatesSorted`).
    ///
    /// Stopped instances are included, where CC drops a killed task. bingo's
    /// registry keeps a stopped instance and the composer can still reach it,
    /// so a roster that hid it would disagree with the `@name` typeahead one
    /// row below.
    pub(crate) fn tree_instances(&self) -> Vec<AgentStatus> {
        self.session.agents.list()
    }

    /// One stable colour per name, main's reserved slot included — the palette
    /// the avatar gutter draws from, so a name and its face never disagree.
    /// The pills, the tree's rows and D106's `@name❯` lines all ask here.
    pub(crate) fn identity_color(&self, name: &str) -> Color {
        let palette = Palette::new(&self.theme);
        let gutter = Gutter::new(false, &palette, &self.faces_pinned);
        palette.avatars[gutter.index_for(name) % palette.avatars.len()]
    }

    /// Open the tree with nothing selected — what `ctrl+t` does. The panels of
    /// the cycle are exclusive, so the task area closes with the same press.
    pub(crate) fn open_agent_tree(&mut self) {
        self.tasks_visible = false;
        self.tasks_auto = false;
        self.tree = Some(AgentTree::default());
        self.dirty = true;
    }

    /// The selection, clamped to what is on screen right now. Instances come
    /// and go under an open tree, and CC clamps the same way in an effect
    /// (`useBackgroundTaskNavigation.ts:112-124`).
    pub(crate) fn tree_selection(&self) -> Option<Sel> {
        let max = self.tree_instances().len() as Sel;
        self.tree
            .as_ref()
            .and_then(|tree| tree.selected)
            .map(|sel| sel.clamp(MAIN_SEL, max))
    }

    /// `shift+↑` / `shift+↓`. With the tree closed the first press opens it and
    /// parks on `@main`; with it open the cursor steps and wraps
    /// (`useBackgroundTaskNavigation.ts:26-58`).
    fn tree_step(&mut self, delta: Sel) {
        let count = self.tree_instances().len() as Sel;
        if count == 0 {
            return;
        }
        self.dirty = true;
        let Some(tree) = self.tree.as_mut() else {
            self.tasks_visible = false;
            self.tasks_auto = false;
            self.tree = Some(AgentTree {
                selected: Some(MAIN_SEL),
            });
            return;
        };
        let current = tree.selected.unwrap_or(MAIN_SEL).clamp(MAIN_SEL, count);
        tree.selected = Some(if delta > 0 {
            if current >= count {
                MAIN_SEL
            } else {
                current + 1
            }
        } else if current <= MAIN_SEL {
            count
        } else {
            current - 1
        });
    }

    /// Keys the tree claims. Everything else falls through to the composer,
    /// which is the point: the tree is a readout, not a modal.
    ///
    /// A permission dialog keeps priority (D81's rule): a panel
    /// must not move under a question that is holding up a turn.
    pub(crate) fn agent_tree_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if self.pending_ask.is_some() {
            return false;
        }
        // With nobody on the roster the chord is left alone rather than
        // swallowed: CC has nothing else bound to it, bingo's arrows are the
        // composer's, and a key that visibly does nothing is worse than a key
        // that does what it always did.
        if modifiers.contains(KeyModifiers::SHIFT)
            && matches!(code, KeyCode::Up | KeyCode::Down)
            && !self.tree_instances().is_empty()
        {
            self.tree_step(if code == KeyCode::Down { 1 } else { -1 });
            return true;
        }
        // `enter` on a selected row (D105). It is CC's whole handler, gated on
        // CC's own condition (`useBackgroundTaskNavigation.ts:206`): with
        // nothing selected the key belongs to the composer, which is what makes
        // a tree safe to leave open while typing.
        if code == KeyCode::Enter
            && !modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
            && self.tree_enter()
        {
            return true;
        }
        // `k` stops the selected instance — CC's key, CC's guard: only in
        // selection mode, only on an instance row (`@main` is index -1 and the
        // hide row indexes past the end), and only through the one stop path
        // every other surface uses, so there is one warning and one watch
        // transition. No confirmation, which is CC's ruling too
        // (`useBackgroundTaskNavigation.ts:228-241`).
        if code == KeyCode::Char('k') && !modifiers.intersects(KeyModifiers::CONTROL) {
            let Some(selected) = self.tree_selection().filter(|sel| *sel >= 0) else {
                return false;
            };
            let agents = self.tree_instances();
            let Some(status) = agents.get(selected as usize) else {
                return false;
            };
            if status.state == AgentState::Stopped {
                return true;
            }
            let name = status.name.clone();
            self.stop_agent(&name);
            return true;
        }
        false
    }

    /// The tree's rows, rebuilt from the registry. Empty when the tree is
    /// closed, and empty when nobody has ever been spawned — CC returns `null`
    /// for the same reason (`TeammateSpinnerTree.tsx:45-47`).
    pub fn agent_tree_rows(&self, width: usize) -> Vec<Row> {
        if self.tree.is_none() {
            return Vec::new();
        }
        let agents = self.tree_instances();
        if agents.is_empty() {
            return Vec::new();
        }
        let selected = self.tree_selection();
        let selecting = selected.is_some();
        let theme = &self.theme;
        let palette = Palette::new(theme);
        let gutter = Gutter::new(false, &palette, &self.faces_pinned);
        let identity = |name: &str| -> Color {
            palette.avatars[gutter.index_for(name) % palette.avatars.len()]
        };
        let dim = SegStyle::fg(theme.text_secondary);
        let now = std::time::Instant::now();
        let mut rows = Vec::new();

        // `@main` leads, and it is not a registry entry: its presence is the
        // host turn. CC's leader row shows its verb and its idle text only
        // while something else has the screen, because its own spinner row
        // says so otherwise (`Spinner.tsx:231-237`); bingo has no such row when
        // idle, so main's state lives here unconditionally.
        let main_selected = selected == Some(MAIN_SEL);
        // Main is lit while it is what the screen is on. A room zoom answers
        // `zoomed()` with `None` — no row on this tree is its subject — so the
        // test is the zoom itself, not the agent behind it.
        let main_lit = main_selected || self.zoom_subject().is_none();
        let mut budget = width;
        let mut line = Line::empty();
        push_fit(&mut line, &mut budget, PAD, dim);
        push_fit(
            &mut line,
            &mut budget,
            if main_selected { "❯" } else { " " },
            SegStyle::fg(if main_selected {
                theme.permission
            } else {
                theme.text_secondary
            }),
        );
        push_fit(
            &mut line,
            &mut budget,
            if main_lit { "╒═ " } else { "┌─ " },
            if main_lit {
                SegStyle::fg(theme.text).bold()
            } else {
                dim
            },
        );
        let main_style = SegStyle::fg(if main_selected {
            theme.permission
        } else {
            identity(MAIN_NAME)
        });
        push_fit(
            &mut line,
            &mut budget,
            &format!("@{MAIN_NAME}"),
            if main_lit {
                main_style.bold()
            } else {
                main_style
            },
        );
        let running = self.running_status();
        let main_state = match &running {
            Some(status) => format!(": {}…", status.verb),
            None => ": Idle".to_string(),
        };
        push_fit(&mut line, &mut budget, &main_state, dim);
        let main_tokens = running.as_ref().map_or(0, |status| status.tokens);
        if main_tokens > 0 {
            push_fit(
                &mut line,
                &mut budget,
                &format!(
                    " · {} tokens",
                    crate::context_usage::compact_tokens(main_tokens, 1_000)
                ),
                dim,
            );
        }
        if main_lit {
            push_fit(&mut line, &mut budget, SELECT_HINT, dim);
        }
        // The leader row offers the view only while something else has the
        // screen — going to `@main` from `@main` is not a destination.
        if main_selected && self.zoomed().is_some() {
            push_fit(&mut line, &mut budget, VIEW_HINT, dim);
        }
        rows.push(Row::new(line));

        for (index, status) in agents.iter().enumerate() {
            let here = selected == Some(index as Sel);
            let lit = here || self.zoomed() == Some(status.name.as_str());
            // The closing corner belongs to the hide row while the tree is
            // being walked, so no instance takes it (`TeammateSpinnerTree.tsx:149`).
            let last = !selecting && index + 1 == agents.len();
            let glyph = match (lit, last) {
                (true, true) => "╘═ ",
                (true, false) => "╞═ ",
                (false, true) => "└─ ",
                (false, false) => "├─ ",
            };
            let name = format!("@{}", status.name);
            let state = status_label(status, now);
            let stats = stats_label(status.tool_uses, status.output_tokens);

            // CC's fit arithmetic, and CC's drop order: the view hint goes
            // first, then the select hint, then the stats
            // (`TeammateSpinnerLine.tsx:151-153`).
            let head = text_width(PAD) + 1 + 3 + text_width(&name) + 2;
            let room = width.saturating_sub(head);
            let show_stats = room > text_width(&stats) + MIN_ACTIVITY + FIT_SLACK;
            let stats_cost = if show_stats { text_width(&stats) } else { 0 };
            // `isSelected && !isForegrounded`: the cursor is here, and here is
            // not what is already on screen.
            let show_view = here
                && self.zoomed() != Some(status.name.as_str())
                && room > text_width(VIEW_HINT) + stats_cost + MIN_ACTIVITY + FIT_SLACK;
            let view_cost = if show_view { text_width(VIEW_HINT) } else { 0 };
            let show_hint = lit
                && room
                    > text_width(SELECT_HINT) + view_cost + stats_cost + MIN_ACTIVITY + FIT_SLACK;
            let spent = stats_cost
                + view_cost
                + if show_hint {
                    text_width(SELECT_HINT)
                } else {
                    0
                };
            let state_room = room.saturating_sub(spent + 1).max(MIN_ACTIVITY);

            let mut budget = width;
            let mut line = Line::empty();
            push_fit(&mut line, &mut budget, PAD, dim);
            push_fit(
                &mut line,
                &mut budget,
                if here { "❯" } else { " " },
                SegStyle::fg(if here {
                    theme.permission
                } else {
                    theme.text_secondary
                }),
            );
            push_fit(
                &mut line,
                &mut budget,
                glyph,
                if lit { SegStyle::fg(theme.text) } else { dim },
            );
            push_fit(
                &mut line,
                &mut budget,
                &name,
                SegStyle::fg(if here {
                    theme.permission
                } else {
                    identity(&status.name)
                }),
            );
            push_fit(&mut line, &mut budget, ": ", dim);
            push_fit(&mut line, &mut budget, &one_line(&state, state_room), dim);
            if show_stats {
                push_fit(&mut line, &mut budget, &stats, dim);
            }
            if show_hint {
                push_fit(&mut line, &mut budget, SELECT_HINT, dim);
            }
            if show_view {
                push_fit(&mut line, &mut budget, VIEW_HINT, dim);
            }
            rows.push(Row::new(line));

            if self.tree_preview {
                let stem = if last { "    " } else { " │  " };
                for text in self.instance_preview(&status.name) {
                    let mut budget = width;
                    let mut line = Line::empty();
                    push_fit(&mut line, &mut budget, PAD, dim);
                    push_fit(&mut line, &mut budget, stem, dim);
                    push_fit(&mut line, &mut budget, " ", dim);
                    push_fit(&mut line, &mut budget, &text, dim);
                    rows.push(Row::new(line));
                }
            }
        }

        // The hide row closes the tree, and only while the cursor is in it
        // (`TeammateSpinnerTree.tsx:180`); it says what Enter will do to it
        // while the cursor is on it (`:253`).
        if selecting {
            let here = selected == Some(agents.len() as Sel);
            let mut budget = width;
            let mut line = Line::empty();
            push_fit(&mut line, &mut budget, PAD, dim);
            push_fit(
                &mut line,
                &mut budget,
                if here { "❯" } else { " " },
                SegStyle::fg(if here {
                    theme.permission
                } else {
                    theme.text_secondary
                }),
            );
            push_fit(
                &mut line,
                &mut budget,
                if here { "╘═ " } else { "└─ " },
                if here {
                    SegStyle::fg(theme.text).bold()
                } else {
                    dim
                },
            );
            push_fit(
                &mut line,
                &mut budget,
                "hide",
                if here {
                    SegStyle::fg(theme.text).bold()
                } else {
                    dim
                },
            );
            if here {
                push_fit(&mut line, &mut budget, COLLAPSE_HINT, dim);
            }
            rows.push(Row::new(line));
        }
        rows
    }

    /// The three lines the preview shows for one instance, from its own record.
    fn instance_preview(&self, name: &str) -> Vec<String> {
        let Some((history, ..)) = self.session.agents.view_of(name) else {
            return Vec::new();
        };
        preview_lines(&history)
    }

    /// The footer pills: `@main @scout @writer · shift + ↓ to expand`.
    ///
    /// Off while the tree is open — CC makes them exclusive, because the tree
    /// says everything a pill would (`BackgroundTaskStatus.tsx`, the
    /// `showSpinnerTree` gate) — and off with an empty registry, because a row
    /// reading `@main` alone is furniture.
    ///
    /// `@main` is always first and never sorted; the instances follow with the
    /// idle and stopped ones pushed to the end, which is CC's ordering when
    /// nothing is being arrowed through.
    pub fn pill_row(&self, width: usize) -> Option<Row> {
        if self.tree.is_some() {
            return None;
        }
        let agents = self.tree_instances();
        if agents.is_empty() {
            return None;
        }
        let theme = &self.theme;
        let dim = SegStyle::fg(theme.text_secondary);

        let mut pills: Vec<(String, bool, bool)> =
            vec![(MAIN_NAME.to_string(), self.busy, self.zoomed().is_none())];
        let mut resting: Vec<(String, bool, bool)> = Vec::new();
        for status in &agents {
            let zoomed = self.zoomed() == Some(status.name.as_str());
            let pill = (
                status.name.clone(),
                status.state == AgentState::Running,
                zoomed,
            );
            if pill.1 {
                pills.push(pill);
            } else {
                resting.push(pill);
            }
        }
        pills.append(&mut resting);

        let mut budget = width.saturating_sub(text_width(EXPAND_HINT) + 2);
        let mut line = Line::styled("  ", dim);
        let mut shown = 0usize;
        for (name, live, zoomed) in &pills {
            let text = if shown == 0 {
                format!("@{name}")
            } else {
                format!(" @{name}")
            };
            if text_width(&text) > budget {
                break;
            }
            budget -= text_width(&text);
            let mut style = SegStyle::fg(if *live {
                self.identity_color(name)
            } else {
                theme.text_secondary
            });
            if *zoomed {
                style = style.bold();
            }
            line.push_styled(text, style);
            shown += 1;
        }
        if shown < pills.len() {
            line.push_styled(" →".to_string(), dim);
        }
        line.push_styled(EXPAND_HINT.to_string(), dim);
        Some(Row::new(line))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentKind, AgentProgress};
    use crate::api::types::{Message, Role};
    use crate::tui::test_util::chat_at;

    fn test_chat() -> Chat {
        chat_at(120, 40)
    }

    fn seed(chat: &Chat, name: &str) {
        chat.session.agents.insert(
            name,
            AgentKind::Crew,
            None,
            "test instance".to_string(),
            chat.session.clone(),
        );
    }

    fn texts(chat: &Chat) -> Vec<String> {
        chat.agent_tree_rows(120)
            .iter()
            .map(|row| row.line.plain_text())
            .collect()
    }

    fn shift(chat: &mut Chat, code: KeyCode) -> bool {
        chat.agent_tree_key(code, KeyModifiers::SHIFT)
    }

    /// The shape, top to bottom: `@main`, then one row per instance, and the
    /// hide row only while the cursor is in the tree.
    #[test]
    fn the_tree_leads_with_main_and_closes_with_hide() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        seed(&chat, "writer");
        chat.open_agent_tree();

        let rows = texts(&chat);
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!(rows[0].contains("@main"), "{rows:?}");
        assert!(
            rows[0].starts_with("    ╒═"),
            "main opens the tree: {rows:?}"
        );
        assert!(rows[1].contains("@scout"), "{rows:?}");
        assert!(rows[1].starts_with("    ├─"), "{rows:?}");
        assert!(
            rows[2].contains("@writer") && rows[2].starts_with("    └─"),
            "the last instance takes the closing corner while nothing is selected: {rows:?}"
        );

        shift(&mut chat, KeyCode::Down);
        let rows = texts(&chat);
        assert_eq!(
            rows.len(),
            4,
            "the hide row joins in selection mode: {rows:?}"
        );
        assert!(rows[3].contains("hide"), "{rows:?}");
        assert!(
            rows[2].starts_with("    ├─"),
            "and takes the closing corner off the last instance: {rows:?}"
        );
        // D104 left CC's two `enter to …` strings off because the key did
        // nothing; D105 binds it, so the promise is made — and only on the row
        // the cursor is on, which is CC's own gate.
        assert!(
            rows[1].ends_with(" · enter to view"),
            "the selected instance row says what Enter will do: {rows:?}"
        );
        assert!(
            !rows[2].contains("enter to"),
            "and no other row does: {rows:?}"
        );
        assert!(!rows[3].contains("enter to"), "{rows:?}");

        // The hide row's own hint, once the cursor reaches it.
        shift(&mut chat, KeyCode::Down);
        shift(&mut chat, KeyCode::Down);
        let rows = texts(&chat);
        assert!(rows[3].ends_with("hide · enter to collapse"), "{rows:?}");
    }

    /// `enter` in the inline host is CC's whole handler
    /// (`useBackgroundTaskNavigation.ts:206-225`): an instance row asks the host
    /// for the zoom, the hide row collapses the panel, the leader row leaves
    /// selection mode, and with nothing selected the key is the composer's.
    #[test]
    fn enter_zooms_collapses_or_belongs_to_the_composer() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        seed(&chat, "writer");
        chat.open_agent_tree();

        // Not selecting: the key falls through, and the draft keeps it.
        assert!(
            !chat.agent_tree_key(KeyCode::Enter, KeyModifiers::NONE),
            "an open tree is a readout, not a modal"
        );
        assert!(chat.open_zoom.is_none());

        // The first instance row.
        shift(&mut chat, KeyCode::Down);
        assert!(chat.agent_tree_key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            chat.open_zoom,
            Some(crate::tui::zoom::ZoomTarget::Agent("scout".into())),
            "the row that names an agent opens that agent"
        );

        // The leader row leaves selection mode and opens nothing: there is no
        // view to leave from the transcript.
        chat.open_zoom = None;
        shift(&mut chat, KeyCode::Up);
        assert!(chat.agent_tree_key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(chat.open_zoom.is_none());
        assert!(chat.tree_selection().is_none(), "the cursor stepped out");
        assert!(chat.tree.is_some(), "and the tree stayed, as CC's does");

        // The hide row hides.
        shift(&mut chat, KeyCode::Up);
        assert!(chat.agent_tree_key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(chat.tree.is_none());
        assert!(chat.open_zoom.is_none());
    }

    /// A running row says what it is doing and what that has cost; an idle one
    /// says how long it has been idle; a stopped one says it is stopped.
    #[test]
    fn a_row_says_what_the_instance_is_doing() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        chat.session.agents.set_progress_snapshot(
            "scout",
            AgentProgress {
                started_at: Some(std::time::Instant::now()),
                output_tokens: 8_300,
                tool_uses: 12,
                recent_activity: vec!["reading src/lib.rs".to_string()],
            },
        );
        chat.open_agent_tree();
        let rows = texts(&chat);
        assert!(
            rows[1].contains("@scout: reading src/lib.rs…"),
            "the newest activity, ellipsized: {rows:?}"
        );
        assert!(
            rows[1].contains("· 12 tool uses · 8.3k tokens"),
            "CC's stats copy, verbatim: {rows:?}"
        );

        // One tool call is a singular use; the word `tool` never pluralizes.
        assert_eq!(stats_label(1, 900), " · 1 tool use · 900 tokens");
        assert_eq!(
            stats_label(0, 0),
            "",
            "a run that has done nothing says nothing"
        );

        chat.session.agents.stop("scout").expect("stopped");
        assert!(
            texts(&chat)[1].contains("@scout: [stopped]"),
            "a stopped instance stays on the roster: its history is intact and its \
             record is still readable"
        );
    }

    /// The idle row is CC's copy minus the promise D104 cannot keep.
    #[test]
    fn the_idle_row_counts_seconds_and_promises_nothing() {
        assert_eq!(duration_label(std::time::Duration::from_secs(14)), "14s");
        assert_eq!(duration_label(std::time::Duration::from_secs(125)), "2m 5s");
        assert_eq!(
            duration_label(std::time::Duration::from_secs(3_723)),
            "1h 2m 3s"
        );

        let mut chat = test_chat();
        seed(&chat, "scout");
        chat.session.agents.finish("scout", Vec::new(), 0);
        chat.open_agent_tree();
        let rows = texts(&chat);
        assert!(rows[1].contains("@scout: Idle for "), "{rows:?}");
        assert!(!rows[1].contains("enter to view"), "{rows:?}");
    }

    /// Selection is CC's index space, and it wraps at both ends.
    #[test]
    fn shift_arrows_walk_main_the_instances_and_the_hide_row() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        seed(&chat, "writer");

        // Closed: the first press opens the tree and parks on @main.
        assert!(shift(&mut chat, KeyCode::Down));
        assert_eq!(chat.tree.as_ref().and_then(|t| t.selected), Some(MAIN_SEL));

        shift(&mut chat, KeyCode::Down);
        assert_eq!(chat.tree.as_ref().and_then(|t| t.selected), Some(0));
        shift(&mut chat, KeyCode::Down);
        assert_eq!(chat.tree.as_ref().and_then(|t| t.selected), Some(1));
        shift(&mut chat, KeyCode::Down);
        assert_eq!(
            chat.tree.as_ref().and_then(|t| t.selected),
            Some(2),
            "past the last instance is the hide row"
        );
        shift(&mut chat, KeyCode::Down);
        assert_eq!(
            chat.tree.as_ref().and_then(|t| t.selected),
            Some(MAIN_SEL),
            "and past that it wraps to @main"
        );
        shift(&mut chat, KeyCode::Up);
        assert_eq!(
            chat.tree.as_ref().and_then(|t| t.selected),
            Some(2),
            "up from @main wraps to the hide row"
        );

        // The cursor is drawn where it stands.
        chat.tree = Some(AgentTree { selected: Some(0) });
        let rows = texts(&chat);
        assert!(rows[1].starts_with("   ❯╞═"), "{rows:?}");
        assert!(rows[0].starts_with("    ╒═"), "{rows:?}");
    }

    /// With nobody spawned there is nothing to select, nothing to draw, and the
    /// chord is left to the composer.
    #[test]
    fn an_empty_registry_has_no_tree_and_leaves_the_chord_alone() {
        let mut chat = test_chat();
        assert!(
            !shift(&mut chat, KeyCode::Down),
            "shift+↓ is not claimed when it would open an empty tree"
        );
        assert!(chat.tree.is_none());
        chat.open_agent_tree();
        assert!(chat.agent_tree_rows(120).is_empty(), "and draws nothing");
        assert!(chat.pill_row(120).is_none());
    }

    /// `k` stops the selected instance through the one stop path, and only
    /// there: nothing selected is nothing stopped, and `@main` and the hide row
    /// are not instances.
    #[test]
    fn k_stops_only_the_instance_under_the_cursor() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        let alive = |chat: &Chat| {
            chat.session
                .agents
                .list()
                .iter()
                .all(|status| status.state != AgentState::Stopped)
        };

        // Tree closed, nothing selected: `k` is a character.
        assert!(!chat.agent_tree_key(KeyCode::Char('k'), KeyModifiers::NONE));
        assert!(alive(&chat));

        chat.open_agent_tree();
        assert!(
            !chat.agent_tree_key(KeyCode::Char('k'), KeyModifiers::NONE),
            "an open tree with no selection still leaves the key to the composer"
        );
        assert!(alive(&chat));

        chat.tree = Some(AgentTree {
            selected: Some(MAIN_SEL),
        });
        assert!(!chat.agent_tree_key(KeyCode::Char('k'), KeyModifiers::NONE));
        assert!(alive(&chat), "main is not stoppable and has no k");

        shift(&mut chat, KeyCode::Down); // @scout
        assert!(chat.agent_tree_key(KeyCode::Char('k'), KeyModifiers::NONE));
        assert!(!alive(&chat), "the instance under the cursor is stopped");
        assert!(
            chat.warnings
                .iter()
                .any(|(_, text)| text.contains("stopped scout")),
            "through the one stop path, so there is one warning: {:?}",
            chat.warnings
        );

        // The hide row indexes past the end: nothing to stop, nothing thrown.
        shift(&mut chat, KeyCode::Down);
        assert!(!chat.agent_tree_key(KeyCode::Char('k'), KeyModifiers::NONE));
    }

    /// The preview is the instance's own record, three lines, oldest first.
    #[test]
    fn the_preview_shows_the_last_three_lines_of_the_record() {
        let history = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "find the parser".to_string(),
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "one\n\ntwo".to_string(),
                    },
                    ContentBlock::ToolUse {
                        id: "1".to_string(),
                        name: "Bash".to_string(),
                        input: serde_json::json!({"command": "cargo test\n--all"}),
                    },
                ],
            },
        ];
        assert_eq!(
            preview_lines(&history),
            vec![
                "cargo test".to_string(),
                "one".to_string(),
                "two".to_string()
            ],
            "CC's own walk: messages newest-first, blocks in order inside one, \
             text lines from the end of a block, the three reversed at the end"
        );

        // A tool call with nothing descriptive names itself.
        let bare = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "1".to_string(),
                name: "Glob".to_string(),
                input: serde_json::json!({}),
            }],
        }];
        assert_eq!(preview_lines(&bare), vec!["Using Glob…".to_string()]);
    }

    /// The preview rows hang off the instance row under the tree's stem.
    #[test]
    fn the_preview_toggle_adds_rows_under_the_instance() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        chat.session
            .agents
            .finish("scout", vec![Message::user_text("look at the lexer")], 0);
        chat.open_agent_tree();
        assert_eq!(texts(&chat).len(), 2);

        chat.tree_preview = true;
        let rows = texts(&chat);
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!(rows[2].contains("look at the lexer"), "{rows:?}");
        assert!(
            rows[2].starts_with("        "),
            "indented under the stem: {rows:?}"
        );
    }

    /// ctrl+shift+O toggles the preview and ctrl+O still opens the transcript:
    /// two keys one modifier apart, and the tree must not take the older one.
    #[test]
    fn the_preview_toggle_does_not_take_the_transcript_key() {
        let mut chat = test_chat();
        // Each press gets its own clock: 50ms apart is above the paste-burst
        // threshold, so rapid keys in a test read as typing.
        let base = std::time::Instant::now();
        let at = base;
        assert!(chat.on_key_at(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            at
        ));
        assert!(chat.tree_preview, "shifted: the preview");
        assert!(!chat.open_transcript, "and not the pager");

        assert!(chat.on_key_at(
            KeyCode::Char('O'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            base + std::time::Duration::from_millis(50)
        ));
        assert!(!chat.tree_preview, "it toggles");

        assert!(chat.on_key_at(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL,
            base + std::time::Duration::from_millis(100)
        ));
        assert!(chat.open_transcript, "unshifted: the transcript, as before");
        assert!(!chat.tree_preview);
    }

    /// The pills name everyone in one line, main first, and say how to expand.
    #[test]
    fn the_pills_name_everyone_and_the_key_that_expands_them() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        seed(&chat, "writer");
        chat.session.agents.finish("writer", Vec::new(), 0);

        let row = chat.pill_row(120).expect("pills");
        let text = row.line.plain_text();
        assert_eq!(
            text.trim_end(),
            "  @main @scout @writer · shift + ↓ to expand"
        );

        // Idle sorts behind running, and main never moves.
        chat.session.agents.finish("scout", Vec::new(), 0);
        chat.session.agents.insert(
            "zoe",
            AgentKind::Crew,
            None,
            "test instance".to_string(),
            chat.session.clone(),
        );
        let text = chat.pill_row(120).expect("pills").line.plain_text();
        assert!(
            text.starts_with("  @main @zoe @scout @writer"),
            "running first, main always leading: {text}"
        );

        // The tree and the pills are exclusive.
        chat.open_agent_tree();
        assert!(chat.pill_row(120).is_none());
    }

    /// A narrow window drops pills rather than overrunning, and says it did.
    #[test]
    fn the_pills_window_rather_than_overflow() {
        let chat = test_chat();
        for name in ["alpha", "bravo", "charlie", "delta", "echo"] {
            seed(&chat, name);
        }
        let row = chat.pill_row(40).expect("pills");
        let text = row.line.plain_text();
        assert!(
            text_width(&text) <= 40,
            "{text} is {} wide",
            text_width(&text)
        );
        assert!(text.contains('→'), "and says there are more: {text}");
        assert!(text.contains("@main"), "{text}");
    }

    /// A row never overruns the canvas, however narrow it gets.
    #[test]
    fn a_narrow_tree_still_fits_its_rows() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        chat.session.agents.set_progress_snapshot(
            "scout",
            AgentProgress {
                started_at: Some(std::time::Instant::now()),
                output_tokens: 12_345,
                tool_uses: 3,
                recent_activity: vec!["reading a file with a very long name indeed".to_string()],
            },
        );
        chat.open_agent_tree();
        for width in [20usize, 40, 60, 80] {
            for row in chat.agent_tree_rows(width) {
                let text = row.line.plain_text();
                assert!(
                    text_width(&text) <= width,
                    "width {width} overrun by {text:?}"
                );
            }
        }
        // The stats and the hint are what a narrow row spends first.
        let narrow = chat
            .agent_tree_rows(40)
            .iter()
            .map(|row| row.line.plain_text())
            .collect::<Vec<String>>()
            .join("\n");
        assert!(!narrow.contains("tool uses"), "{narrow}");
    }
}
