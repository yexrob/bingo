//! Renderer-agnostic contract between the agent core and any front end.
//!
//! Nothing here may depend on a terminal library: [`UiEvent`] and the dialog
//! transport types are what a TUI, a GUI or a test harness all consume, and
//! [`tui_hooks`] is the adapter that turns [`UiHooks`] callbacks into channel
//! traffic. The TUI implementation lives in [`crate::tui`].

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::api::types::StreamEvent;
use crate::query::{ToolCallDone, UiHooks};
use crate::tui::activities::WatchStatus;

/// Serialized event stream protocol version (UiEventEnvelope). Bump on breaking
/// wire-format changes; consumers reject unknown versions instead of mis-parsing.
/// (Protocol API for the future web_hooks adapter; no production consumer yet.)
#[cfg_attr(not(test), allow(dead_code))]
pub const UI_EVENT_PROTOCOL_VERSION: u32 = 1;

/// Versioned envelope for serialized UiEvent streams (GUI/WebSocket protocol):
/// `{"version": 1, "type": "TextDelta", ...}` with the event fields flattened
/// alongside the version.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Serialize)]
pub struct UiEventEnvelope {
    pub version: u32,
    #[serde(flatten)]
    pub event: UiEvent,
}

#[cfg_attr(not(test), allow(dead_code))]
impl UiEventEnvelope {
    pub fn new(event: UiEvent) -> Self {
        Self { version: UI_EVENT_PROTOCOL_VERSION, event }
    }
}

/// Permission prompt: request + result receipt.
/// (The request/response halves are what cross the wire: [`PermissionRequest`]
/// goes to the front end, [`DialogAction`] comes back; the oneshot sender never
/// serializes.)
pub type AskRequest = (PermissionRequest, oneshot::Sender<DialogAction>);

/// Permission dialog result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DialogAction {
    /// Option `index` (0-based) confirmed.
    Confirm(usize),
    /// AskUserQuestion's Other free-form input submitted.
    Answer(String),
    /// Dialog cancelled with Esc.
    Cancel,
}

/// Permission/question block to display.
#[derive(Debug, Clone, Serialize)]
pub struct PermissionRequest {
    /// Title, e.g. `Allow Bash` or AskUserQuestion's header.
    pub title: String,
    /// Description under the title.
    pub question: String,
    /// Numbered options (numbers are added automatically).
    pub options: Vec<String>,
    /// Description of options[i] (CC Select sub-line, dimmed).
    pub descriptions: Vec<Option<String>>,
    /// AskUserQuestion: a "Other" free-form input is appended automatically (CC behavior).
    pub free_text: bool,
}

impl PermissionRequest {
    pub fn new(title: impl Into<String>, question: impl Into<String>, options: Vec<String>) -> Self {
        Self {
            title: title.into(),
            question: question.into(),
            options,
            descriptions: Vec::new(),
            free_text: false,
        }
    }
}

/// Event channel from the agent task to components.
/// Serialized for the GUI protocol (see [`UiEventEnvelope`]): `type` tags the
/// variant, `data` carries the payload (`{"type": "TextDelta", "data": "hi"}`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum UiEvent {
    TurnStart,
    /// All tool calls in a batch finished (one query loop round closed).
    RoundEnd,
    TextDelta(String),
    ThinkingDelta(String),
    /// Cumulative output token count from message_delta.
    OutputTokens(u64),
    ToolStart { name: String },
    /// Tool block fully received while streaming (including input): the fold decision point.
    /// standalone=true: non-model tools like the `!` command — summary only, not part of a fold group.
    ToolReady {
        name: String,
        input: serde_json::Value,
        standalone: bool,
    },
    ToolDone(ToolCallDone),
    /// Watchable status events (command/agent lifecycle, forwarded from the registry).
    WatchEvent {
        label: String,
        kind: crate::watch::WatchKind,
        status: WatchStatus,
        detail: Option<String>,
        duration_ms: u64,
        payload: Option<serde_json::Value>,
        signal: Option<String>,
    },
    /// `/model` secondary selector: a provider's model list finished fetching asynchronously
    /// (appended to the menu).
    ModelsLoaded {
        provider: String,
        models: Vec<String>,
    },
    /// Image finished loading asynchronously after the message was finalized
    /// (meta=None = load failed, placeholder shown).
    ImageReady {
        url: String,
        meta: Option<crate::tui::gfx::ImageMeta>,
    },
    TurnEnd,
    /// Non-fatal warning (e.g. MCP connection failure), shown above the input
    /// box; expires after `WARNING_TTL` (10s, filtered at render time).
    Warning(String),
    /// Async slash-command result (/compact /status /context): rendered after the messages.
    SlashOutput(String),
    /// Turn-level error (structured): `code` is a stable error code (SCREAMING_SNAKE, mapped
    /// through the unified exit of `crate::error::map_error`), `msg` is human-readable text,
    /// `level` is the presentation level (the renderer branches by level: page/field-level →
    /// highlight the error line, whole-flow-level → full-screen error state), `context` is the
    /// triggering context — both are explicitly carried by the **producer** when emitting
    /// (not inferred by the render layer); level and context are guaranteed consistent by §4.4.
    Error {
        code: &'static str,
        msg: String,
        level: crate::error::ErrorLevel,
        context: crate::error::ErrorContext,
    },
}

/// Wire query's UiHooks to the TUI channels.
pub fn tui_hooks(
    events: mpsc::UnboundedSender<UiEvent>,
    asks: mpsc::UnboundedSender<AskRequest>,
) -> UiHooks {
    let tool_events = events.clone();
    let ready_events = events.clone();
    let round_events = events.clone();
    let warn_events = events.clone();
    let ask_asks = asks.clone();
    UiHooks {
        on_event: Box::new(move |event| match event {
            StreamEvent::TextDelta { text, .. } => {
                let _ = events.send(UiEvent::TextDelta(text.clone()));
            }
            StreamEvent::ThinkingDelta { thinking, .. } => {
                let _ = events.send(UiEvent::ThinkingDelta(thinking.clone()));
            }
            StreamEvent::ToolUseStart { name, .. } => {
                let _ = events.send(UiEvent::ToolStart { name: name.clone() });
            }
            StreamEvent::StopReason { output_tokens: Some(tokens), .. } => {
                let _ = events.send(UiEvent::OutputTokens(*tokens));
            }
            _ => {}
        }),
        on_tool_ready: Box::new(move |name, input, standalone| {
            let _ = ready_events.send(UiEvent::ToolReady {
                name,
                input,
                standalone,
            });
        }),
        on_tool_done: Box::new(move |done| {
            let _ = tool_events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
                name: done.name.clone(),
                summary: done.summary.clone(),
                output: done.output.clone(),
                is_error: done.is_error,
                diff: done.diff.clone(),
                duration_ms: done.duration_ms,
            }));
        }),
        on_round_end: Box::new(move || {
            let _ = round_events.send(UiEvent::RoundEnd);
        }),
        on_warning: Box::new(move |message| {
            let _ = warn_events.send(UiEvent::Warning(message));
        }),
        ask: Arc::new(move |tool_name, reason| {
            let request = PermissionRequest::new(
                format!("允许执行 {tool_name}"),
                reason,
                vec!["允许".to_string(), "拒绝".to_string()],
            );
            let (tx, rx) = oneshot::channel();
            if ask_asks.send((request, tx)).is_err() {
                return Box::pin(async { false });
            }
            Box::pin(async move {
                matches!(rx.await, Ok(DialogAction::Confirm(0)))
            })
        }),
        ask_question: Arc::new(move |title, question, options| {
            let mut request = PermissionRequest::new(title, question, Vec::new());
            request.free_text = true;
            request.options = options.iter().map(|(l, _d)| l.clone()).collect();
            request.descriptions = options.into_iter().map(|(_l, d)| d).collect();
            let (tx, rx) = oneshot::channel();
            if asks.send((request, tx)).is_err() {
                return Box::pin(async { None });
            }
            Box::pin(async move {
                match rx.await {
                    Ok(DialogAction::Confirm(index)) => {
                        Some(crate::query::AskAnswer::Option(index))
                    }
                    Ok(DialogAction::Answer(text)) => {
                        Some(crate::query::AskAnswer::Other(text))
                    }
                    Ok(DialogAction::Cancel) | Err(_) => None,
                }
            })
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AskUserQuestion's TUI hook: requests go through the permission modal
    /// (title/question/options); confirm → Some(index), Esc cancel → None.
    #[tokio::test]
    async fn ask_question_hook_maps_confirm_and_cancel() {
        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        let (asks_tx, mut asks_rx) = mpsc::unbounded_channel();
        let ui = tui_hooks(events_tx, asks_tx);

        let fut = (ui.ask_question)(
            "技术选型".to_string(),
            "用哪个库？".to_string(),
            vec![
                ("A".to_string(), None),
                ("B".to_string(), Some("更快".to_string())),
            ],
        );
        let (request, tx) = asks_rx.try_recv().expect("模态请求已发出");
        assert_eq!(request.title, "技术选型");
        assert_eq!(request.question, "用哪个库？");
        assert_eq!(request.options, vec!["A", "B"]);
        assert!(request.free_text, "AskUserQuestion 请求带 Other 自由输入");
        tx.send(DialogAction::Confirm(1)).unwrap();
        assert_eq!(
            fut.await,
            Some(crate::query::AskAnswer::Option(1)),
            "按 2 选中 B"
        );

        let fut = (ui.ask_question)(
            "t".to_string(),
            "q?".to_string(),
            vec![("a".to_string(), None)],
        );
        let (_request, tx) = asks_rx.try_recv().expect("第二个模态请求");
        tx.send(DialogAction::Cancel).unwrap();
        assert_eq!(fut.await, None, "Esc 取消 → 未回答");

        let fut = (ui.ask_question)(
            "t".to_string(),
            "q?".to_string(),
            vec![("a".to_string(), None)],
        );
        let (_request, tx) = asks_rx.try_recv().expect("第三个模态请求");
        tx.send(DialogAction::Answer("自定义".into())).unwrap();
        assert_eq!(
            fut.await,
            Some(crate::query::AskAnswer::Other("自定义".to_string())),
            "Other 自由输入回填文本"
        );
    }

    /// Serialized event stream contract: versioned envelope with the event
    /// flattened in, `type`-tagged variants (struct variants keep field names,
    /// newtype variants use `value`).
    #[test]
    fn envelope_versions_and_flattens_the_event() {
        let json = serde_json::to_value(UiEventEnvelope::new(UiEvent::TextDelta("hi".into()))).unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(json["type"], "TextDelta");
        assert_eq!(json["data"], "hi");
    }

    #[test]
    fn struct_variants_serialize_with_field_names() {
        let json = serde_json::to_value(UiEvent::ToolStart { name: "Bash".into() }).unwrap();
        assert_eq!(json["type"], "ToolStart");
        assert_eq!(json["data"]["name"], "Bash");

        let done = UiEvent::ToolDone(crate::query::ToolCallDone {
            name: "Edit".into(),
            summary: "edited 1 file".into(),
            output: String::new(),
            is_error: false,
            diff: None,
            duration_ms: 5,
        });
        let json = serde_json::to_value(done).unwrap();
        assert_eq!(json["type"], "ToolDone");
        assert_eq!(json["data"]["name"], "Edit");
        assert_eq!(json["data"]["duration_ms"], 5);

        let json = serde_json::to_value(UiEvent::WatchEvent {
            label: "watch ls".into(),
            kind: crate::watch::WatchKind::Command,
            status: WatchStatus::Running,
            detail: Some("round 1".into()),
            duration_ms: 3,
            payload: None,
            signal: None,
        })
        .unwrap();
        assert_eq!(json["type"], "WatchEvent");
        assert_eq!(json["data"]["kind"], "Command");
        assert_eq!(json["data"]["status"], "Running");
        assert_eq!(json["data"]["detail"], "round 1");

        let json = serde_json::to_value(UiEvent::Error {
            code: "TIMEOUT",
            msg: "timed out".into(),
            level: crate::error::ErrorLevel::Full,
            context: crate::error::ErrorContext::LongTurn,
        })
        .unwrap();
        assert_eq!(json["type"], "Error");
        assert_eq!(json["data"]["code"], "TIMEOUT");
        assert_eq!(json["data"]["level"], "Full");
        assert_eq!(json["data"]["context"], "LongTurn");

        let json = serde_json::to_value(UiEvent::ImageReady {
            url: "img.png".into(),
            meta: Some(crate::tui::gfx::ImageMeta { cols: 2, rows: 3, bytes: vec![1, 2] }),
        })
        .unwrap();
        assert_eq!(json["type"], "ImageReady");
        assert_eq!(json["data"]["meta"]["cols"], 2);
        assert_eq!(json["data"]["meta"]["bytes"], serde_json::json!([1, 2]));
    }

    #[test]
    fn permission_request_and_dialog_action_serialize() {
        let request = PermissionRequest {
            title: "Allow Bash".into(),
            question: "Run git log?".into(),
            options: vec!["Allow".into(), "Deny".into()],
            descriptions: vec![None],
            free_text: true,
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["title"], "Allow Bash");
        assert_eq!(json["free_text"], true);

        // DialogAction crosses the wire back from the front end: round-trip.
        for action in [DialogAction::Confirm(2), DialogAction::Answer("x".into()), DialogAction::Cancel] {
            let json = serde_json::to_value(&action).unwrap();
            let back: DialogAction = serde_json::from_value(json).unwrap();
            assert_eq!(back, action);
        }
    }
}
