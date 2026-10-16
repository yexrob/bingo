use bingo_collab::{lifecycle::Lifecycle, name::of_session};
use bingo_sdk::{
    spent::{Price, spent},
    *,
};
use bingo_services::{
    agents_control::AgentRow,
    rooms_control::RoomRow,
    tasks_write::{Status, TaskRow},
};
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub line: String,
    pub details: Vec<String>,
}
pub fn agent_card(row: &AgentRow, live: Option<&SessionState>, price: Option<Price>) -> Card {
    let summary = live.map_or(&row.summary, |state| &state.summary);
    let state = activity(row, live);
    let glyph = match state {
        Some(Lifecycle::Working) => "●",
        Some(Lifecycle::Asking) => "?",
        Some(Lifecycle::Stopped) => "■",
        _ => "○",
    };
    let state = state.map_or("", Lifecycle::word);
    let spent = spent(summary.usage, price);
    let cost = spent.usd.map_or("$—".into(), |usd| format!("${usd:.2}"));
    let model = summary.model.as_deref().unwrap_or("—");
    let mut details = vec![
        format!(
            "asked  {}",
            summary.history.first_ask.as_deref().unwrap_or("—")
        ),
        format!("model  {model}"),
    ];
    if let Some(live) = live {
        if let Some(tool) = current_tool(live) {
            details.push(format!("tool   {tool}"));
        }
        if let Some(last) = last_line(live) {
            details.push(format!("last   {last}"));
        }
    }
    Card {
        line: format!(
            "{}   {glyph} {state}   {model}  {} · {cost}",
            of_session(summary),
            tokens(spent.input.saturating_add(spent.output))
        ),
        details,
    }
}
pub fn activity(row: &AgentRow, live: Option<&SessionState>) -> Option<Lifecycle> {
    Lifecycle::activity(&row.summary, live)
}
fn tokens(n: u64) -> String {
    if n >= 1000 {
        format!("{}k", n / 1000)
    } else {
        n.to_string()
    }
}
pub fn last_line(state: &SessionState) -> Option<String> {
    state
        .transcript
        .items()
        .iter()
        .rev()
        .find_map(|item| match &item.body {
            ItemBody::Assistant { text } => text
                .lines()
                .rfind(|s| !s.trim().is_empty())
                .map(str::to_string),
            _ => None,
        })
}
fn current_tool(state: &SessionState) -> Option<String> {
    state
        .transcript
        .items()
        .iter()
        .rev()
        .find_map(|item| match &item.body {
            ItemBody::ToolCall {
                name, output: None, ..
            } if state.busy() => Some(name.clone()),
            _ => None,
        })
}
pub fn room_card(row: &RoomRow) -> Card {
    Card {
        line: format!(
            "{}   {}",
            row.name,
            if row.closed {
                "closed".into()
            } else {
                row.purpose.clone().unwrap_or_else(|| "—".into())
            }
        ),
        details: vec![
            format!("seats   {}", row.seats.join(", ")),
            format!("holder  {}", row.holder),
        ],
    }
}
pub fn task_card(row: &TaskRow) -> Card {
    Card {
        line: format!(
            "{} #{}  {}  @{}",
            if row.status == Status::Completed {
                "☑"
            } else {
                "☐"
            },
            row.id,
            row.subject,
            row.owner.as_deref().unwrap_or("unassigned")
        ),
        details: vec![format!(
            "status  {}",
            match row.status {
                Status::Pending => "pending",
                Status::InProgress => "in progress",
                Status::Completed => "completed",
            }
        )],
    }
}

pub fn team_card(row: &bingo_services::agents_control::TeamRow) -> Card {
    let state = row
        .summary
        .as_ref()
        .map_or(Some(Lifecycle::Unseated), Lifecycle::of_summary)
        .map_or("", Lifecycle::word);
    Card {
        line: format!("{}   {state}", row.role),
        details: vec![
            format!("agent    {}", row.agent.as_deref().unwrap_or("-")),
            format!(
                "session  {}",
                row.summary
                    .as_ref()
                    .map_or("-", |summary| summary.id.as_str())
            ),
        ],
    }
}
