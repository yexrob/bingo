//! The status layer's helpers (D104 → v6): the wording and the badges every
//! surface shares.
//!
//! D104 built the agent tree here — shift+↑/↓, a panel above the composer —
//! and D115 hung the badges on it. v6 replaced the panel with the roster
//! ([`crate::tui::roster`]): a constant row list under the composer, entered
//! by `↓` from the draft. What stays in this module is what both bodies
//! always shared and every other surface reads too: the status copy
//! ([`status_label`], CC's `TeammateSpinnerLine` wording), the cost segment
//! ([`stats_label`]), the duration format, the two-tier badge
//! ([`push_badge`], D115's grammar), and the store readers
//! (`tree_instances`/`tree_rooms`/`badge_of`/`asking_instance`/
//! `identity_color`).

use ratatui::style::Color;

use crate::agents::{AgentState, AgentStatus};
use crate::tui::avatar::{Gutter, Palette};
use crate::tui::buffer::BufferId;
use crate::tui::chat::{Chat, one_line};
use crate::tui::line::{Line, SegStyle, text_width};

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

/// The two-tier badge, appended to a row or a pill (D115): activity is a bare
/// dot in the text colour — brighter than the dim around it, no number —
/// and a mention (words at *you*) is the count in the accent, bold. The
/// grammar is the ctrl+b dialog's `Tone::Unread`, worn where the pull model
/// rings: nothing else of a conversation's life reaches the flow any more
/// (D114), so the badge is the summons.
pub(crate) fn push_badge(
    line: &mut Line,
    budget: &mut usize,
    (unread, mention): (u64, bool),
    theme: &crate::tui::theme::Theme,
) {
    if unread == 0 {
        return;
    }
    if mention {
        push_fit(
            line,
            budget,
            &format!(" •{unread}"),
            SegStyle::fg(theme.claude).bold(),
        );
    } else {
        push_fit(line, budget, " •", SegStyle::fg(theme.text));
    }
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

    /// The rooms the tree lists under the instances (D115): the ones the user
    /// is a member of, which is the accounting store's own rule (D95 — a room
    /// you are not in is somebody else's conversation, findable in the ctrl+b
    /// dialog). Joining is one post away, and the join is what starts the
    /// badge.
    pub(crate) fn tree_rooms(&self) -> Vec<crate::channels::ChannelStatus> {
        self.session
            .channels
            .list()
            .into_iter()
            .filter(|status| {
                status
                    .members
                    .iter()
                    .any(|m| m == crate::channels::USER_NAME)
            })
            .collect()
    }

    pub(crate) fn badge_of(&self, id: &BufferId) -> (u64, bool) {
        let (mut unread, mention) = self
            .buffers
            .get(id)
            .map(|b| (b.unread(), b.mention()))
            .unwrap_or((0, false));
        if let BufferId::Dm(name) = id {
            unread += self.agent_mail.get(name.as_str()).copied().unwrap_or(0);
        }
        (unread, mention)
    }

    /// The instance whose permission ask is on screen, when the pending ask
    /// is a subagent's (D116). The subagent prompt surface opens its reason
    /// with `{instance} · ` (tool/agent.rs, `subagent_hooks`), so the roster
    /// is the parser; main's own asks carry no such prefix and match nobody.
    pub(crate) fn asking_instance(&self) -> Option<String> {
        let (request, _) = self.pending_ask.as_ref()?;
        let head = request.question.split(" · ").next()?;
        self.tree_instances()
            .iter()
            .find(|s| s.name == head)
            .map(|s| s.name.clone())
    }

    /// One stable colour per name, main's reserved slot included — the palette
    /// the avatar gutter draws from, so a name and its face never disagree.
    /// The pills, the tree's rows and D106's `@name❯` lines all ask here.
    pub(crate) fn identity_color(&self, name: &str) -> Color {
        let palette = Palette::new(&self.theme);
        let gutter = Gutter::new(false, false, &palette, &self.faces_pinned);
        palette.avatars[gutter.index_for(name) % palette.avatars.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The idle row is CC's copy minus the promise D104 cannot keep.
    #[test]
    fn the_idle_row_counts_seconds_and_promises_nothing() {
        assert_eq!(duration_label(std::time::Duration::from_secs(14)), "14s");
        assert_eq!(duration_label(std::time::Duration::from_secs(125)), "2m 5s");
        assert_eq!(
            duration_label(std::time::Duration::from_secs(3723)),
            "1h 2m 3s"
        );
    }

    /// One tool call is a singular use; the word `tool` never pluralizes, and
    /// a run that has done nothing says nothing.
    #[test]
    fn the_stats_segment_keeps_ccs_wording() {
        assert_eq!(stats_label(1, 900), " · 1 tool use · 900 tokens");
        assert_eq!(stats_label(12, 8_300), " · 12 tool uses · 8.3k tokens");
        assert_eq!(stats_label(0, 0), "");
        assert_eq!(stats_body(0, 0), "0 tool uses · 0 tokens");
    }

    /// The badge's two tiers (D115): activity is a bare dot, a mention is the
    /// count in the accent — and nothing at zero.
    #[test]
    fn the_badge_has_two_tiers_and_nothing_at_zero() {
        let theme = crate::tui::theme::Theme::dark();
        let row = |badge| {
            let mut line = Line::styled(String::new(), SegStyle::fg(theme.text));
            let mut budget = 40usize;
            push_badge(&mut line, &mut budget, badge, &theme);
            line.plain_text()
        };
        assert_eq!(row((0, false)), "");
        assert_eq!(row((3, false)), " •", "activity is a dot, no number");
        assert_eq!(row((3, true)), " •3", "a mention is the count");
    }
}
