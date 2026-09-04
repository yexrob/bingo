//! `esc esc` on an empty composer: the turns of this transcript, newest first,
//! and `⏎` to go back to one (design §3 — the rewind picker is a card).
//!
//! The turns are derived from `state.items` every frame: an item carries the
//! turn it belongs to, so the boundaries are already in the transcript and
//! nothing here keeps a second list of them.
//!
//! The picker is offered only where the session has a `/rewind` to run, which
//! keeps it honest wherever it is not registered. Since M67 the checkpoints
//! plugin registers one (`docs/plans/M67-the-turn-that-can-be-undone.md`,
//! ADR-0045) and the chord is live; nothing here changed for it, which was
//! the point of asking the catalogue rather than assuming.

use bingo_sdk::{CommandSpec, ContentPart, ItemBody, SessionState, TurnId};
use ratatui::text::{Line, Span};

use crate::{theme, window};

/// The command a chosen row runs.
pub const COMMAND: &str = "rewind";

/// How many turns the card shows at once: the same eight rows the `/` menu
/// offers. A longer list is windowed under the cursor, not cut off at eight.
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
/// they are, then one row per turn with what it dropped. `room` is what the
/// card has to draw in; the turns take [`ROWS`] of what is left under the
/// title, and window themselves when there are more of them than that.
pub fn lines(turns: &[Turn], selected: usize, room: usize) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(Span::styled(
        "Rewind to".to_string(),
        theme::text().patch(theme::bold()),
    ))];
    let rows = turns
        .iter()
        .enumerate()
        .map(|(at, turn)| row(turn, at == selected))
        .collect();
    out.extend(window::around(
        rows,
        selected,
        room.saturating_sub(out.len()).min(ROWS),
    ));
    out
}

/// One turn as the card draws it: what was asked, and what going back to it
/// would take with it.
fn row(turn: &Turn, focused: bool) -> Line<'static> {
    let style = match focused {
        true => theme::text(),
        false => theme::dim(),
    };
    Line::from(vec![
        theme::cursor_span(focused),
        Span::styled(turn.label(), style),
        Span::styled(dropped(turn.items), theme::dim()),
    ])
}

/// What going back to a turn would take with it.
fn dropped(items: usize) -> String {
    match items {
        1 => " · 1 item".to_string(),
        many => format!(" · {many} items"),
    }
}

/// How many rows the card can be walked through: every turn there is. The
/// card windows them (§3), so the walk is no longer stopped at what fits.
pub fn rows(turns: &[Turn]) -> usize {
    turns.len()
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

    /// A turn a command opened goes back to the line that opened it. The
    /// kernel leads such a prompt with `/name args`, so the row is the two
    /// words a person would recognise rather than the page they expanded to.
    #[test]
    fn a_turn_a_command_opened_is_named_by_the_line_that_was_typed() {
        let mut state = state();
        state.items = vec![in_turn(
            "itm_1",
            "trn_1",
            delivered(
                "itm_1",
                "command",
                None,
                "/guide the wire format\n\nRead this before answering about bingo itself.",
            ),
        )];
        assert_eq!(turns(&state)[0].label(), "/guide the wire format");
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
        let drawn: Vec<String> = lines(&turns, 1, 20)
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

    /// Data-driven, both ways: a session whose catalogue has a `rewind` is
    /// offered the card, and one built without the plugin that registers it
    /// is silent.
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
