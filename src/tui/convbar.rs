//! The conversation bar (D90): one row above the composer saying which
//! conversations exist, which one you are in, and which of them want you.
//!
//! It is the only net-new chrome row the blueprint approves, and it earns the
//! row by answering the question the unified conversation model created: once
//! every conversation shares one terminal, "what else is going on" has no
//! surface at all. `/open` (D89) could reach a conversation but never named
//! one, so a DM that filled up while you were in the hub was invisible until
//! you thought to ask.
//!
//! Three rules keep it from becoming noise:
//!
//! - **It renders only when there is something to switch between.** A session
//!   that has never spawned an agent or created a channel has exactly one
//!   buffer, and one buffer needs no bar — the simple case pays nothing.
//! - **It reads the registry and nothing else.** Order is [`BufferId`]'s own
//!   ordering, unread and mention are the accessors D88 already maintains on
//!   the fifteen-tick poll, and presence is the agent state the ctrl+b manager
//!   shows. Nothing here polls, counts or caches, so the bar cannot disagree
//!   with the switcher or the manager about the same fact.
//! - **Overflow is deterministic.** Too many conversations for the width elide
//!   to `…` around a run that always contains the active one. There is no
//!   scroll offset, so the bar has no state to get stuck in.

use ratatui::style::Color;

use crate::tui::avatar::Palette;
use crate::tui::buffer::BufferId;
use crate::tui::chat::{Chat, Row, one_line};
use crate::tui::line::{Line, SegStyle, text_width};
use crate::tui::theme::Theme;

/// The gutter every chrome tier keeps.
const INDENT: &str = "  ";
/// What stands between two entries. Two columns, because an entry can carry an
/// internal space (`@scout (2)`) and a single gap would blur the boundary.
const GAP: &str = "  ";
/// What stands in for entries that did not fit.
const ELLIPSIS: &str = "…";

/// One conversation as the bar sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BarEntry {
    pub id: BufferId,
    /// `hub` / `#team` / `#build` / `@scout` — the label already carries its
    /// sigil, so the bar never re-derives one and can never print a name the
    /// `/open` completion would not accept.
    pub label: String,
    /// Whether the instance behind a DM is running. `None` for everything that
    /// is not a DM: a channel has no pulse to report, and inventing an idle
    /// glyph for one would say something false.
    pub presence: Option<bool>,
    pub unread: u64,
    pub mention: bool,
    pub active: bool,
}

impl BarEntry {
    /// The entry as plain text: `●@scout (2)`.
    pub fn text(&self) -> String {
        let mut out = String::new();
        match self.presence {
            Some(true) => out.push('●'),
            Some(false) => out.push('○'),
            None => {}
        }
        out.push_str(&self.label);
        if self.unread > 0 {
            out.push_str(&format!(" ({})", self.unread));
        }
        out
    }
}

/// How the active entry is marked. Kept next to the entry styling so the bar
/// and the switcher cannot drift on what "you are here" looks like.
fn active_style(theme: &Theme) -> SegStyle {
    SegStyle::fg(theme.claude).bold()
}

/// Style for an entry's name.
fn label_style(entry: &BarEntry, theme: &Theme) -> SegStyle {
    if entry.active {
        active_style(theme)
    } else if entry.unread > 0 {
        SegStyle::fg(theme.text)
    } else {
        SegStyle::fg(theme.text_secondary)
    }
}

/// Style for an entry's `(n)` badge. A conversation that said your name is
/// worth more than one that merely moved, and the active conversation never
/// carries a badge at all (entering one reads it), so the accent is free to
/// mean "wants you" here without colliding with "you are here".
fn badge_style(entry: &BarEntry, theme: &Theme, pal: &Palette) -> SegStyle {
    if entry.mention {
        SegStyle::fg(theme.claude).bold()
    } else {
        SegStyle::fg(pal.unread)
    }
}

/// Append one entry's segments.
fn push_entry(line: &mut Line, entry: &BarEntry, theme: &Theme, pal: &Palette) {
    if let Some(running) = entry.presence {
        let color = if running {
            pal.presence_on
        } else {
            pal.presence_off
        };
        line.push_styled(
            if running { "●" } else { "○" }.to_string(),
            SegStyle::fg(color),
        );
    }
    line.push_styled(entry.label.clone(), label_style(entry, theme));
    if entry.unread > 0 {
        line.push_styled(
            format!(" ({})", entry.unread),
            badge_style(entry, theme, pal),
        );
    }
}

/// The run of entries that fits, as a half-open range containing `active`.
///
/// The run grows outward from the active entry — forward first, so the
/// registry's order still reads left to right — and `…` stands in for whatever
/// was cut on either side. It is a pure function of the widths, the active
/// index and the budget: the same registry at the same width always produces
/// the same row, which is why the bar needs no scroll state and never animates.
fn window(widths: &[usize], active: usize, budget: usize) -> (usize, usize) {
    if run_width(widths, 0, widths.len()) <= budget {
        return (0, widths.len());
    }
    let (mut start, mut end) = (active, active + 1);
    loop {
        let mut grew = false;
        if end < widths.len() && run_width(widths, start, end + 1) <= budget {
            end += 1;
            grew = true;
        }
        if start > 0 && run_width(widths, start - 1, end) <= budget {
            start -= 1;
            grew = true;
        }
        if !grew {
            return (start, end);
        }
    }
}

/// How wide a run renders: its entries, the gaps between them, and the `…`
/// marks standing in for whatever it cuts on either side.
fn run_width(widths: &[usize], start: usize, end: usize) -> usize {
    let gap = text_width(GAP);
    let mark = text_width(ELLIPSIS) + gap;
    let mut total: usize = widths[start..end].iter().sum();
    total += gap * (end - start).saturating_sub(1);
    if start > 0 {
        total += mark;
    }
    if end < widths.len() {
        total += mark;
    }
    total
}

impl Chat {
    /// The colour of the teammate you are talking to, or `None` anywhere else.
    ///
    /// CC's teammate grammar: while a DM is the conversation, the composer
    /// wears that agent's colour, so the surface you are typing into says who
    /// will read it before you have read a word of chrome. The colour is the
    /// one the avatar machinery already assigns the name (D89 kept `Palette`),
    /// so a teammate is the same hue in the bar, the gutter and the prompt —
    /// no new colours, and both themes are covered by the palette's own
    /// light/dark branch.
    pub(crate) fn teammate_tint(&self) -> Option<Color> {
        let BufferId::Dm(name) = self.buffers.active() else {
            return None;
        };
        let pal = Palette::new(&self.theme);
        let index = crate::tui::avatar::index_of(name);
        Some(pal.avatars[index % pal.avatars.len()])
    }

    /// The registry as the bar reads it.
    ///
    /// One pass over the agent list rather than a lookup per DM: the list is a
    /// lock and a clone, and a bar that took one per entry would pay for the
    /// registry twice on every frame.
    pub(crate) fn bar_entries(&self) -> Vec<BarEntry> {
        let statuses = self.session.agents.list();
        let active = self.buffers.active();
        self.buffers
            .iter()
            .map(|buffer| {
                let id = buffer.id().clone();
                let presence = match &id {
                    BufferId::Dm(name) => Some(statuses.iter().any(|status| {
                        status.name == *name && status.state == crate::agents::AgentState::Running
                    })),
                    _ => None,
                };
                BarEntry {
                    label: id.label(),
                    presence,
                    unread: buffer.unread(),
                    mention: buffer.mention(),
                    active: id == *active,
                    id,
                }
            })
            .collect()
    }

    /// The bar, or no rows at all.
    ///
    /// A session with one conversation has nothing to switch between, and a
    /// bar reading `hub` on its own would be a row spent saying that the only
    /// thing on screen is the thing on screen.
    pub(crate) fn conversation_bar_rows(&self, width: usize) -> Vec<Row> {
        let entries = self.bar_entries();
        if entries.len() < 2 {
            return Vec::new();
        }
        let theme = &self.theme;
        let pal = Palette::new(theme);
        let budget = width.saturating_sub(text_width(INDENT));
        let texts: Vec<String> = entries.iter().map(BarEntry::text).collect();
        let widths: Vec<usize> = texts.iter().map(|text| text_width(text)).collect();
        let active = entries.iter().position(|entry| entry.active).unwrap_or(0);
        let (start, end) = window(&widths, active, budget);

        // The one case the window cannot satisfy: a terminal too narrow to hold
        // the active entry *and* the marks saying what was cut around it. The
        // active entry is the one thing that must be visible, so it is what
        // survives, clamped and unmarked — a bar that overflowed the width
        // would wrap, and a wrapped chrome row throws off every height the
        // frame assembler measured.
        if run_width(&widths, start, end) > budget {
            return vec![Row::new(Line::styled(
                format!("{INDENT}{}", one_line(&texts[active], budget)),
                active_style(theme),
            ))];
        }

        let mut line = Line::styled(INDENT.to_string(), SegStyle::plain());
        let dim = theme.muted();
        if start > 0 {
            line.push_styled(format!("{ELLIPSIS}{GAP}"), dim);
        }
        for (offset, entry) in entries[start..end].iter().enumerate() {
            if offset > 0 {
                line.push_styled(GAP.to_string(), dim);
            }
            push_entry(&mut line, entry, theme, &pal);
        }
        if end < entries.len() {
            line.push_styled(format!("{GAP}{ELLIPSIS}"), dim);
        }
        vec![Row::new(line)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentKind;
    use crate::channels::ChannelMode;
    use crate::tui::test_util::chat_at;
    use crate::tui::theme::{Theme, ThemeSetting};

    fn test_chat() -> Chat {
        chat_at(100, 40)
    }

    fn seed_agent(chat: &Chat, name: &str) {
        chat.session.agents.insert(
            name,
            AgentKind::Hire,
            None,
            "test instance".to_string(),
            chat.session.clone(),
        );
    }

    /// A crew member: what puts `#team` in the registry (D93).
    fn seed_crew(chat: &Chat, name: &str) {
        chat.session.agents.insert(
            name,
            AgentKind::Crew,
            None,
            "crew member".to_string(),
            chat.session.clone(),
        );
    }

    fn bar_text(chat: &Chat, width: usize) -> String {
        chat.conversation_bar_rows(width)
            .iter()
            .map(|row| row.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The bar is not a decoration that is always there: with one conversation
    /// there is nothing to switch between, and the row is not spent.
    #[test]
    fn a_lone_hub_shows_no_bar() {
        let chat = test_chat();
        assert!(
            chat.conversation_bar_rows(100).is_empty(),
            "a bare hub session pays nothing for the bar"
        );
    }

    /// Entries, their order, their sigils and their badges — the registry's
    /// order is the bar's order, so the two can never disagree.
    #[test]
    fn the_bar_lists_the_registry_in_its_own_order() {
        let mut chat = test_chat();
        chat.session
            .channels
            .create("build", vec!["scout".to_string()], ChannelMode::Free)
            .expect("channel created");
        seed_agent(&chat, "zoe");
        seed_crew(&chat, "scout");
        chat.buffers.note_watch_event(
            "scout #1 · go",
            crate::watch::WatchKind::Agent,
            crate::watch::WatchState::Running,
            None,
            1,
        );
        chat.refresh_conversations();

        let text = bar_text(&chat, 100);
        let order: Vec<&str> = ["hub", "#team", "#build", "@scout", "@zoe"].to_vec();
        let mut at = 0usize;
        for name in order {
            let found = text[at..]
                .find(name)
                .unwrap_or_else(|| panic!("{name} missing or out of order in {text:?}"));
            at += found + name.len();
        }
    }

    /// Presence is a DM's fact and nobody else's: a channel has no pulse, so
    /// it gets no glyph rather than a glyph that means nothing.
    #[test]
    fn presence_marks_dms_and_only_dms() {
        let mut chat = test_chat();
        chat.session
            .channels
            .create("build", vec!["scout".to_string()], ChannelMode::Free)
            .expect("channel created");
        seed_agent(&chat, "scout");
        chat.refresh_conversations();

        let entries = chat.bar_entries();
        let dm = entries
            .iter()
            .find(|entry| entry.label == "@scout")
            .expect("the DM is listed");
        let channel = entries
            .iter()
            .find(|entry| entry.label == "#build")
            .expect("the channel is listed");
        assert!(dm.presence.is_some(), "a DM reports whether it is running");
        assert_eq!(channel.presence, None, "a channel has no pulse to report");
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.label == "hub")
                .map(|entry| entry.presence),
            Some(None)
        );
    }

    /// Unread counts, the mention flag and the active mark, each in its own
    /// emphasis — and the active conversation never carries a badge, because
    /// being in one reads it.
    #[test]
    fn unread_mention_and_active_each_read_differently() {
        let mut chat = test_chat();
        chat.session
            .channels
            .create(
                "build",
                vec!["scout".to_string(), crate::channels::USER_NAME.to_string()],
                ChannelMode::Free,
            )
            .expect("channel created");
        seed_agent(&chat, "scout");
        chat.refresh_conversations();
        chat.session
            .channels
            .post("scout", "build", "@user look at this")
            .expect("posted");
        chat.session
            .channels
            .post("scout", "build", "and this")
            .expect("posted");
        chat.buffers.refresh(&chat.session.clone(), 2);

        assert!(
            bar_text(&chat, 100).contains("#build (2)"),
            "the badge counts: {}",
            bar_text(&chat, 100)
        );

        let theme = chat.theme.clone();
        let pal = Palette::new(&theme);
        let entries = chat.bar_entries();
        let channel = entries
            .iter()
            .find(|entry| entry.label == "#build")
            .expect("the channel is listed");
        assert!(channel.mention, "it said the user's name");
        assert_eq!(
            badge_style(channel, &theme, &pal).fg,
            Some(theme.claude),
            "a conversation that wants you is emphasized apart from one that merely moved"
        );
        let quiet = BarEntry {
            mention: false,
            ..channel.clone()
        };
        assert_eq!(badge_style(&quiet, &theme, &pal).fg, Some(pal.unread));

        let hub = entries
            .iter()
            .find(|entry| entry.label == "hub")
            .expect("the hub is listed");
        assert!(hub.active, "the hub is where the session starts");
        assert_eq!(hub.unread, 0, "the conversation you are in is never unread");
        assert_eq!(label_style(hub, &theme).fg, Some(theme.claude));
        assert!(label_style(hub, &theme).bold);
        assert_eq!(
            label_style(channel, &theme).fg,
            Some(theme.text),
            "an unread conversation is brighter than a quiet one"
        );
    }

    /// Both themes paint the bar from the same tokens, so neither is left
    /// reading a color that was only ever checked in the other.
    #[test]
    fn the_bar_is_painted_in_both_themes() {
        for setting in [ThemeSetting::Dark, ThemeSetting::Light] {
            let mut chat = test_chat();
            chat.theme = Theme::for_terminal(setting, None);
            seed_agent(&chat, "scout");
            chat.refresh_conversations();
            let rows = chat.conversation_bar_rows(100);
            assert_eq!(rows.len(), 1, "{setting:?}");
            let colors: Vec<_> = rows[0]
                .line
                .segs
                .iter()
                .filter_map(|seg| seg.style.fg)
                .collect();
            assert!(
                colors.contains(&chat.theme.claude),
                "the active conversation is accented in {setting:?}: {colors:?}"
            );
            assert!(
                colors.contains(&chat.theme.text_secondary),
                "and a quiet one is dim in {setting:?}: {colors:?}"
            );
        }
    }

    /// Narrow terminals keep the entry that matters. Whatever the width, the
    /// row fits it — a chrome row that wrapped would throw off every height
    /// the frame assembler measured.
    #[test]
    fn overflow_keeps_the_active_entry_and_never_exceeds_the_width() {
        let mut chat = test_chat();
        for name in ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"] {
            seed_agent(&chat, name);
        }
        chat.refresh_conversations();
        chat.switch_to(BufferId::Dm("foxtrot".to_string()));

        for width in 4..90 {
            let rows = chat.conversation_bar_rows(width);
            assert_eq!(rows.len(), 1, "one row at width {width}");
            let text = rows[0].line.plain_text();
            assert!(
                text_width(&text) <= width,
                "width {width} overflowed: {text:?}"
            );
            // Below the width of one entry the bar can only show the active
            // entry cut short — but it is still that entry, never another.
            let shown = if width >= 20 { "foxtrot" } else { "\u{25cf}" };
            assert!(
                text.contains(shown),
                "the active conversation stayed visible at width {width}: {text:?}"
            );
            for other in ["alpha", "bravo", "charlie"] {
                assert!(
                    width >= 20 || !text.contains(other),
                    "a narrow bar showed {other} instead of the active entry: {text:?}"
                );
            }
        }

        // And a width that fits everything elides nothing.
        let wide = bar_text(&chat, 200);
        assert!(!wide.contains(ELLIPSIS), "{wide:?}");
        for name in ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"] {
            assert!(wide.contains(name), "{name} missing from {wide:?}");
        }
    }

    /// The window is the elision rule, checked where it is cheapest to check.
    #[test]
    fn the_window_grows_outward_from_the_active_entry() {
        let widths = vec![5, 5, 5, 5, 5];
        assert_eq!(window(&widths, 0, 100), (0, 5), "everything fits");
        // 5 + gap(2) + 5 = 12 for two entries, plus a trailing `… ` mark (3).
        assert_eq!(window(&widths, 0, 15), (0, 2));
        // From the middle, forward first: index 2 grows to 3 before it grows
        // back to 1.
        let (start, end) = window(&widths, 2, 18);
        assert!(
            start <= 2 && end > 2,
            "the active entry is inside {start}..{end}"
        );
        assert_eq!((start, end), (2, 4));
        // A budget that holds only the active entry keeps exactly it.
        assert_eq!(window(&widths, 2, 16), (2, 3));
        // A budget that cannot hold even one entry still yields the active one.
        assert_eq!(window(&widths, 3, 1), (3, 4));
    }
}
