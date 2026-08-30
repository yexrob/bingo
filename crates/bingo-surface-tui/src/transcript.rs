//! Items to styled lines. The whole transcript is rebuilt every frame from
//! `state.items`: the reducer is the only history, and a cache would be a
//! second one.

use bingo_sdk::{
    ContentPart, DecisionKind, Item, ItemBody, ItemStatus, SessionState, ToolOutput, TurnStatus,
    View,
};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::tree::Agents;
use crate::{markdown, preview, theme, wrap};

/// Output lines shown under a tool row before it is folded.
const OUTPUT_ROWS: usize = 5;
/// Diff rows shown under a tool row.
const DIFF_ROWS: usize = 12;
/// The gutter that ties a result to the call above it.
const CONNECTOR: &str = "  ⎿  ";
const INDENT: &str = "     ";

/// The transcript, wrapped to `width`. `spinner` is the frame a running tool
/// shows and `agents` the sub-sessions this transcript's tool calls spawned;
/// the caller owns the clock and the tree.
pub fn lines(
    state: &SessionState,
    agents: &Agents,
    width: usize,
    spinner: &str,
) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    for item in &state.items {
        let mut block = item_lines(item, width, spinner);
        if block.is_empty() {
            continue;
        }
        if let Some(agent) = agents.get(&item.id) {
            block.push(child_line(agent));
        }
        if !out.is_empty() {
            out.push(Line::default());
        }
        out.extend(wrap::wrap_all(&block, width));
    }
    out.extend(failure(state));
    out
}

/// A turn that failed says why, derived from `last_turn` rather than kept as a
/// line of the surface's own.
fn failure(state: &SessionState) -> Vec<Line<'static>> {
    let Some(TurnStatus::Failed { error }) = state.last_turn.as_ref().filter(|_| !state.busy())
    else {
        return Vec::new();
    };
    vec![
        Line::default(),
        Line::from(Span::styled(
            format!("{} {}", theme::FAILED, error.message),
            theme::danger(),
        )),
    ]
}

/// The tool call that spawned a sub-session says so under its own row; what
/// the child is doing is read from its state, never copied into this one.
fn child_line(agent: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {} {agent}", theme::CHILD),
        theme::dim(),
    ))
}

fn item_lines(item: &Item, width: usize, spinner: &str) -> Vec<Line<'static>> {
    match &item.body {
        ItemBody::User { parts, origin } => user(parts, origin.principal.as_deref()),
        ItemBody::Assistant { text } => markdown::render(text, width),
        ItemBody::Reasoning { .. } => vec![Line::from(Span::styled(
            format!("{}thinking…", theme::THINKING),
            theme::dim(),
        ))],
        ItemBody::ToolCall {
            name,
            input,
            output,
            progress,
            ..
        } => tool_call(
            item.status,
            name,
            input,
            output.as_ref(),
            progress.as_deref(),
            spinner,
        ),
        ItemBody::Action { name, args, result } => action(name, args, result.as_ref()),
        ItemBody::Compaction { before, after, .. } => vec![rule(
            &format!("context compacted ({before} → {after} tokens)"),
            width,
        )],
        ItemBody::Rewind { dropped, .. } => {
            vec![rule(&format!("rewound, {dropped} items dropped"), width)]
        }
        ItemBody::Interruption { marker } => {
            vec![Line::from(Span::styled(marker.clone(), theme::dim()))]
        }
        ItemBody::Notice { level, text, .. } => {
            vec![Line::from(Span::styled(text.clone(), theme::level(*level)))]
        }
        ItemBody::QuestionAnswer {
            question, answer, ..
        } => vec![
            Line::from(Span::styled(format!("Q {question}"), theme::dim())),
            Line::from(Span::styled(format!("A {answer}"), theme::dim())),
        ],
        ItemBody::PermissionReceipt {
            tool,
            decision,
            feedback,
            ..
        } => vec![Line::from(Span::styled(
            receipt(tool, *decision, feedback.as_deref()),
            theme::dim(),
        ))],
        ItemBody::Asset { asset, label } => vec![Line::from(Span::styled(
            format!("[{}]", label.clone().unwrap_or_else(|| asset.clone())),
            theme::dim(),
        ))],
    }
}

/// A person's own line, and a post somebody else wrote. An origin that names
/// a principal is somebody speaking — a room's member, a parent talking to its
/// child — so the transcript says who, as a chat does. Where they said it is
/// the view one is looking at; saying it again would be noise.
fn user(parts: &[ContentPart], principal: Option<&str>) -> Vec<Line<'static>> {
    let text = parts
        .iter()
        .filter_map(ContentPart::as_text)
        .collect::<Vec<_>>()
        .join("");
    if text.trim().is_empty() {
        return Vec::new();
    }
    text.lines()
        .enumerate()
        .map(|(i, line)| {
            let mut spans = vec![Span::styled(
                if i == 0 { theme::USER } else { "  " },
                theme::dim(),
            )];
            if i == 0
                && let Some(name) = principal
            {
                spans.push(Span::styled(format!("{name}: "), theme::accent()));
            }
            spans.push(Span::raw(line.to_string()));
            Line::from(spans)
        })
        .collect()
}

fn tool_call(
    status: ItemStatus,
    name: &str,
    input: &Value,
    output: Option<&ToolOutput>,
    progress: Option<&str>,
    spinner: &str,
) -> Vec<Line<'static>> {
    let failed = status == ItemStatus::Failed || output.is_some_and(|o| o.is_error);
    let (glyph, style) = marker(status, failed, spinner);
    let mut header = vec![
        Span::styled(format!("{glyph} "), style),
        Span::raw(name.to_string()),
    ];
    let summary = summarize(input);
    if !summary.is_empty() {
        header.push(Span::styled(format!(" {summary}"), theme::dim()));
    }
    let mut out = vec![Line::from(header)];
    if status == ItemStatus::Running
        && let Some(progress) = progress
    {
        out.push(gutter(0, Span::styled(progress.to_string(), theme::dim())));
    }
    out.extend(output.map(output_lines).unwrap_or_default());
    out
}

fn marker(status: ItemStatus, failed: bool, spinner: &str) -> (String, ratatui::style::Style) {
    if failed {
        return (theme::FAILED.into(), theme::danger());
    }
    match status {
        ItemStatus::Pending => (theme::TOOL.trim_end().into(), theme::dim()),
        ItemStatus::Running => (spinner.into(), theme::accent()),
        ItemStatus::Completed => (theme::DONE.into(), theme::good()),
        ItemStatus::Failed => (theme::FAILED.into(), theme::danger()),
        ItemStatus::Interrupted => (theme::STOPPED.into(), theme::caution()),
    }
}

/// What the call is about, from the field a person would recognise.
fn summarize(input: &Value) -> String {
    for key in ["file_path", "command", "pattern", "url", "query"] {
        if let Some(Value::String(value)) = input.get(key) {
            return value.clone();
        }
    }
    match input {
        Value::Object(map) => map
            .iter()
            .find_map(|(_, v)| v.as_str())
            .unwrap_or_default()
            .to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn output_lines(output: &ToolOutput) -> Vec<Line<'static>> {
    let (rows, limit) = match &output.display {
        Some(View::Diff { unified }) => (preview::diff(unified), DIFF_ROWS),
        Some(view) => (plain(&view.fold()), OUTPUT_ROWS),
        None => (plain(&text_of(output)), OUTPUT_ROWS),
    };
    fold(rows, limit)
}

fn plain(text: &str) -> Vec<Line<'static>> {
    text.trim_end()
        .lines()
        .map(|line| Line::from(Span::styled(line.to_string(), theme::dim())))
        .collect()
}

fn text_of(output: &ToolOutput) -> String {
    output
        .parts
        .iter()
        .filter_map(ContentPart::as_text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The first rows under the gutter, plus how many were left out.
fn fold(rows: Vec<Line<'static>>, limit: usize) -> Vec<Line<'static>> {
    let hidden = rows.len().saturating_sub(limit);
    let mut out: Vec<Line<'static>> = rows
        .into_iter()
        .take(limit)
        .enumerate()
        .map(|(i, line)| gutter_line(i, line))
        .collect();
    if hidden > 0 {
        out.push(gutter(
            out.len(),
            Span::styled(format!("… +{hidden} lines"), theme::dim()),
        ));
    }
    out
}

fn gutter(index: usize, span: Span<'static>) -> Line<'static> {
    gutter_line(index, Line::from(span))
}

fn gutter_line(index: usize, line: Line<'static>) -> Line<'static> {
    let lead = if index == 0 { CONNECTOR } else { INDENT };
    let mut spans = vec![Span::styled(lead, theme::dim())];
    spans.extend(line.spans);
    Line::from(spans)
}

/// A command's record: `!` and the line it ran, then what it printed.
fn action(name: &str, args: &Value, result: Option<&Value>) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(vec![
        Span::styled(format!("{name} "), theme::accent()),
        Span::raw(as_text(args)),
    ])];
    if let Some(result) = result {
        out.extend(fold(plain(&as_text(result)), OUTPUT_ROWS));
    }
    out
}

/// Strings travel verbatim; anything else as compact JSON.
fn as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn receipt(tool: &str, decision: DecisionKind, feedback: Option<&str>) -> String {
    let verdict = match decision {
        DecisionKind::Allow => "allowed",
        DecisionKind::AllowSession => "allowed for this session",
        DecisionKind::Deny => "denied",
    };
    match feedback {
        Some(feedback) => format!("{tool} {verdict} — {feedback}"),
        None => format!("{tool} {verdict}"),
    }
}

/// A full-width divider with its reason in the middle of the left run.
fn rule(text: &str, width: usize) -> Line<'static> {
    let head = format!("─── {text} ");
    let tail = width.saturating_sub(head.chars().count());
    Line::from(Span::styled(
        format!("{head}{}", "─".repeat(tail)),
        theme::dim(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{folded, frame, post, user as person};
    use bingo_sdk::Event;

    fn drawn(items: Vec<Item>) -> Vec<String> {
        let frames = items
            .into_iter()
            .enumerate()
            .map(|(i, item)| frame(i as u64 + 1, Event::ItemCompleted { item }))
            .collect();
        lines(&folded(frames), &Agents::new(), 60, "⠋")
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn a_post_says_who_wrote_it_and_a_persons_own_line_does_not() {
        assert_eq!(
            drawn(vec![
                post("itm_1", "reviewer", "two nits, otherwise fine"),
                person("itm_2", "thanks"),
            ]),
            vec![
                "❯ reviewer: two nits, otherwise fine".to_string(),
                String::new(),
                "❯ thanks".to_string(),
            ],
        );
    }

    #[test]
    fn the_room_a_post_came_from_is_the_view_it_is_read_in() {
        let drawn = drawn(vec![post("itm_1", "scout", "found it")]).join("\n");
        assert!(!drawn.contains("#design"), "{drawn}");
    }

    #[test]
    fn only_the_first_line_of_a_post_carries_the_name() {
        assert_eq!(
            drawn(vec![post("itm_1", "scout", "one\ntwo")]),
            vec!["❯ scout: one".to_string(), "  two".to_string()],
        );
    }
}
