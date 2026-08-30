//! Claude Code's `stream-json` envelope, as a projection of frames.
//!
//! ADR-0007 §8: a lossy compatibility encoder, so a host that already drives
//! `claude -p --output-format stream-json` drives bingo without a plugin. It is
//! never a second event model — the encoder keeps only the per-turn facts the
//! folded state throws away, and derives every line from the frame and the
//! `SessionState` the renderer already holds.
//!
//! Verified on 2026-08-29 against the current documentation, which now lives at
//! `code.claude.com/docs/en/…` (the `docs.anthropic.com/en/docs/claude-code/…`
//! paths redirect there):
//!
//! - `cli-reference` — `--output-format stream-json`, and
//!   `--include-partial-messages`, which alone adds the `stream_event` lines.
//!   Deltas are therefore not written here.
//! - `headless` — `system`/`init` is the first line of the stream, `result` the
//!   last one that matters.
//! - `agent-sdk/typescript` — the `SDKMessage` union, which is the only place
//!   the fields are enumerated (the docs print no sample line). `session_id`
//!   and `parent_tool_use_id` are required on every line written here; `result`
//!   exists **only** on the `success` arm, and the error arms carry
//!   `errors: string[]` in its place.
//! - `platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls`
//!   — a `tool_result` block's `content` is a string or a block array, and a
//!   block may be an image.
//!
//! Documented fields bingo cannot fill are left out rather than invented:
//! `uuid`, `claude_code_version`, `mcp_servers`, `slash_commands`,
//! `output_style`, `modelUsage`, `permission_denials`, and the `result` line's
//! `stop_reason`. What is written but constant: `apiKeySource`
//! is `none`, `total_cost_usd` is `0.0` because nothing here prices a turn,
//! `duration_api_ms` is `0` because only the whole turn is timed, and a
//! message's `usage` is zero because bingo counts tokens per round
//! (`TurnUsage`) and reports the total once, in the `result` line.

use bingo_sdk::{
    ContentPart, ErrorCode, Event, Frame, InterruptReason, Item, ItemBody, SessionState,
    ToolOutput, TurnStatus, Usage,
};

/// Whose frame is being encoded, and the root it is reported under. A
/// sub-session's lines carry the root's `session_id` and, as
/// `parent_tool_use_id`, the call that spawned it (ADR-0010 §4): the child's
/// `parent.item` names the root's tool call, whose `call_id` is the id the
/// envelope already wrote for that `tool_use`.
struct Scope<'a> {
    state: &'a SessionState,
    root: &'a SessionState,
}

impl Scope<'_> {
    fn is_root(&self) -> bool {
        self.state.summary.id == self.root.summary.id
    }

    fn session_id(&self) -> &str {
        self.root.summary.id.as_str()
    }

    fn parent_tool_use_id(&self) -> Value {
        if self.is_root() {
            return Value::Null;
        }
        let call = self
            .state
            .summary
            .parent
            .as_ref()
            .and_then(|link| link.item.as_ref())
            .and_then(|item| self.root.item(item))
            .and_then(|item| match &item.body {
                ItemBody::ToolCall { call_id, .. } => Some(call_id.clone()),
                _ => None,
            });
        call.map_or(Value::Null, Value::String)
    }
}
use serde_json::{Value, json};

use crate::render::tool_failed;

#[derive(Debug)]
pub(crate) struct Encoder {
    /// The tool names the preamble advertises; only the host knows them.
    tools: Vec<String>,
    turn: Turn,
}

/// The per-turn facts the `result` line reports. None of them survive in the
/// folded state, which clears the live turn as it applies `TurnCompleted`.
#[derive(Debug, Default)]
struct Turn {
    /// `None` until `TurnStarted`, so a stream joined mid-turn reports no
    /// duration rather than a made-up one.
    started_ms: Option<i64>,
    /// Model rounds, one `TurnUsage` each.
    rounds: u64,
    /// The last assistant text, which the `success` arm reports.
    text: String,
}

impl Turn {
    fn started_at(ms: i64) -> Self {
        Self {
            started_ms: Some(ms),
            ..Self::default()
        }
    }

    fn elapsed_ms(&self, now: i64) -> u64 {
        self.started_ms.map_or(0, |started| {
            u64::try_from(now.saturating_sub(started)).unwrap_or(0)
        })
    }
}

impl Encoder {
    pub(crate) fn new(tools: Vec<String>) -> Self {
        Self {
            tools,
            turn: Turn::default(),
        }
    }

    /// The preamble, written once before any frame.
    pub(crate) fn init(&self, state: &SessionState) -> Value {
        let mut line = json!({
            "type": "system",
            "subtype": "init",
            "session_id": state.summary.id.as_str(),
            "cwd": state.summary.cwd,
            "tools": self.tools,
            "model": state.summary.model,
            "permissionMode": permission_mode(state),
            "apiKeySource": "none",
        });
        drop_unknown(&mut line, "model");
        line
    }

    /// At most one line per frame; `None` for everything the envelope has no
    /// shape for. `root` is the session the run opened; a frame of one of its
    /// sub-sessions is reported under it, and its turns write no `result`.
    pub(crate) fn line(
        &mut self,
        frame: &Frame,
        state: &SessionState,
        root: &SessionState,
    ) -> Option<Value> {
        let scope = Scope { state, root };
        match &frame.event {
            Event::TurnStarted { .. } if scope.is_root() => {
                self.turn = Turn::started_at(frame.ts.as_millisecond());
                None
            }
            Event::TurnUsage { .. } if scope.is_root() => {
                self.turn.rounds += 1;
                None
            }
            Event::TurnCompleted { status, usage, .. } if scope.is_root() => {
                Some(self.result(frame, &scope, status, *usage))
            }
            Event::TurnStarted { .. } | Event::TurnUsage { .. } | Event::TurnCompleted { .. } => {
                None
            }
            Event::ItemStarted { item } => started(item, &scope),
            Event::ItemCompleted { item } => self.completed(item, &scope),
            // A delta is a `stream_event` line, which Claude Code writes only
            // under `--include-partial-messages`. The rest has no shape in this
            // envelope and reaches stderr exactly as `--output-format json`
            // leaves it.
            Event::ItemDelta { .. }
            | Event::ItemUpdated { .. }
            | Event::SessionUpdated { .. }
            | Event::SessionClosed { .. }
            | Event::TurnRetrying { .. }
            | Event::QueueChanged { .. }
            | Event::InteractionOpened { .. }
            | Event::InteractionResolved { .. }
            | Event::InteractionCancelled { .. }
            | Event::IntentAck { .. }
            | Event::Compacted { .. }
            | Event::Rewound { .. }
            | Event::ConfigChanged { .. }
            | Event::CatalogChanged { .. }
            | Event::Notice { .. }
            | Event::Extension { .. }
            | Event::Signal { .. }
            | Event::Lagged { .. } => None,
        }
    }

    fn completed(&mut self, item: &Item, scope: &Scope<'_>) -> Option<Value> {
        match &item.body {
            ItemBody::Assistant { text } => self.assistant_text(item, scope, text),
            ItemBody::ToolCall {
                call_id, output, ..
            } => Some(tool_result(
                scope,
                call_id,
                output.as_ref(),
                tool_failed(item, output.as_ref()),
            )),
            // Not part of the envelope: it has no shape for them.
            ItemBody::User { .. }
            | ItemBody::Reasoning { .. }
            | ItemBody::Action { .. }
            | ItemBody::Compaction { .. }
            | ItemBody::Rewind { .. }
            | ItemBody::Interruption { .. }
            | ItemBody::Notice { .. }
            | ItemBody::QuestionAnswer { .. }
            | ItemBody::PermissionReceipt { .. }
            | ItemBody::Asset { .. } => None,
        }
    }

    /// The completion is authoritative over every delta before it. An empty one
    /// is a round that only called tools, and has no message to report.
    fn assistant_text(&mut self, item: &Item, scope: &Scope<'_>, text: &str) -> Option<Value> {
        if text.is_empty() {
            return None;
        }
        if scope.is_root() {
            self.turn.text = text.to_string();
        }
        Some(assistant(scope, item.id.as_str(), text_block(text)))
    }

    fn result(&self, frame: &Frame, scope: &Scope<'_>, status: &TurnStatus, total: Usage) -> Value {
        let (outcome_key, outcome) = outcome(status, &self.turn.text);
        json!({
            "type": "result",
            "subtype": subtype(status),
            "is_error": !matches!(status, TurnStatus::Completed),
            "duration_ms": self.turn.elapsed_ms(frame.ts.as_millisecond()),
            "duration_api_ms": 0,
            "num_turns": self.turn.rounds,
            outcome_key: outcome,
            "session_id": scope.session_id(),
            "total_cost_usd": 0.0,
            "usage": tokens(total),
        })
    }
}

/// A tool call reaches the envelope as the assistant asking for it.
fn started(item: &Item, scope: &Scope<'_>) -> Option<Value> {
    let ItemBody::ToolCall {
        call_id,
        name,
        input,
        ..
    } = &item.body
    else {
        return None;
    };
    Some(assistant(
        scope,
        item.id.as_str(),
        tool_use_block(call_id, name, input),
    ))
}

/// One assistant message around one content block: a bingo item is one thing,
/// so no line carries two.
fn assistant(scope: &Scope<'_>, id: &str, content: Value) -> Value {
    let mut message = json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": scope.state.summary.model,
        "content": [content],
        // Documented as nullable: this message reports no stop reason.
        "stop_reason": Value::Null,
        "usage": tokens(Usage::default()),
    });
    drop_unknown(&mut message, "model");
    json!({
        "type": "assistant",
        "message": message,
        "parent_tool_use_id": scope.parent_tool_use_id(),
        "session_id": scope.session_id(),
    })
}

fn tool_result(
    scope: &Scope<'_>,
    call_id: &str,
    output: Option<&ToolOutput>,
    is_error: bool,
) -> Value {
    json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": call_id,
                "content": tool_content(output),
                "is_error": is_error,
            }],
        },
        "parent_tool_use_id": scope.parent_tool_use_id(),
        "session_id": scope.session_id(),
    })
}

/// What a tool returned. Text alone is the string form; an image forces the
/// block-array form, the other shape the API documents for `tool_result`.
fn tool_content(output: Option<&ToolOutput>) -> Value {
    let Some(output) = output else {
        return json!("");
    };
    if output.parts.iter().any(is_image) {
        json!(output.parts.iter().filter_map(block).collect::<Vec<_>>())
    } else {
        json!(joined_text(&output.parts))
    }
}

fn is_image(part: &ContentPart) -> bool {
    matches!(part, ContentPart::Image { .. })
}

fn joined_text(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(ContentPart::as_text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The blocks a tool result may carry; a nested call or reasoning has no place
/// in one and is dropped.
fn block(part: &ContentPart) -> Option<Value> {
    match part {
        ContentPart::Text { text } => Some(text_block(text)),
        ContentPart::Image { media_type, data } => Some(json!({
            "type": "image",
            "source": { "type": "base64", "media_type": media_type, "data": data },
        })),
        ContentPart::ToolUse { .. }
        | ContentPart::ToolResult { .. }
        | ContentPart::Reasoning { .. } => None,
    }
}

fn text_block(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

fn tool_use_block(call_id: &str, name: &str, input: &Value) -> Value {
    json!({
        "type": "tool_use",
        "id": call_id,
        "name": name,
        "input": input,
    })
}

fn subtype(status: &TurnStatus) -> &'static str {
    match status {
        TurnStatus::Completed => "success",
        TurnStatus::Failed { error } if error.code == ErrorCode::TurnBudgetExhausted => {
            "error_max_turns"
        }
        TurnStatus::Failed { .. } | TurnStatus::Interrupted { .. } => "error_during_execution",
    }
}

/// The `success` arm reports the last assistant text; the error arms report
/// what went wrong instead, and carry no `result` at all.
fn outcome(status: &TurnStatus, text: &str) -> (&'static str, Value) {
    match status {
        TurnStatus::Completed => ("result", json!(text)),
        TurnStatus::Failed { error } => ("errors", json!([error.message])),
        TurnStatus::Interrupted { reason } => ("errors", json!([interrupted(*reason)])),
    }
}

fn interrupted(reason: InterruptReason) -> &'static str {
    match reason {
        InterruptReason::UserCancel => "the turn was interrupted",
        InterruptReason::NewInput => "the turn was interrupted by new input",
        InterruptReason::Shutdown => "the turn was interrupted by a shutdown",
        InterruptReason::Budget => "the turn ran out of budget",
    }
}

/// The one place the envelope names the session.
/// Token counts under the names the Anthropic API gives them.
fn tokens(usage: Usage) -> Value {
    json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_read_input_tokens": usage.cache_read_tokens,
        "cache_creation_input_tokens": usage.cache_write_tokens,
    })
}

/// A field bingo does not know is absent, not `null`: a reader must not mistake
/// silence for a value.
fn drop_unknown(line: &mut Value, key: &str) {
    if line.get(key) == Some(&Value::Null)
        && let Some(object) = line.as_object_mut()
    {
        object.remove(key);
    }
}

/// The session's permission mode as the policy describes it (ADR-0009 §5);
/// `default` until a policy has said otherwise.
fn permission_mode(state: &SessionState) -> &str {
    state
        .config
        .plugins
        .get("bingo.permissions")
        .and_then(|p| p.get("mode"))
        .and_then(Value::as_str)
        .unwrap_or("default")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Mode;
    use crate::render::tests::{Sinks, play, play_with, text_turn};
    use crate::tests::{assistant, frame, session_state, tool_call};
    use bingo_sdk::{
        ContextUsage, DeltaKind, InterruptReason, ItemId, ItemStatus, KernelError, Level,
        SessionId, TurnId, TurnOrigin,
    };
    use jiff::Timestamp;

    /// The fixed instant `crate::tests::frame` stamps every frame with.
    const START_MS: i64 = 1_700_000_000_000;

    /// The catalogue the host would hand the surface.
    fn tools() -> Vec<String> {
        vec!["Read".into(), "Bash".into()]
    }

    fn play_stream(frames: &[Frame]) -> Sinks {
        play_with(Mode::StreamJson, frames, tools())
    }

    /// Only the `result` line measures a duration, so only these fixtures move
    /// the clock.
    fn at_ms(mut frame: Frame, ms: i64) -> Frame {
        frame.ts = Timestamp::from_millisecond(START_MS + ms).expect("a fixed instant");
        frame
    }

    fn counted() -> Usage {
        Usage {
            input_tokens: 12,
            output_tokens: 34,
            cache_read_tokens: 56,
            cache_write_tokens: 78,
            reasoning_tokens: 0,
        }
    }

    fn started(seq: u64) -> Frame {
        at_ms(
            frame(
                seq,
                Event::TurnStarted {
                    turn: TurnId::from_raw("trn_1"),
                    inputs: vec![],
                    origin: TurnOrigin::Submit,
                },
            ),
            0,
        )
    }

    /// One model round, which is what `num_turns` counts.
    fn round(seq: u64) -> Frame {
        frame(
            seq,
            Event::TurnUsage {
                turn: TurnId::from_raw("trn_1"),
                usage: counted(),
                context: ContextUsage {
                    used: 100,
                    window: 1000,
                    trigger: 800,
                },
            },
        )
    }

    fn ended(seq: u64, status: TurnStatus) -> Frame {
        at_ms(
            frame(
                seq,
                Event::TurnCompleted {
                    turn: TurnId::from_raw("trn_1"),
                    status,
                    usage: counted(),
                },
            ),
            1500,
        )
    }

    fn spoke(seq: u64, id: &str, text: &str) -> Frame {
        frame(
            seq,
            Event::ItemCompleted {
                item: assistant(id, text, ItemStatus::Completed),
            },
        )
    }

    fn failed(error: KernelError) -> Vec<Frame> {
        vec![started(1), round(2), ended(3, TurnStatus::Failed { error })]
    }

    fn text_round() -> Vec<Frame> {
        vec![
            started(1),
            round(2),
            spoke(3, "itm_1", "Hello"),
            ended(4, TurnStatus::Completed),
        ]
    }

    fn tool_round() -> Vec<Frame> {
        vec![
            started(1),
            round(2),
            frame(
                3,
                Event::ItemStarted {
                    item: tool_call("itm_2", "Read", None, ItemStatus::Running),
                },
            ),
            frame(
                4,
                Event::ItemCompleted {
                    item: tool_call(
                        "itm_2",
                        "Read",
                        Some(ToolOutput::text("[package]")),
                        ItemStatus::Completed,
                    ),
                },
            ),
            round(5),
            spoke(6, "itm_3", "Read it."),
            ended(7, TurnStatus::Completed),
        ]
    }

    fn last(rendered: &str) -> Value {
        let line = rendered.lines().next_back().expect("a line");
        serde_json::from_str(line).expect("JSON")
    }

    // ---- the lines ------------------------------------------------------

    #[test]
    fn a_text_turn_is_the_preamble_the_message_and_the_result() {
        insta::assert_snapshot!("text_turn", play_stream(&text_round()).out());
    }

    #[test]
    fn a_tool_round_is_a_tool_use_then_a_tool_result() {
        insta::assert_snapshot!("tool_round", play_stream(&tool_round()).out());
    }

    #[test]
    fn a_failed_turn_is_an_error_result_carrying_the_message() {
        let frames = failed(KernelError::new(
            ErrorCode::ProviderUnavailable,
            "the provider is down",
        ));
        insta::assert_snapshot!("failed_turn", play_stream(&frames).out());
    }

    #[test]
    fn a_budget_failure_is_error_max_turns() {
        let frames = failed(KernelError::new(
            ErrorCode::TurnBudgetExhausted,
            "the turn budget is exhausted",
        ));
        insta::assert_snapshot!("max_turns", play_stream(&frames).out());
    }

    // ---- the envelope ---------------------------------------------------

    #[test]
    fn every_line_is_an_envelope_naming_the_session() {
        for frames in [text_round(), tool_round()] {
            let rendered = play_stream(&frames).out();
            assert!(!rendered.is_empty());
            for line in rendered.lines() {
                let value: Value = serde_json::from_str(line).expect("one JSON object per line");
                assert!(
                    matches!(
                        value["type"].as_str(),
                        Some("system" | "assistant" | "user" | "result")
                    ),
                    "unexpected line type in {line}"
                );
                assert_eq!(value["session_id"].as_str(), Some("ses_1"));
            }
        }
    }

    #[test]
    fn the_preamble_is_the_first_line() {
        let rendered = play_stream(&text_round()).out();
        let first = rendered.lines().next().expect("a preamble");
        let value: Value = serde_json::from_str(first).expect("JSON");
        assert_eq!(value["type"], json!("system"));
        assert_eq!(value["subtype"], json!("init"));
        assert_eq!(value["tools"], json!(["Read", "Bash"]));
        assert_eq!(value["cwd"], json!("/tmp"));
        assert_eq!(value["model"], json!("fake-1"));
    }

    /// The whole point of the mode: a host parsing stdout sees no prose, no
    /// delta and no notice.
    #[test]
    fn deltas_and_notices_never_reach_stdout() {
        let frames = vec![
            started(1),
            frame(
                2,
                Event::ItemDelta {
                    item: ItemId::from_raw("itm_1"),
                    n: 0,
                    kind: DeltaKind::Text,
                    data: "Hel".into(),
                },
            ),
            frame(
                3,
                Event::Notice {
                    level: Level::Warn,
                    code: "COUNT_TOKENS_UNAVAILABLE".into(),
                    text: "estimating".into(),
                },
            ),
            spoke(4, "itm_1", "Hello"),
            ended(5, TurnStatus::Completed),
        ];
        let rendered = play_stream(&frames).out();
        assert_eq!(rendered.lines().count(), 3, "init, assistant, result");
        assert!(!rendered.contains("estimating"));
    }

    /// Stderr is not this mode's business: it says exactly what
    /// `--output-format json` says.
    #[test]
    fn stderr_is_what_json_mode_would_have_written() {
        let cases = [
            text_round(),
            tool_round(),
            text_turn(),
            failed(KernelError::new(ErrorCode::ToolFailed, "boom")),
        ];
        for frames in cases {
            assert_eq!(
                play_stream(&frames).err(),
                play(Mode::Json, &frames).err(),
                "stderr must not depend on the stdout dialect"
            );
        }
    }

    // ---- the lossy edges ------------------------------------------------

    #[test]
    fn an_interrupted_turn_is_an_error_during_execution() {
        let frames = vec![
            started(1),
            ended(
                2,
                TurnStatus::Interrupted {
                    reason: InterruptReason::UserCancel,
                },
            ),
        ];
        let value = last(&play_stream(&frames).out());
        assert_eq!(value["subtype"], json!("error_during_execution"));
        assert_eq!(value["is_error"], json!(true));
        assert_eq!(value["errors"], json!(["the turn was interrupted"]));
        assert_eq!(value.get("result"), None, "no result on an error arm");
    }

    #[test]
    fn the_result_reports_the_rounds_the_duration_and_the_tokens() {
        let value = last(&play_stream(&text_round()).out());
        assert_eq!(value["num_turns"], json!(1));
        assert_eq!(value["duration_ms"], json!(1500));
        assert_eq!(value["result"], json!("Hello"));
        assert_eq!(
            value["usage"],
            json!({
                "input_tokens": 12,
                "output_tokens": 34,
                "cache_read_input_tokens": 56,
                "cache_creation_input_tokens": 78,
            })
        );
    }

    #[test]
    fn a_failed_tool_call_says_so_in_its_result() {
        let frames = vec![frame(
            1,
            Event::ItemCompleted {
                item: tool_call(
                    "itm_2",
                    "Read",
                    Some(ToolOutput::error("file not found")),
                    ItemStatus::Failed,
                ),
            },
        )];
        let value = last(&play_stream(&frames).out());
        let block = &value["message"]["content"][0];
        assert_eq!(block["is_error"], json!(true));
        assert_eq!(block["content"], json!("file not found"));
        assert_eq!(block["tool_use_id"], json!("call_1"));
    }

    /// Text alone is the string form; an image forces the block form the API
    /// documents for a `tool_result`.
    /// A root whose running tool call `i1` spawned `ses_2`, and that child.
    fn root_and_child() -> (SessionState, SessionState) {
        let mut root = session_state();
        root.apply(&frame(
            1,
            Event::ItemStarted {
                item: tool_call("i1", "SpawnAgent", None, ItemStatus::Running),
            },
        ));
        let mut child_summary = root.summary.clone();
        child_summary.id = SessionId::from_raw("ses_2");
        child_summary.parent = Some(bingo_sdk::ParentLink {
            session: root.summary.id.clone(),
            item: Some(ItemId::from_raw("i1")),
        });
        (root, SessionState::new(child_summary))
    }

    fn child_frame(seq: u64, event: Event) -> Frame {
        let mut f = frame(seq, event);
        f.session = SessionId::from_raw("ses_2");
        f
    }

    #[test]
    fn a_sub_sessions_lines_name_the_root_and_the_call_that_spawned_it() {
        let (root, mut child) = root_and_child();
        let mut encoder = Encoder::new(tools());
        let said = child_frame(
            2,
            Event::ItemCompleted {
                item: assistant("c1", "hi from the child", ItemStatus::Completed),
            },
        );
        child.apply(&said);
        let line = encoder
            .line(&said, &child, &root)
            .expect("an assistant line");
        assert_eq!(line["type"], "assistant");
        assert_eq!(line["parent_tool_use_id"], "call_1");
        assert_eq!(line["session_id"], "ses_1");

        let over = child_frame(
            3,
            Event::TurnCompleted {
                turn: TurnId::from_raw("trn_c"),
                status: TurnStatus::Completed,
                usage: counted(),
            },
        );
        assert_eq!(
            encoder.line(&over, &child, &root),
            None,
            "a child's turn writes no result line"
        );
        let result = encoder
            .line(&ended(4, TurnStatus::Completed), &root, &root)
            .expect("the root's result");
        assert_eq!(
            result["result"], "",
            "the child's words are not the root's answer"
        );
    }

    #[test]
    fn an_image_in_a_tool_result_becomes_a_block_array() {
        let output = ToolOutput {
            parts: vec![
                ContentPart::text("the chart"),
                ContentPart::Image {
                    media_type: "image/png".into(),
                    data: "iVBORw0KGgo=".into(),
                },
            ],
            is_error: false,
            display: None,
        };
        let frames = vec![frame(
            1,
            Event::ItemCompleted {
                item: tool_call("itm_2", "Read", Some(output), ItemStatus::Completed),
            },
        )];
        let value = last(&play_stream(&frames).out());
        assert_eq!(
            value["message"]["content"][0]["content"],
            json!([
                { "type": "text", "text": "the chart" },
                {
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": "image/png",
                        "data": "iVBORw0KGgo=",
                    },
                },
            ])
        );
    }

    /// An assistant item that only asked for a tool has no message to report.
    #[test]
    fn an_empty_assistant_completion_writes_no_line() {
        let frames = vec![
            started(1),
            spoke(2, "itm_1", ""),
            ended(3, TurnStatus::Completed),
        ];
        let rendered = play_stream(&frames).out();
        assert_eq!(rendered.lines().count(), 2, "init and result only");
        assert_eq!(last(&rendered)["result"], json!(""));
    }

    #[test]
    fn a_session_with_no_model_omits_the_field_rather_than_inventing_one() {
        let mut state = session_state();
        state.summary.model = None;
        let encoder = Encoder::new(tools());
        assert_eq!(encoder.init(&state).get("model"), None);
    }

    #[test]
    fn a_turn_that_never_started_still_ends_with_a_result() {
        let frames = vec![ended(1, TurnStatus::Completed)];
        let value = last(&play_stream(&frames).out());
        assert_eq!(value["duration_ms"], json!(0));
        assert_eq!(value["num_turns"], json!(0));
        assert_eq!(value["result"], json!(""));
    }
}
