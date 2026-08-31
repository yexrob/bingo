//! The fixtures of M11d: the three lanes of ADR-0013 as frames, the demo
//! plugin's board as a value, and the pins a person makes. They are kept
//! beside [`crate::test_support`] and re-exported from it, so a test still
//! reaches every fixture through one import.

use bingo_sdk::{Event, SessionId, SessionState, View};
use serde_json::Value;

use crate::test_support::{extended, folded, frame, user};
use crate::ui::Ui;

/// A plugin publishing live state onto the stream: never journaled, gone on
/// a resume, replaced in place by the next one (ADR-0013 §2).
pub fn signalled(plugin: &str, kind: &str, payload: Value) -> Event {
    Event::Signal {
        plugin: plugin.into(),
        kind: kind.into(),
        payload,
    }
}

/// One node as a payload: what a plugin puts on the wire when it describes a
/// screen rather than a record.
pub fn as_payload(view: View) -> Value {
    serde_json::to_value(view).expect("a view is serialisable")
}

pub fn progress_view(value: u64, total: u64) -> Value {
    as_payload(View::Progress {
        value,
        total: Some(total),
        label: Some("cargo test".into()),
    })
}

/// The board the demo plugin publishes: a table with buttons under it, which
/// is the shape every interactive panel has.
pub fn board_view() -> View {
    View::Panel {
        title: "Board".into(),
        child: Box::new(View::Stack {
            children: vec![
                View::Table {
                    headers: vec!["id".into(), "state".into()],
                    rows: vec![
                        vec!["1".into(), "ready".into()],
                        vec!["2".into(), "held".into()],
                    ],
                },
                View::Actions {
                    items: vec![
                        bingo_sdk::ActionItem {
                            label: "Tick".into(),
                            action: bingo_sdk::Action {
                                name: "board.tick".into(),
                                args: Value::Null,
                            },
                            key: None,
                        },
                        bingo_sdk::ActionItem {
                            label: "Reset".into(),
                            action: bingo_sdk::Action {
                                name: "board.reset".into(),
                                args: Value::Null,
                            },
                            key: None,
                        },
                    ],
                },
            ],
        }),
    }
}

/// A session the demo plugin has written to in two lanes at once: a board it
/// journaled, and the progress it is publishing as it goes.
pub fn boarded() -> SessionState {
    folded(vec![
        frame(
            1,
            Event::ItemCompleted {
                item: user("itm_1", "run the board"),
            },
        ),
        frame(
            2,
            extended("bingo.demo.ui", "board", as_payload(board_view())),
        ),
        frame(
            3,
            signalled("bingo.demo.ui", "progress", progress_view(24, 30)),
        ),
    ])
}

/// One of the demo plugin's cards, by the kind it published under.
pub fn demo_card(kind: &str) -> crate::rail::CardId {
    crate::rail::CardId {
        plugin: "bingo.demo.ui".into(),
        kind: kind.into(),
    }
}

/// A person who pinned the board into the root session's rail.
pub fn pin_board(ui: &mut Ui) {
    ui.pinned.insert(crate::rail::Pin {
        session: SessionId::from_raw("ses_1"),
        card: demo_card("board"),
    });
}
