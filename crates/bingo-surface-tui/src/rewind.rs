//! `esc esc` on an empty composer: the turns of this transcript, newest first,
//! and `⏎` to go back to one (design §3 — the rewind picker is a card).
//!
//! The turns are derived from `state.items` every frame: an item carries the
//! turn it belongs to, so the boundaries are already in the transcript and
//! nothing here keeps a second list of them.
//!
//! The picker is offered only where the session has a `/rewind` to run. As of
//! M11e no plugin registers one (`docs/plans/M11e-content-kinds.md`), so the
//! chord is silent rather than opening a card that could not do anything; the
//! day a store command lands, it lights up with no change here.

use bingo_sdk::{CommandSpec, ContentPart, ItemBody, SessionState, TurnId};
use ratatui::text::{Line, Span};

use crate::theme;

/// The command a chosen row runs.
pub const COMMAND: &str = "rewind";

/// How many turns the card lists: the same eight rows the `/` menu offers.
const ROWS: usize = 8;

/// The card's own state. Which turns it lists is read from the session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rewind {
    pub selected: usize,
}

/// One turn of a transcript, as a person would recognise it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    pub id: TurnId,
    /// What was asked, when the turn opened with a line of yours.
    pub asked: Option<String>,
    /// How many items it left behind, which is what going back would drop.
    pub items: usize,
}

impl Turn {
    /// What the row says: the line that started the turn, else the turn's own
    /// name for one nobody opened by typing.
    fn label(&self) -> String {
        match &self.asked {
            Some(asked) => asked.clone(),
            None => self.id.as_str().to_string(),
        }
    }
}

/// The turns of this transcript, newest first.
pub fn turns(state: &SessionState) -> Vec<Turn> {
    let mut out: Vec<Turn> = Vec::new();
    for item in &state.items {
        let Some(id) = item.turn.clone() else {
            continue;
        };
        match out.last_mut() {
            Some(turn) if turn.id == id => turn.items += 1,
            _ => out.push(Turn {
                id,
                asked: None,
                items: 1,
            }),
        }
        if let (Some(turn), Some(asked)) = (out.last_mut(), asked(item)) {
            turn.asked.get_or_insert(asked);
        }
    }
    out.reverse();
    out
}

/// The line a person typed, when the item is one.
fn asked(item: &bingo_sdk::Item) -> Option<String> {
    let ItemBody::User { parts, .. } = &item.body else {
        return None;
    };
    let text = parts
        .iter()
        .filter_map(ContentPart::as_text)
        .collect::<Vec<_>>()
        .join("");
    let line = text.lines().next().unwrap_or_default().trim().to_string();
    (!line.is_empty()).then_some(line)
}

/// Whether this session has a `/rewind` to run. Nothing is offered that could
/// not be done.
pub fn offered(commands: &[CommandSpec]) -> bool {
    commands
        .iter()
        .any(|spec| spec.name == COMMAND || spec.aliases.iter().any(|name| name == COMMAND))
}

/// The line a chosen row submits.
pub fn line(turn: &Turn) -> String {
    format!("/{COMMAND} {}", turn.id.as_str())
}

/// The card: a title, because unlike the switcher's its rows do not say what
/// they are, then one row per turn with what it dropped.
pub fn lines(turns: &[Turn], selected: usize) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(Span::styled(
        "Rewind to".to_string(),
        theme::text().patch(theme::bold()),
    ))];
    out.extend(turns.iter().take(ROWS).enumerate().map(|(at, turn)| {
        let focused = at == selected;
        let style = match focused {
            true => theme::text(),
            false => theme::dim(),
        };
        Line::from(vec![
            theme::cursor_span(focused),
            Span::styled(turn.label(), style),
            Span::styled(dropped(turn.items), theme::dim()),
        ])
    }));
    out
}

/// What going back to a turn would take with it.
fn dropped(items: usize) -> String {
    match items {
        1 => " · 1 item".to_string(),
        many => format!(" · {many} items"),
    }
}

/// How many rows the card can be walked through.
pub fn rows(turns: &[Turn]) -> usize {
    turns.len().min(ROWS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use bingo_sdk::{ArgSpec, ItemStatus};

    fn spec(name: &str) -> CommandSpec {
        CommandSpec {
            name: name.into(),
            aliases: Vec::new(),
            hint: String::new(),
            args: ArgSpec::None,
            instant: true,
            family: "session".into(),
        }
    }

    fn transcript() -> SessionState {
        let mut state = state();
        state.items = vec![
            in_turn(
                "itm_1",
                "trn_1",
                user("itm_1", "what is in this workspace?"),
            ),
            in_turn(
                "itm_2",
                "trn_1",
                assistant("itm_2", "One package.", ItemStatus::Completed),
            ),
            in_turn("itm_3", "trn_2", user("itm_3", "write me a note")),
        ];
        state
    }

    /// The same item, stamped with the turn it belongs to.
    fn in_turn(id: &str, turn: &str, mut item: bingo_sdk::Item) -> bingo_sdk::Item {
        item.id = bingo_sdk::ItemId::from_raw(id);
        item.turn = Some(TurnId::from_raw(turn));
        item
    }

    #[test]
    fn the_turns_are_the_transcripts_own_boundaries_newest_first() {
        let turns = turns(&transcript());
        assert_eq!(
            turns
                .iter()
                .map(|turn| (turn.id.as_str().to_string(), turn.items))
                .collect::<Vec<_>>(),
            vec![("trn_2".to_string(), 1), ("trn_1".to_string(), 2)],
        );
        assert_eq!(turns[0].asked.as_deref(), Some("write me a note"));
        assert_eq!(
            turns[1].asked.as_deref(),
            Some("what is in this workspace?"),
        );
    }

    #[test]
    fn a_turn_nobody_opened_by_typing_is_named_by_its_own_id() {
        let mut state = state();
        state.items = vec![in_turn(
            "itm_1",
            "trn_9",
            assistant("itm_1", "on my own", ItemStatus::Completed),
        )];
        assert_eq!(turns(&state)[0].label(), "trn_9");
    }

    #[test]
    fn the_card_marks_the_row_the_keyboard_is_on() {
        let turns = turns(&transcript());
        let drawn: Vec<String> = lines(&turns, 1)
            .iter()
            .map(|line| line.to_string())
            .collect();
        assert_eq!(
            drawn,
            vec![
                "Rewind to".to_string(),
                "  write me a note · 1 item".to_string(),
                "❯ what is in this workspace? · 2 items".to_string(),
            ],
        );
    }

    #[test]
    fn nothing_is_offered_that_could_not_be_done() {
        assert!(!offered(&[spec("compact"), spec("model")]));
        assert!(offered(&[spec("compact"), spec(COMMAND)]));
        let mut aliased = spec("undo");
        aliased.aliases = vec![COMMAND.to_string()];
        assert!(offered(&[aliased]));
    }

    #[test]
    fn a_chosen_row_runs_the_command_on_its_own_turn() {
        assert_eq!(line(&turns(&transcript())[0]), "/rewind trn_2");
    }
}
