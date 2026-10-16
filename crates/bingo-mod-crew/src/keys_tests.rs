use crate::{
    keys::*,
    state::{Confirmation, Panel, Rows, Tab},
};
use bingo_sdk::*;
use bingo_services::{
    agents_control::AgentRow,
    rooms_control::RoomRow,
    tasks_write::{Status, TaskRow},
};

fn rows() -> Rows {
    Rows {
        team: None,
        agents: Some(vec![AgentRow {
            summary: serde_json::from_value(serde_json::json!({
                "id":"ses_child","title":"reviewer","cwd":"/work",
                "createdAt":"2026-10-15T00:00:00Z","updatedAt":"2026-10-15T00:00:00Z"
            }))
            .unwrap(),
        }]),
        rooms: Some(vec![RoomRow {
            id: SessionId::from_raw("ses_room"),
            name: "#design".into(),
            purpose: None,
            seats: vec![],
            holder: "parent".into(),
            closed: false,
        }]),
        tasks: Some(vec![TaskRow {
            id: 1,
            subject: "Review".into(),
            status: Status::Pending,
            owner: None,
        }]),
    }
}
#[test]
fn navigation_and_empty_board_keys_are_total() {
    let rows = Rows::default();
    for tab in [Tab::Agents, Tab::Rooms, Tab::Tasks, Tab::Team] {
        let panel = Panel {
            tab,
            ..Default::default()
        };
        for (key, expected) in [
            ("esc", Operation::Close),
            ("up", Operation::Move(false)),
            ("down", Operation::Move(true)),
            ("]", Operation::Tab),
            ("enter", Operation::None),
            ("s", Operation::None),
        ] {
            assert_eq!(operation(&panel, &rows, key), expected);
        }
    }
    assert_eq!(
        operation(
            &Panel {
                tab: Tab::Tasks,
                ..Default::default()
            },
            &rows,
            "+"
        ),
        Operation::Fill("/tasks add ".into())
    );
}
#[test]
fn agent_keys_view_stop_message_pick_and_confirm_dismissal() {
    let rows = rows();
    let panel = Panel::default();
    for (key, expected) in [
        (
            "s",
            Operation::Confirm(Confirmation::Stop("reviewer".into())),
        ),
        (
            "x",
            Operation::Confirm(Confirmation::Dismiss("reviewer".into())),
        ),
        ("m", Operation::Fill("/agents message reviewer ".into())),
        ("enter", Operation::View(SessionId::from_raw("ses_child"))),
    ] {
        assert_eq!(operation(&panel, &rows, key), expected);
    }
    assert_eq!(
        operation(
            &Panel {
                model: true,
                ..panel
            },
            &rows,
            "enter"
        ),
        Operation::Pick(SessionId::from_raw("ses_child"))
    );
}
#[test]
fn room_keys_prefill_or_confirm() {
    let rows = rows();
    let panel = Panel {
        tab: Tab::Rooms,
        ..Default::default()
    };
    assert_eq!(
        operation(&panel, &rows, "enter"),
        Operation::View(SessionId::from_raw("ses_room"))
    );
    for (key, verb) in [("p", "purpose"), ("a", "seat"), ("u", "unseat")] {
        assert_eq!(
            operation(&panel, &rows, key),
            Operation::Fill(format!("/rooms {verb} #design "))
        );
    }
    assert_eq!(
        operation(&panel, &rows, "c"),
        Operation::Confirm(Confirmation::CloseRoom("#design".into()))
    );
}
#[test]
fn task_keys_change_status_or_prefill_owner() {
    let rows = rows();
    let panel = Panel {
        tab: Tab::Tasks,
        ..Default::default()
    };
    assert_eq!(
        operation(&panel, &rows, " "),
        Operation::Tick(1, Status::Completed)
    );
    assert_eq!(
        operation(&panel, &rows, "o"),
        Operation::Fill("/tasks assign 1 ".into())
    );
}

#[test]
fn declared_team_roles_are_read_only() {
    let rows = Rows {
        team: Some(vec![bingo_services::agents_control::TeamRow {
            role: "reviewer".into(),
            agent: None,
            summary: None,
        }]),
        ..Default::default()
    };
    assert_eq!(rows.tabs(), [Tab::Team]);
    assert_eq!(rows.len(Tab::Team), 1);
    assert_eq!(rows.identity(Tab::Team, 0), "reviewer");
    let panel = Panel {
        tab: Tab::Team,
        ..Default::default()
    };
    for key in ["enter", "s", "x", "m", "+", "space"] {
        assert_eq!(operation(&panel, &rows, key), Operation::None);
    }
}
#[test]
fn confirmations_hold_the_target_and_require_enter() {
    for (confirm, expected) in [
        (
            Confirmation::Stop("reviewer".into()),
            Operation::Stop("reviewer".into()),
        ),
        (
            Confirmation::CloseRoom("#design".into()),
            Operation::CloseRoom("#design".into()),
        ),
        (
            Confirmation::Dismiss("reviewer".into()),
            Operation::Dismiss("reviewer".into()),
        ),
    ] {
        let panel = Panel {
            confirm: Some(confirm),
            ..Default::default()
        };
        for key in ["]", "up", "down", "m", "s", "x"] {
            assert_eq!(operation(&panel, &Rows::default(), key), Operation::None);
        }
        assert_eq!(operation(&panel, &Rows::default(), "esc"), Operation::Close);
        assert_eq!(operation(&panel, &Rows::default(), "enter"), expected);
    }
}
