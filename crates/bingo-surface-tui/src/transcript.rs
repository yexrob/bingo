//! One item to styled lines, in Claude Code's grammar (`docs/design/tui.md`
//! §4): `⏺` for what the model says and does, `⎿` for what came back, `>` on a
//! raised bar for what you said. The reducer is the only history: nothing here
//! remembers a thing, and [`crate::blocks`] stacks and memoises what it draws.

use bingo_sdk::{
    ContentPart, DecisionKind, Item, ItemBody, ItemStatus, SessionState, ToolOutput, TurnStatus,
    View,
};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use crate::tree::{self, Agents};
use crate::{markdown, paths, preview, theme, wrap};

/// Output rows kept under a finished tool row before the rest folds away.
const OUTPUT_ROWS: usize = 5;
/// A running tool's tail: enough to see it move, few enough to look past.
const TAIL_ROWS: usize = 3;
/// Diff rows kept under a tool row.
const DIFF_ROWS: usize = 12;
/// What opens a folded result. The key is the frame's; the words are here
/// because this is what is folded.
const EXPAND: &str = "ctrl+o to expand";

/// What every row of one transcript needs to know about where it is.
pub struct Rows<'a> {
    pub cwd: &'a str,
    pub width: usize,
}

impl Rows<'_> {
    /// Prose is read, not scanned: it stops at the measure (design §7).
    fn measure(&self) -> usize {
        wrap::measure(self.width)
    }

    /// A path as a person reads it, from this session's own directory.
    fn shorten(&self, text: &str) -> String {
        paths::shorten_in(text, self.cwd, paths::home())
    }
}

/// A receipt is the answer to the row above it, so it opens no block of its
/// own (design §4: the receipt joins the result).
pub fn joins_the_row_above(item: &Item) -> bool {
    matches!(item.body, ItemBody::PermissionReceipt { .. })
}

/// One item's block. `previous` is the item before it, which a receipt joins;
/// `agents` the sub-sessions this transcript's calls spawned.
pub fn item_lines(
    item: &Item,
    previous: Option<&Item>,
    agents: &Agents<'_>,
    rows: &Rows<'_>,
) -> Vec<Line<'static>> {
    match &item.body {
        ItemBody::User { parts, origin } => user(parts, origin.principal.as_deref(), rows),
        ItemBody::Assistant { text } => assistant(text, rows),
        ItemBody::Reasoning { .. } => thinking(item),
        ItemBody::ToolCall { .. } => called(item, agents, rows),
        ItemBody::Action { name, args, result } => {
            action(item.status, name, args, result.as_ref(), rows)
        }
        ItemBody::Compaction { before, after, .. } => vec![rule(
            &format!("context compacted ({before} → {after} tokens)"),
            rows.width,
        )],
        ItemBody::Rewind { dropped, .. } => {
            vec![rule(
                &format!("rewound, {dropped} items dropped"),
                rows.width,
            )]
        }
        ItemBody::Interruption { marker } => {
            vec![Line::from(Span::styled(marker.clone(), theme::dim()))]
        }
        ItemBody::Notice { level, text, .. } => {
            vec![Line::from(Span::styled(text.clone(), theme::level(*level)))]
        }
        ItemBody::QuestionAnswer {
            question, answer, ..
        } => answered(question, answer, rows),
        ItemBody::PermissionReceipt {
            tool,
            decision,
            feedback,
            ..
        } => receipt(tool, *decision, feedback.as_deref(), previous, rows),
        ItemBody::Asset { asset, label } => vec![Line::from(Span::styled(
            format!("[{}]", label.clone().unwrap_or_else(|| asset.clone())),
            theme::dim(),
        ))],
    }
}

/// A call that started a session is that session's row; every other call is
/// its own (design §3: a child is a row where it began).
fn called(item: &Item, agents: &Agents<'_>, rows: &Rows<'_>) -> Vec<Line<'static>> {
    let ItemBody::ToolCall {
        name,
        input,
        output,
        progress,
        ..
    } = &item.body
    else {
        return Vec::new();
    };
    match agents.get(&item.id) {
        Some(child) => child_row(child, input, rows),
        None => tool_call(
            Call {
                status: item.status,
                name,
                input,
                output: output.as_ref(),
                progress: progress.as_deref(),
            },
            rows,
        ),
    }
}

// ---- the two marks ------------------------------------------------------

/// Where the text under a `⏺` starts: column 2 (design §4).
fn speaks_indent() -> usize {
    theme::bullet().width() + 1
}

/// The `  ⎿  ` a result hangs from, and where its text starts: column 5.
fn connector() -> String {
    format!("  {}  ", theme::connector())
}

/// A block under a mark: the mark on its first row, its text at `indent` on
/// every row under it, wrapped so nothing overflows the measure.
fn under(
    mark: Span<'static>,
    body: Vec<Line<'static>>,
    indent: usize,
    width: usize,
) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(indent).max(1);
    let mut out: Vec<Line<'static>> = Vec::new();
    for line in wrap::wrap_all(&body, inner) {
        let lead = match out.is_empty() {
            true => mark.clone(),
            false => Span::raw(" ".repeat(indent)),
        };
        let mut spans = vec![lead];
        spans.extend(line.spans);
        out.push(Line::from(spans));
    }
    out
}

/// What the model says and does.
fn speaks(style: Style, body: Vec<Line<'static>>, rows: &Rows<'_>) -> Vec<Line<'static>> {
    let mark = Span::styled(format!("{} ", theme::bullet()), style);
    under(mark, body, speaks_indent(), rows.measure())
}

/// What came back.
fn returns(body: Vec<Line<'static>>, rows: &Rows<'_>) -> Vec<Line<'static>> {
    let mark = connector();
    let indent = mark.width();
    under(
        Span::styled(mark, theme::dim()),
        body,
        indent,
        rows.measure(),
    )
}

// ---- the kinds ----------------------------------------------------------

/// A person's own line, on a bar the width of the transcript. An origin that
/// names a principal is somebody else speaking — a room's member, a parent
/// talking to its child — so the line says who, as a chat does. Where they
/// said it is the view one is looking at; saying it again would be noise.
fn user(parts: &[ContentPart], principal: Option<&str>, rows: &Rows<'_>) -> Vec<Line<'static>> {
    let text = parts
        .iter()
        .filter_map(ContentPart::as_text)
        .collect::<Vec<_>>()
        .join("");
    if text.trim().is_empty() {
        return Vec::new();
    }
    let mut body: Vec<Line<'static>> = text
        .lines()
        .map(|line| Line::from(Span::styled(line.to_string(), theme::text())))
        .collect();
    if let Some(name) = principal
        && let Some(first) = body.first_mut()
    {
        first.spans.insert(
            0,
            Span::styled(format!("{name}: "), theme::text().patch(theme::bold())),
        );
    }
    let mark = Span::styled(format!("{} ", theme::user()), theme::dim());
    under(mark, body, speaks_indent(), rows.measure())
        .into_iter()
        .map(|line| bar(line, rows.width))
        .collect()
}

/// The raised bar behind a `>` line: it runs to the edge of the transcript,
/// so what you said is a band and not a sentence.
fn bar(line: Line<'static>, width: usize) -> Line<'static> {
    let mut spans = line.spans;
    let used: usize = spans.iter().map(|s| s.content.width()).sum();
    spans.push(Span::raw(" ".repeat(width.saturating_sub(used))));
    let mut line = Line::from(spans);
    line.style = theme::raised();
    line
}

/// The answer: the brightest text on the screen, after a white `⏺`.
fn assistant(text: &str, rows: &Rows<'_>) -> Vec<Line<'static>> {
    let body = markdown::render(text, rows.measure().saturating_sub(speaks_indent()));
    speaks(theme::text().patch(theme::bold()), body, rows)
}

/// `✻ Thinking…` while it lasts, `✻ Thought for 2s` once it is over.
fn thinking(item: &Item) -> Vec<Line<'static>> {
    let (text, style) = match item.completed_at {
        Some(end) => (
            format!(
                "Thought for {}s",
                end.duration_since(item.started_at).as_secs().max(0)
            ),
            theme::dim(),
        ),
        None => (
            format!("Thinking{}", theme::ellipsis()),
            theme::dim().patch(theme::italic()),
        ),
    };
    vec![Line::from(vec![
        Span::styled(format!("{} ", theme::spark()), theme::dim()),
        Span::styled(text, style),
    ])]
}

/// One tool call, as much of it as there is yet.
struct Call<'a> {
    status: ItemStatus,
    name: &'a str,
    input: &'a Value,
    output: Option<&'a ToolOutput>,
    progress: Option<&'a str>,
}

fn tool_call(call: Call<'_>, rows: &Rows<'_>) -> Vec<Line<'static>> {
    let failed = call.status == ItemStatus::Failed || call.output.is_some_and(|o| o.is_error);
    let mut out = speaks(
        bullet_style(call.status, failed),
        vec![signature(call.name, &summarize(call.input), rows)],
        rows,
    );
    out.extend(result(&call, rows));
    out
}

/// `Read(Cargo.toml)`: the name bold, what it is about plain.
fn signature(name: &str, about: &str, rows: &Rows<'_>) -> Line<'static> {
    let mut spans = vec![Span::styled(name.to_string(), theme::bold())];
    let about = rows.shorten(about);
    if !about.is_empty() {
        spans.push(Span::styled(format!("({about})"), theme::text()));
    }
    Line::from(spans)
}

/// The bullet carries the state, and nothing else has to.
fn bullet_style(status: ItemStatus, failed: bool) -> Style {
    if failed {
        return theme::bad();
    }
    match status {
        ItemStatus::Running => theme::presence(),
        ItemStatus::Completed => theme::good(),
        ItemStatus::Failed => theme::bad(),
        ItemStatus::Pending | ItemStatus::Interrupted => theme::dim(),
    }
}

/// What is under a tool row: its tail while it runs, its output once it is
/// done, folded to a line that says how much was left out.
fn result(call: &Call<'_>, rows: &Rows<'_>) -> Vec<Line<'static>> {
    if call.status == ItemStatus::Running {
        let Some(progress) = call.progress else {
            return Vec::new();
        };
        return returns(tail(progress), rows);
    }
    let Some(output) = call.output else {
        return Vec::new();
    };
    returns(folded(output), rows)
}

/// The last rows of what a running tool has printed so far.
fn tail(progress: &str) -> Vec<Line<'static>> {
    let all: Vec<&str> = progress.trim_end().lines().collect();
    plain(&all[all.len().saturating_sub(TAIL_ROWS)..].join("\n"))
}

fn folded(output: &ToolOutput) -> Vec<Line<'static>> {
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

/// The first rows, then how many were left out and what opens them.
fn fold(rows: Vec<Line<'static>>, limit: usize) -> Vec<Line<'static>> {
    let hidden = rows.len().saturating_sub(limit);
    let mut out: Vec<Line<'static>> = rows.into_iter().take(limit).collect();
    if hidden > 0 {
        out.push(Line::from(Span::styled(
            format!("{} +{hidden} lines ({EXPAND})", theme::ellipsis()),
            theme::dim(),
        )));
    }
    out
}

/// A sub-session is a row where it began: what it is, what it was asked, and
/// what it is doing — read from its own state, never copied into this one.
fn child_row(child: &SessionState, input: &Value, rows: &Rows<'_>) -> Vec<Line<'static>> {
    let mut out = speaks(
        tree::bullet_style(tree::Status::of(child), tree::asking(child)),
        vec![signature(&tree::name(child), &summarize(input), rows)],
        rows,
    );
    if let Some(activity) = tree::activity(child) {
        out.extend(returns(
            vec![Line::from(Span::styled(activity, theme::dim()))],
            rows,
        ));
    }
    out
}

/// What the call is about, from the field a person would recognise.
fn summarize(input: &Value) -> String {
    for key in ["file_path", "command", "pattern", "url", "query", "prompt"] {
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

/// A long-running operation of the surface's own: login, reconnect, a team
/// starting. It reads as a tool row because that is what it is.
fn action(
    status: ItemStatus,
    name: &str,
    args: &Value,
    result: Option<&Value>,
    rows: &Rows<'_>,
) -> Vec<Line<'static>> {
    let mut out = speaks(
        bullet_style(status, false),
        vec![signature(name, &as_text(args), rows)],
        rows,
    );
    if let Some(result) = result {
        out.extend(returns(fold(plain(&as_text(result)), OUTPUT_ROWS), rows));
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

/// A question the model asked and the answer it was given.
fn answered(question: &str, answer: &str, rows: &Rows<'_>) -> Vec<Line<'static>> {
    let mut out = speaks(
        theme::text().patch(theme::bold()),
        vec![Line::from(Span::styled(
            question.to_string(),
            theme::text(),
        ))],
        rows,
    );
    out.extend(returns(
        vec![Line::from(Span::styled(answer.to_string(), theme::dim()))],
        rows,
    ));
    out
}

/// The gate's answer, joined to the row that asked for it. The tool is named
/// only when the row above did not already name it.
fn receipt(
    tool: &str,
    decision: DecisionKind,
    feedback: Option<&str>,
    previous: Option<&Item>,
    rows: &Rows<'_>,
) -> Vec<Line<'static>> {
    let verdict = match decision {
        DecisionKind::Allow => "allowed",
        DecisionKind::AllowSession => "allowed for this session",
        DecisionKind::Deny => "denied",
    };
    let said = match previous.is_some_and(|item| calls(item, tool)) {
        true => verdict.to_string(),
        false => format!("{tool} {verdict}"),
    };
    let text = match feedback {
        Some(feedback) => format!("{said} — {}", rows.shorten(feedback)),
        None => said,
    };
    returns(vec![Line::from(Span::styled(text, theme::dim()))], rows)
}

fn calls(item: &Item, tool: &str) -> bool {
    matches!(&item.body, ItemBody::ToolCall { name, .. } if name == tool)
}

/// A turn that failed says why on a `⏺` of its own, derived from `last_turn`
/// rather than kept as a line of the surface's own. It belongs to no item, so
/// it is the transcript's last block rather than one of them.
pub fn failure(state: &SessionState, rows: &Rows<'_>) -> Vec<Line<'static>> {
    let Some(TurnStatus::Failed { error }) = state.last_turn.as_ref().filter(|_| !state.busy())
    else {
        return Vec::new();
    };
    speaks(
        theme::bad(),
        vec![Line::from(Span::styled(
            error.message.clone(),
            theme::bad(),
        ))],
        rows,
    )
}

/// A full-width divider with its reason in the middle of the left run.
fn rule(text: &str, width: usize) -> Line<'static> {
    let head = format!("{0}{0}{0} {text} ", theme::rule());
    let tail = width.saturating_sub(head.width());
    Line::from(Span::styled(
        format!("{head}{}", theme::rule().repeat(tail)),
        theme::dim(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        assistant, completed, folded, frame, item, post, receipt_item, running_tool, started,
        started_tool, tool, ts, user as person,
    };
    use bingo_sdk::Event;

    fn drawn(items: Vec<Item>) -> Vec<String> {
        let frames = items
            .into_iter()
            .enumerate()
            .map(|(i, item)| frame(i as u64 + 1, Event::ItemCompleted { item }))
            .collect();
        rendered(&folded(frames))
    }

    /// The transcript without the welcome box, which `welcome.rs` pins on its
    /// own: these tests are about the grammar under it.
    fn rendered(state: &SessionState) -> Vec<String> {
        let welcomed = crate::welcome::lines(state, 60).len();
        let mut blocks = crate::blocks::Blocks::default();
        let height = blocks.sync(state, &Agents::new(), 60);
        let mut rows: Vec<String> = blocks
            .window(0, height)
            .iter()
            .skip(welcomed)
            .map(|line| line.to_string().trim_end().to_string())
            .collect();
        if rows.first().is_some_and(String::is_empty) {
            rows.remove(0);
        }
        rows
    }

    #[test]
    fn a_post_says_who_wrote_it_and_a_persons_own_line_does_not() {
        assert_eq!(
            drawn(vec![
                post("itm_1", "reviewer", "two nits, otherwise fine"),
                person("itm_2", "thanks"),
            ]),
            vec![
                "> reviewer: two nits, otherwise fine".to_string(),
                String::new(),
                "> thanks".to_string(),
            ],
        );
    }

    #[test]
    fn the_room_a_post_came_from_is_the_view_it_is_read_in() {
        let drawn = drawn(vec![post("itm_1", "scout", "found it")]).join("\n");
        assert!(!drawn.contains("#design"), "{drawn}");
    }

    #[test]
    fn a_second_line_of_yours_stays_on_the_bar_under_the_first() {
        assert_eq!(
            drawn(vec![post("itm_1", "scout", "one\ntwo")]),
            vec!["> scout: one".to_string(), "  two".to_string()],
        );
    }

    #[test]
    fn the_model_speaks_after_a_bold_bullet_and_keeps_its_indent() {
        assert_eq!(
            drawn(vec![assistant(
                "itm_1",
                "alpha bravo charlie delta echo foxtrot golf hotel india juliet",
                ItemStatus::Completed,
            )]),
            vec![
                "⏺ alpha bravo charlie delta echo foxtrot golf hotel india".to_string(),
                "  juliet".to_string(),
            ],
            "a wrapped answer hangs under its own bullet"
        );
    }

    #[test]
    fn a_tool_row_names_the_call_and_hangs_its_result_under_it() {
        let output = ToolOutput::text("Read 3 lines");
        assert_eq!(
            drawn(vec![tool(
                "itm_1",
                "Read",
                serde_json::json!({"file_path": "/tmp/project/Cargo.toml"}),
                Some(output),
                ItemStatus::Completed,
            )]),
            vec![
                "⏺ Read(Cargo.toml)".to_string(),
                "  ⎿  Read 3 lines".to_string(),
            ],
            "the path is named from the session's own directory"
        );
    }

    #[test]
    fn a_long_result_says_how_much_it_folded_away_and_what_opens_it() {
        let output = ToolOutput::text((1..=9).map(|i| format!("line {i}\n")).collect::<String>());
        let drawn = drawn(vec![tool(
            "itm_1",
            "Read",
            serde_json::json!({"file_path": "src/lib.rs"}),
            Some(output),
            ItemStatus::Completed,
        )]);
        assert_eq!(drawn.len(), OUTPUT_ROWS + 2);
        assert_eq!(
            drawn.last().map(String::as_str),
            Some("     … +4 lines (ctrl+o to expand)")
        );
    }

    #[test]
    fn a_running_tool_shows_the_last_rows_of_its_tail() {
        let state = folded(vec![started_tool(
            1,
            running_tool("itm_1", "Bash", "one\ntwo\nthree\nfour"),
        )]);
        assert_eq!(
            rendered(&state),
            vec![
                "⏺ Bash(cargo test)".to_string(),
                "  ⎿  two".to_string(),
                "     three".to_string(),
                "     four".to_string(),
            ],
        );
    }

    #[test]
    fn thinking_decays_into_what_it_took() {
        let running = item(
            "itm_1",
            ItemStatus::Running,
            ItemBody::Reasoning {
                text: "…".into(),
                provider_metadata: Default::default(),
            },
        );
        let mut done = running.clone();
        done.status = ItemStatus::Completed;
        done.completed_at = Some(ts() + jiff::SignedDuration::from_secs(2));
        assert_eq!(drawn(vec![running]), vec!["✻ Thinking…".to_string()]);
        assert_eq!(drawn(vec![done]), vec!["✻ Thought for 2s".to_string()]);
    }

    #[test]
    fn a_receipt_joins_the_row_that_asked_for_it() {
        assert_eq!(
            drawn(vec![
                tool(
                    "itm_1",
                    "Edit",
                    serde_json::json!({"file_path": "src/lib.rs"}),
                    None,
                    ItemStatus::Failed,
                ),
                receipt_item("itm_2", "Edit", DecisionKind::Deny, Some("use cargo clean")),
            ]),
            vec![
                "⏺ Edit(src/lib.rs)".to_string(),
                "  ⎿  denied — use cargo clean".to_string(),
            ],
            "no blank line, and the tool is named once"
        );
    }

    #[test]
    fn a_receipt_with_no_call_above_it_names_its_own_tool() {
        assert_eq!(
            drawn(vec![receipt_item(
                "itm_1",
                "Edit",
                DecisionKind::Allow,
                None
            )]),
            vec!["  ⎿  Edit allowed".to_string()],
        );
    }

    #[test]
    fn a_failed_turn_is_a_bullet_of_its_own() {
        let state = folded(vec![
            frame(1, started("trn_1")),
            frame(
                2,
                completed(
                    "trn_1",
                    TurnStatus::Failed {
                        error: bingo_sdk::KernelError::new(
                            bingo_sdk::ErrorCode::ProviderUnavailable,
                            "no route to the provider",
                        ),
                    },
                ),
            ),
        ]);
        assert_eq!(
            rendered(&state).last().map(String::as_str),
            Some("⏺ no route to the provider")
        );
    }

    #[test]
    fn the_measure_stops_prose_at_a_hundred_columns() {
        let state = folded(vec![frame(
            1,
            Event::ItemCompleted {
                item: assistant("itm_1", &"word ".repeat(60), ItemStatus::Completed),
            },
        )]);
        let welcomed = crate::welcome::lines(&state, 160).len();
        let mut blocks = crate::blocks::Blocks::default();
        let height = blocks.sync(&state, &Agents::new(), 160);
        let widest = blocks
            .window(0, height)
            .iter()
            .skip(welcomed)
            .map(|line| line.to_string().trim_end().width())
            .max()
            .unwrap_or(0);
        assert!(widest <= wrap::MEASURE, "{widest} cells");
        assert!(widest > 80, "and it uses the measure it has: {widest}");
    }
}
