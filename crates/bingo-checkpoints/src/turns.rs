//! The turns of a transcript, as a person picking one recognises them.
//!
//! Derived from the items every time it is asked: an item carries the turn it
//! belongs to, so the boundaries are already in the transcript and nothing
//! here keeps a list beside it.

use bingo_sdk::{ContentPart, Item, ItemBody, SessionState, TurnId};

/// One turn, in transcript order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    pub id: TurnId,
    /// The first line of what was asked, when a person's own line opened it.
    pub asked: Option<String>,
}

impl Turn {
    /// What a row calls it: the line that opened it, else its own id.
    pub fn label(&self) -> String {
        self.asked
            .clone()
            .unwrap_or_else(|| self.id.as_str().to_string())
    }
}

/// The turns of this transcript, oldest first.
pub fn of(state: &SessionState) -> Vec<Turn> {
    let mut out: Vec<Turn> = Vec::new();
    for item in &state.items {
        let Some(id) = item.turn.clone() else {
            continue;
        };
        if out.last().is_none_or(|turn| turn.id != id) {
            out.push(Turn { id, asked: None });
        }
        if let (Some(turn), Some(asked)) = (out.last_mut(), asked(item)) {
            turn.asked.get_or_insert(asked);
        }
    }
    out
}

/// This turn and every later one, which is what going back to it undoes.
pub fn from(turns: &[Turn], to_turn: &TurnId) -> Option<Vec<TurnId>> {
    let at = turns.iter().position(|turn| &turn.id == to_turn)?;
    Some(turns[at..].iter().map(|turn| turn.id.clone()).collect())
}

/// The line a person typed, when the item is one.
fn asked(item: &Item) -> Option<String> {
    let ItemBody::User { parts, .. } = &item.body else {
        return None;
    };
    let text: String = parts
        .iter()
        .filter_map(ContentPart::as_text)
        .collect::<Vec<_>>()
        .join("");
    let line = text.lines().next().unwrap_or_default().trim().to_string();
    (!line.is_empty()).then_some(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::transcript;

    #[test]
    fn the_turns_are_the_transcripts_own_boundaries_oldest_first() {
        let turns = of(&transcript());
        assert_eq!(
            turns.iter().map(Turn::label).collect::<Vec<_>>(),
            ["write the note", "and rename it"]
        );
    }

    #[test]
    fn a_turn_nobody_opened_by_typing_is_named_by_its_own_id() {
        let mut state = transcript();
        state.items[0].body = ItemBody::Assistant {
            text: "on my own".into(),
        };
        assert_eq!(of(&state)[0].label(), "trn_1");
    }

    #[test]
    fn going_back_to_a_turn_undoes_it_and_every_later_one() {
        let turns = of(&transcript());
        assert_eq!(
            from(&turns, &TurnId::from_raw("trn_1")),
            Some(vec![TurnId::from_raw("trn_1"), TurnId::from_raw("trn_2")])
        );
        assert_eq!(
            from(&turns, &TurnId::from_raw("trn_2")),
            Some(vec![TurnId::from_raw("trn_2")])
        );
        assert_eq!(from(&turns, &TurnId::from_raw("trn_9")), None);
    }
}
