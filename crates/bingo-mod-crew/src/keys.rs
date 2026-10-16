use crate::state::{Confirmation, Panel, Rows, Tab};
use bingo_sdk::SessionId;
use bingo_services::tasks_write::Status;
#[derive(Debug, PartialEq)]
pub enum Operation {
    Close,
    Tab,
    Move(bool),
    Model,
    View(SessionId),
    Pick(SessionId),
    Stop(String),
    Dismiss(String),
    Fill(String),
    Confirm(Confirmation),
    CloseRoom(String),
    Tick(u64, Status),
    None,
}
pub fn operation(panel: &Panel, rows: &Rows, key: &str) -> Operation {
    if key == "esc" {
        return Operation::Close;
    }
    if let Some(name) = &panel.confirm {
        return if key == "enter" {
            match name {
                Confirmation::Stop(name) => Operation::Stop(name.clone()),
                Confirmation::Dismiss(name) => Operation::Dismiss(name.clone()),
                Confirmation::CloseRoom(name) => Operation::CloseRoom(name.clone()),
            }
        } else {
            Operation::None
        };
    }
    match key {
        "]" => return Operation::Tab,
        "up" => return Operation::Move(false),
        "down" => return Operation::Move(true),
        _ => {}
    }
    match panel.tab {
        Tab::Agents => agent(panel, rows, key),
        Tab::Rooms => room(panel, rows, key),
        Tab::Tasks => task(panel, rows, key),
        Tab::Team => Operation::None,
    }
}
fn agent(panel: &Panel, rows: &Rows, key: &str) -> Operation {
    let Some(row) = rows.agents.as_ref().and_then(|r| r.get(panel.selected)) else {
        return Operation::None;
    };
    match key {
        "left" | "right" => Operation::Model,
        "enter" if panel.model => Operation::Pick(row.summary.id.clone()),
        "enter" => Operation::View(row.summary.id.clone()),
        "s" => Operation::Confirm(Confirmation::Stop(
            bingo_collab::name::of_session(&row.summary).into(),
        )),
        "x" => Operation::Confirm(Confirmation::Dismiss(
            bingo_collab::name::of_session(&row.summary).into(),
        )),
        "m" => {
            let live = panel.follow.as_ref().and_then(|f| f.state.lock().ok());
            let stopped = crate::cards::activity(row, live.as_deref())
                == Some(bingo_collab::lifecycle::Lifecycle::Stopped);
            Operation::Fill(format!(
                "/agents {} {} ",
                if stopped { "reopen" } else { "message" },
                bingo_collab::name::of_session(&row.summary)
            ))
        }
        _ => Operation::None,
    }
}
fn room(panel: &Panel, rows: &Rows, key: &str) -> Operation {
    let Some(row) = rows.rooms.as_ref().and_then(|r| r.get(panel.selected)) else {
        return Operation::None;
    };
    match key {
        "enter" => Operation::View(row.id.clone()),
        "p" => Operation::Fill(format!("/rooms purpose {} ", row.name)),
        "a" => Operation::Fill(format!("/rooms seat {} ", row.name)),
        "u" => Operation::Fill(format!("/rooms unseat {} ", row.name)),
        "c" => Operation::Confirm(Confirmation::CloseRoom(row.name.clone())),
        _ => Operation::None,
    }
}
fn task(panel: &Panel, rows: &Rows, key: &str) -> Operation {
    if key == "+" || key == "shift++" {
        return Operation::Fill("/tasks add ".into());
    }
    let Some(row) = rows.tasks.as_ref().and_then(|r| r.get(panel.selected)) else {
        return Operation::None;
    };
    match key {
        " " | "space" => Operation::Tick(
            row.id,
            if row.status == Status::Completed {
                Status::Pending
            } else {
                Status::Completed
            },
        ),
        "o" => Operation::Fill(format!("/tasks assign {} ", row.id)),
        _ => Operation::None,
    }
}
