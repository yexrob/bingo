//! The ctrl+k conversation switcher (D90): every conversation in one list,
//! filtered by typing, entered with Enter.
//!
//! The bar says what exists; the switcher is how you get there without
//! spelling a name. It is the door `/open` was standing in for (D89) — same
//! destination, same [`Chat::switch_to`], but reached by recognition rather
//! than recall, which is what a roster is for.
//!
//! Two orderings, and the difference matters. The bar is ordered by the
//! registry, because a bar you read at a glance must not move under you. The
//! switcher is ordered by **recency**, because a reader who reached for ctrl+k
//! is asking "what just happened" rather than "what exists" — with the hub
//! pinned on top, since it is where turns run and where Esc goes.
//!
//! The switcher does not absorb the ctrl+b manager, though the blueprint
//! allows it to eventually: the two answer different questions (which
//! conversation am I in, versus what are my agents doing) and the manager
//! carries per-agent stats and prompts this list has no room for. What they do
//! share is the one thing that must not fork — stopping an agent goes through
//! the manager's own [`Chat::stop_agent_from_manager`], so the two surfaces
//! cannot drift on what stopping means.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::tui::buffer::BufferId;
use crate::tui::chat::{Chat, Row, one_line};
use crate::tui::complete::fuzzy_rank;
use crate::tui::line::{Line, SegStyle};

/// How many conversations the list shows at once; the rest scroll under the
/// selection. The ctrl+b manager's number, because the two overlays occupy the
/// same place on screen and a reader should not have to relearn its size.
const SWITCHER_ROWS_MAX: usize = 8;

/// The open switcher.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Switcher {
    /// What has been typed to filter the list.
    pub query: String,
    /// Index into the *filtered* list.
    pub selected: usize,
}

/// One conversation as the switcher sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitcherItem {
    pub id: BufferId,
    pub label: String,
    /// Whether the instance behind a DM is running; `None` for anything else.
    pub presence: Option<bool>,
    pub unread: u64,
    pub mention: bool,
    /// Tick of the last change in the source; 0 when nothing has happened.
    pub last_activity: u64,
}

/// How long ago, in the shortest form that is still true. Empty when the
/// conversation has no clock — the hub never observes itself, and printing an
/// age derived from tick zero would be a number that means nothing.
fn age_label(now: u64, last_activity: u64) -> String {
    if last_activity == 0 {
        return String::new();
    }
    let secs = now.saturating_sub(last_activity) * crate::tui::motion::TICK_MS / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m", secs / 60)
    }
}

impl Chat {
    /// Every conversation, most recently active first, hub pinned on top.
    pub(crate) fn switcher_items(&self) -> Vec<SwitcherItem> {
        let statuses = self.session.agents.list();
        let mut items: Vec<SwitcherItem> = self
            .buffers
            .iter()
            .map(|buffer| {
                let id = buffer.id().clone();
                let presence = match &id {
                    BufferId::Dm(name) => Some(statuses.iter().any(|status| {
                        status.name == *name && status.state == crate::agents::AgentState::Running
                    })),
                    _ => None,
                };
                SwitcherItem {
                    label: id.label(),
                    presence,
                    unread: buffer.unread(),
                    mention: buffer.mention(),
                    last_activity: buffer.last_activity(),
                    id,
                }
            })
            .collect();
        // Recency, then name: two conversations that last moved on the same
        // tick must not swap places between frames.
        items.sort_by(|left, right| {
            right
                .last_activity
                .cmp(&left.last_activity)
                .then_with(|| left.label.cmp(&right.label))
        });
        pin_hub(&mut items);
        items
    }

    /// The list as the query leaves it. One scorer for the whole app (D85), so
    /// the switcher matches names the way the mention dropdown and every
    /// argument completion do.
    pub(crate) fn switcher_matches(&self, query: &str) -> Vec<SwitcherItem> {
        let mut items = fuzzy_rank(query, self.switcher_items(), |item| item.label.as_str());
        pin_hub(&mut items);
        items
    }

    /// Open the switcher. Inert behind a permission dialog (D81): a surface
    /// that competes for Enter must not open over a question that is blocking
    /// a turn.
    pub(crate) fn open_switcher(&mut self) {
        if self.pending_ask.is_some() {
            return;
        }
        self.switcher = Some(Switcher::default());
        self.dirty = true;
    }

    /// Keys, while the switcher is open. Modal by default: an open roster
    /// swallows what it does not understand rather than letting it edit the
    /// draft underneath.
    pub(crate) fn switcher_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if self.pending_ask.is_some() {
            return false;
        }
        let Some(mut state) = self.switcher.take() else {
            return false;
        };
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        let alt = modifiers.contains(KeyModifiers::ALT);
        let items = self.switcher_matches(&state.query);
        state.selected = state.selected.min(items.len().saturating_sub(1));
        self.dirty = true;
        match code {
            // Ctrl+C skips every layer and always means out (D80). It is left
            // to the global handler rather than answered here.
            KeyCode::Char('c') if ctrl => {
                self.switcher = Some(state);
                return false;
            }
            KeyCode::Esc => return true,
            KeyCode::Down => {
                state.selected = (state.selected + 1).min(items.len().saturating_sub(1));
            }
            KeyCode::Up => state.selected = state.selected.saturating_sub(1),
            KeyCode::Enter => {
                if let Some(item) = items.get(state.selected) {
                    let id = item.id.clone();
                    self.switch_to(id);
                }
                return true;
            }
            // Stopping an agent is the manager's action, reached here through
            // the manager's own path. It is `ctrl+x` rather than the manager's
            // bare `x` because this list filters as you type: a plain `x` would
            // have to be either a letter or a kill, and a key that is sometimes
            // irreversible is worse than one extra modifier.
            KeyCode::Char('x') if ctrl => {
                if let Some(BufferId::Dm(name)) = items.get(state.selected).map(|item| &item.id) {
                    let name = name.clone();
                    self.stop_agent_from_manager(&name);
                }
            }
            KeyCode::Backspace => {
                state.query.pop();
                state.selected = 0;
            }
            KeyCode::Char(c) if !ctrl && !alt && !c.is_control() => {
                state.query.push(c);
                state.selected = 0;
            }
            _ => {}
        }
        self.switcher = Some(state);
        true
    }

    /// Rows for the overlay, in the ctrl+b manager's frame.
    pub fn switcher_rows(&self, width: usize) -> Vec<Row> {
        let Some(state) = &self.switcher else {
            return Vec::new();
        };
        let theme = &self.theme;
        let pal = crate::tui::avatar::Palette::new(theme);
        let items = self.switcher_matches(&state.query);
        let heading = if state.query.is_empty() {
            "Switch conversation".to_string()
        } else {
            format!("Switch conversation · {}", state.query)
        };
        let mut rows = vec![Row::new(Line::styled(
            heading,
            SegStyle::fg(theme.text).bold(),
        ))];
        if items.is_empty() {
            rows.push(Row::new(Line::styled(
                "No conversation matches",
                SegStyle::fg(theme.text_secondary),
            )));
        } else {
            let selected = state.selected.min(items.len() - 1);
            let start = selected.saturating_sub(SWITCHER_ROWS_MAX - 1);
            for (index, item) in items.iter().enumerate().skip(start).take(SWITCHER_ROWS_MAX) {
                let presence = match item.presence {
                    Some(true) => "● ",
                    Some(false) => "○ ",
                    None => "  ",
                };
                let unread = match (item.unread, item.mention) {
                    (0, _) => String::new(),
                    (n, true) => format!(" · {n} unread · wants you"),
                    (n, false) => format!(" · {n} unread"),
                };
                let age = match age_label(self.tick, item.last_activity) {
                    age if age.is_empty() => String::new(),
                    age => format!(" · {age}"),
                };
                let prefix = if index == selected { "❯ " } else { "  " };
                rows.push(Row::new(Line::styled(
                    one_line(
                        &format!("{prefix}{presence}{}{unread}{age}", item.label),
                        width.saturating_sub(2),
                    ),
                    SegStyle::fg(if index == selected {
                        theme.permission
                    } else if item.unread > 0 {
                        theme.text
                    } else {
                        theme.text_secondary
                    }),
                )));
            }
            if items.len() > SWITCHER_ROWS_MAX {
                rows.push(Row::new(Line::styled(
                    format!("  … {} conversations", items.len()),
                    SegStyle::fg(pal.unread),
                )));
            }
        }
        rows.push(Row::new(Line::styled(
            "↑/↓ select · Enter open · ctrl+x stop · Esc close",
            SegStyle::fg(theme.text_secondary),
        )));
        crate::tui::chat::manager_box(rows, width, theme)
    }
}

/// The hub goes first however the rest is ordered: it is where turns run and
/// where Esc goes, so it is the one destination that must never be hunted for.
fn pin_hub(items: &mut Vec<SwitcherItem>) {
    if let Some(at) = items.iter().position(|item| item.id == BufferId::Hub)
        && at > 0
    {
        let hub = items.remove(at);
        items.insert(0, hub);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentKind;
    use crate::tui::test_util::chat_at;

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

    fn key(chat: &mut Chat, code: KeyCode) -> bool {
        chat.switcher_key(code, KeyModifiers::NONE)
    }

    fn labels(chat: &Chat) -> Vec<String> {
        let query = chat
            .switcher
            .as_ref()
            .map(|state| state.query.clone())
            .unwrap_or_default();
        chat.switcher_matches(&query)
            .into_iter()
            .map(|item| item.label)
            .collect()
    }

    /// Recency order with the hub pinned: the question a roster answers is
    /// "what just happened", not "what exists".
    #[test]
    fn the_list_is_recent_first_with_the_hub_on_top() {
        let mut chat = test_chat();
        for name in ["alpha", "bravo", "charlie"] {
            seed_agent(&chat, name);
        }
        chat.refresh_conversations();
        // Three separate ticks of activity, oldest first.
        chat.buffers
            .observe(BufferId::Dm("alpha".to_string()), 1, true, 10);
        chat.buffers
            .observe(BufferId::Dm("bravo".to_string()), 1, true, 30);
        chat.buffers
            .observe(BufferId::Dm("charlie".to_string()), 1, true, 20);

        assert_eq!(
            labels(&chat),
            vec!["hub", "@bravo", "@charlie", "@alpha"],
            "most recently active first, hub pinned"
        );
    }

    /// Typing filters through the app's one scorer, and the hub stays reachable
    /// at the top of whatever survives.
    #[test]
    fn typing_filters_the_list() {
        let mut chat = test_chat();
        for name in ["scout", "zoe"] {
            seed_agent(&chat, name);
        }
        chat.refresh_conversations();
        chat.open_switcher();

        assert_eq!(labels(&chat).len(), 3, "hub + two DMs");
        for c in "sct".chars() {
            assert!(key(&mut chat, KeyCode::Char(c)));
        }
        assert_eq!(
            labels(&chat),
            vec!["@scout"],
            "a subsequence match, the same rule the mention dropdown uses"
        );
        assert!(key(&mut chat, KeyCode::Backspace));
        assert!(key(&mut chat, KeyCode::Backspace));
        assert!(key(&mut chat, KeyCode::Backspace));
        assert_eq!(labels(&chat).len(), 3, "and backspace gives them back");
    }

    /// Enter switches through D89's own path and closes; Esc closes without
    /// moving anything.
    #[test]
    fn enter_switches_and_esc_does_not() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout");
        chat.refresh_conversations();

        chat.open_switcher();
        assert!(key(&mut chat, KeyCode::Down));
        assert!(key(&mut chat, KeyCode::Enter));
        assert!(chat.switcher.is_none(), "Enter closes it");
        assert_eq!(*chat.buffers.active(), BufferId::Dm("scout".to_string()));

        chat.open_switcher();
        assert!(key(&mut chat, KeyCode::Down));
        assert!(key(&mut chat, KeyCode::Esc));
        assert!(chat.switcher.is_none(), "Esc closes it too");
        assert_eq!(
            *chat.buffers.active(),
            BufferId::Dm("scout".to_string()),
            "and leaves the conversation where it was"
        );
    }

    /// The selection clamps at both ends rather than wrapping, the way the
    /// ctrl+b manager's list does.
    #[test]
    fn the_selection_clamps_at_both_ends() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout");
        chat.refresh_conversations();
        chat.open_switcher();

        for _ in 0..8 {
            key(&mut chat, KeyCode::Down);
        }
        assert_eq!(
            chat.switcher.as_ref().map(|state| state.selected),
            Some(1),
            "two conversations, so the last index is 1"
        );
        for _ in 0..8 {
            key(&mut chat, KeyCode::Up);
        }
        assert_eq!(chat.switcher.as_ref().map(|state| state.selected), Some(0));
    }

    /// `ctrl+x` stops the highlighted agent through the manager's own path, and
    /// the conversation stays on the list — a stopped teammate still has a
    /// conversation worth reading back.
    #[tokio::test]
    async fn ctrl_x_stops_the_highlighted_agent_and_keeps_its_conversation() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout");
        chat.refresh_conversations();
        assert_eq!(
            chat.session.agents.list()[0].state,
            crate::agents::AgentState::Running
        );

        chat.open_switcher();
        assert!(key(&mut chat, KeyCode::Down));
        assert!(chat.switcher_key(KeyCode::Char('x'), KeyModifiers::CONTROL));

        assert_ne!(
            chat.session.agents.list()[0].state,
            crate::agents::AgentState::Running,
            "the agent was stopped"
        );
        assert!(
            chat.warnings.iter().any(|(_, text)| text.contains("scout")),
            "through the manager's path, which says so: {:?}",
            chat.warnings
        );
        assert!(chat.switcher.is_some(), "the overlay stays open");
        assert!(
            labels(&chat).contains(&"@scout".to_string()),
            "and the conversation is still listed"
        );
        let idle = chat
            .switcher_matches("")
            .into_iter()
            .find(|item| item.label == "@scout")
            .expect("scout is listed");
        assert_eq!(idle.presence, Some(false), "listed as idle");
    }

    /// A permission dialog owns the keyboard (D81): the switcher neither opens
    /// behind one nor answers a key.
    #[tokio::test]
    async fn the_switcher_is_inert_behind_a_dialog() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout");
        chat.refresh_conversations();
        chat.open_switcher();
        assert!(chat.switcher.is_some());
        chat.switcher = None;

        let (tx, _rx) = tokio::sync::oneshot::channel();
        chat.pending_ask = Some((
            crate::ui::PermissionRequest {
                title: "Allow Bash".to_string(),
                question: "run `ls`?".to_string(),
                options: vec!["yes".to_string(), "no".to_string()],
                descriptions: vec![None, None],
                free_text: false,
                kind: crate::ui::AskKind::Permission,
                preview: None,
                scope: None,
            },
            tx,
        ));

        chat.open_switcher();
        assert!(chat.switcher.is_none(), "it does not open behind a dialog");
        assert!(
            !chat.switcher_key(KeyCode::Enter, KeyModifiers::NONE),
            "and it answers nothing"
        );
    }

    /// The rows say what the list knows: presence, unread, the mention, an age,
    /// and which one Enter would open.
    #[test]
    fn the_rows_state_presence_unread_and_age() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout");
        chat.refresh_conversations();
        chat.tick = 100;
        chat.buffers
            .observe(BufferId::Dm("scout".to_string()), 3, true, 40);
        chat.open_switcher();

        let text = chat
            .switcher_rows(100)
            .iter()
            .map(|row| row.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Switch conversation"), "{text}");
        assert!(text.contains("❯ "), "the selection is marked: {text}");
        assert!(text.contains("● @scout"), "presence: {text}");
        assert!(text.contains("3 unread · wants you"), "unread: {text}");
        // 60 ticks at 33ms is just under two seconds.
        assert!(text.contains(" · 1s"), "age: {text}");
        assert!(text.contains("ctrl+x stop"), "the hint names it: {text}");
    }

    /// Ages read in the shortest true unit, and a conversation with no clock
    /// gets no age rather than one measured from tick zero.
    #[test]
    fn ages_are_short_and_never_invented() {
        assert_eq!(age_label(100, 0), "", "the hub never observes itself");
        assert_eq!(age_label(40, 10), "0s");
        assert_eq!(age_label(1000, 10), "32s");
        assert_eq!(age_label(10_000, 10), "5m");
    }
}
