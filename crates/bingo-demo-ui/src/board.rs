//! The board: three rows and where each has got to. It is the whole of this
//! plugin's state, and it lives in the session's journal as one payload
//! (ADR-0011 §2), so `--continue` reads back what the last run left.
//!
//! [`Board::view`] is the brick everything else stands on: a pure function
//! from the board to what a person should see. It is asserted with
//! `assert_eq!` and never needs a terminal.

use bingo_sdk::{Action, ActionItem, View};
use serde::{Deserialize, Serialize};

/// What the board offers a person to do, and the command each fires.
pub const TICK: &str = "board.tick";
pub const RESET: &str = "board.reset";

/// This plugin's own word for an element the sdk has none for (ADR-0038 §1),
/// namespaced by the plugin that owns its shape.
pub const SPARKLINE: &str = "demo.sparkline";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum State {
    #[default]
    Pending,
    Running,
    Done,
}

impl State {
    /// Where a row goes next; `Done` is where it stops.
    fn next(self) -> Self {
        match self {
            State::Pending => State::Running,
            State::Running | State::Done => State::Done,
        }
    }

    /// How far along a row is, as a number a sparkline can plot.
    fn reached(self) -> u64 {
        match self {
            State::Pending => 0,
            State::Running => 1,
            State::Done => 2,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Running => "running",
            State::Done => "done",
        }
    }

    fn of_str(text: &str) -> Self {
        match text {
            "running" => State::Running,
            "done" => State::Done,
            _ => State::Pending,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub id: u64,
    pub task: String,
    #[serde(default)]
    pub state: State,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Board {
    pub rows: Vec<Row>,
}

impl Default for Board {
    fn default() -> Self {
        Self {
            rows: ["write the plan", "ship it", "tell the others"]
                .into_iter()
                .enumerate()
                .map(|(at, task)| Row {
                    id: at as u64 + 1,
                    task: task.into(),
                    state: State::default(),
                })
                .collect(),
        }
    }
}

impl Board {
    /// One press of `Tick`: the first row that is not done moves on. The whole
    /// board is published again, because the payload *is* the board.
    pub fn tick(&mut self) {
        if let Some(row) = self.rows.iter_mut().find(|row| row.state != State::Done) {
            row.state = row.state.next();
        }
    }

    /// What a person sees: a table of the rows, how far they have got as an
    /// element the sdk has no word for, and the buttons under both. Nothing
    /// here knows what a terminal is (ADR-0013 §4).
    pub fn view(&self) -> View {
        View::Panel {
            title: "Board".into(),
            child: Box::new(View::Stack {
                children: vec![self.table(), self.sparkline(), buttons()],
            }),
        }
    }

    /// The inverse of [`Board::view`]. What the journal holds is the view a
    /// person was shown, so the next run reads its board straight back out of
    /// it: one fact, one representation, and no shadow record to keep in step.
    pub fn of_view(view: &View) -> Option<Self> {
        let View::Panel { child, .. } = view else {
            return None;
        };
        let View::Stack { children } = child.as_ref() else {
            return None;
        };
        let table = children.iter().find_map(|child| match child {
            View::Table { rows, .. } => Some(rows),
            _ => None,
        })?;
        Some(Self {
            rows: table.iter().filter_map(|cells| row(cells)).collect(),
        })
    }

    /// The porch beside the house (ADR-0038 §4): an element this plugin has a
    /// word for and the sdk does not. A surface that learns `demo.sparkline`
    /// draws the points; every surface that has not — which today is all of
    /// them — reads the fold, so the plugin owes one that is honest.
    fn sparkline(&self) -> View {
        let points: Vec<u64> = self.rows.iter().map(|row| row.state.reached()).collect();
        View::Custom {
            kind: SPARKLINE.into(),
            fold: points
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(" "),
            data: serde_json::json!({ "points": points }),
        }
    }

    fn table(&self) -> View {
        View::Table {
            headers: vec!["id".into(), "task".into(), "state".into()],
            rows: self
                .rows
                .iter()
                .map(|row| {
                    vec![
                        row.id.to_string(),
                        row.task.clone(),
                        row.state.as_str().to_string(),
                    ]
                })
                .collect(),
        }
    }
}

/// One row of the table, back as the row it came from.
fn row(cells: &[String]) -> Option<Row> {
    Some(Row {
        id: cells.first()?.parse().ok()?,
        task: cells.get(1)?.clone(),
        state: State::of_str(cells.get(2)?),
    })
}

/// A plugin names labels and the command each fires; which key does it is the
/// surface's answer (ADR-0013 §4), so no key is named here.
fn buttons() -> View {
    View::Actions {
        items: [("Tick", TICK), ("Reset", RESET)]
            .into_iter()
            .map(|(label, name)| ActionItem {
                label: label.into(),
                action: Action {
                    name: name.into(),
                    args: serde_json::Value::Null,
                },
                key: None,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_board_has_three_rows_and_none_of_them_has_started() {
        let board = Board::default();
        assert_eq!(board.rows.len(), 3);
        assert!(board.rows.iter().all(|row| row.state == State::Pending));
    }

    #[test]
    fn a_tick_moves_the_first_row_that_is_not_done_and_then_the_next() {
        let mut board = Board::default();
        board.tick();
        assert_eq!(board.rows[0].state, State::Running);
        board.tick();
        assert_eq!(board.rows[0].state, State::Done);
        board.tick();
        assert_eq!(board.rows[1].state, State::Running);
    }

    #[test]
    fn every_tick_of_a_finished_board_leaves_it_as_it_is() {
        let mut board = Board::default();
        for _ in 0..20 {
            board.tick();
        }
        let done = board.clone();
        board.tick();
        assert_eq!(board, done);
    }

    /// The plugin's UI is a value: this is the whole of what it shows, with
    /// no terminal anywhere near it.
    #[test]
    fn the_view_is_a_panel_of_a_table_and_its_buttons() {
        let mut board = Board::default();
        board.tick();
        let View::Panel { title, child } = board.view() else {
            panic!("a board is a panel");
        };
        assert_eq!(title, "Board");
        let View::Stack { children } = *child else {
            panic!("a table with its buttons under it");
        };
        assert_eq!(
            children[0],
            View::Table {
                headers: vec!["id".into(), "task".into(), "state".into()],
                rows: vec![
                    vec!["1".into(), "write the plan".into(), "running".into()],
                    vec!["2".into(), "ship it".into(), "pending".into()],
                    vec!["3".into(), "tell the others".into(), "pending".into()],
                ],
            }
        );
        let View::Actions { items } = &children[2] else {
            panic!("buttons");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].action.name, TICK);
        assert_eq!(items[0].key, None, "which key fires it is the surface's");
    }

    /// The element the sdk has no word for, beside the ones it does: what it
    /// plots is this plugin's business, what every surface shows until one
    /// learns the word is the fold.
    #[test]
    fn the_board_shows_one_element_the_sdk_has_no_word_for() {
        let mut board = Board::default();
        board.tick();
        board.tick();
        board.tick();
        assert_eq!(
            board.sparkline(),
            View::Custom {
                kind: SPARKLINE.into(),
                data: serde_json::json!({"points": [2, 1, 0]}),
                fold: "2 1 0".into(),
            }
        );
        assert_eq!(board.view().fold().lines().nth(5), Some("2 1 0"));
    }

    /// The payload is the view, and the view is the board: what the journal
    /// carries is exactly what a person saw.
    #[test]
    fn the_view_is_the_payload_and_the_board_reads_back_out_of_it() {
        let mut board = Board::default();
        board.tick();
        board.tick();
        let payload = serde_json::to_value(board.view()).expect("a view is json");
        let read: View = serde_json::from_value(payload).expect("read back");
        assert_eq!(Board::of_view(&read), Some(board));
    }

    #[test]
    fn a_view_that_is_not_a_board_is_not_read_as_one() {
        assert_eq!(Board::of_view(&View::text("hello")), None);
        assert_eq!(
            Board::of_view(&View::Panel {
                title: "Board".into(),
                child: Box::new(View::text("empty")),
            }),
            None
        );
    }
}
