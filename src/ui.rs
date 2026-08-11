//! Renderer-agnostic contract between the agent core and any front end.
//!
//! Nothing here may depend on a terminal library: [`UiEvent`] and the dialog
//! transport types are what a TUI, a GUI or a test harness all consume, and
//! [`tui_hooks`] is the adapter that turns [`UiHooks`] callbacks into channel
//! traffic. Front-end implementations live outside this module.

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use crate::api::contract::StreamEvent;
use crate::query::{ToolCallDone, UiHooks};
use crate::watch::WatchState;

/// A loaded image payload: target cell size plus renderer-ready PNG bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageMeta {
    pub cols: usize,
    pub rows: usize,
    pub bytes: Vec<u8>,
}

/// Permission prompt: request + result receipt.
pub type AskRequest = (PermissionRequest, oneshot::Sender<DialogAction>);

/// Permission dialog result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogAction {
    /// Option `index` (0-based) confirmed.
    Confirm(usize),
    /// AskUserQuestion's Other free-form input submitted.
    Answer(String),
    /// Dialog cancelled with Esc.
    Cancel,
}

/// Permission/question block to display.
#[derive(Debug, Clone)]
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
    pub fn new(
        title: impl Into<String>,
        question: impl Into<String>,
        options: Vec<String>,
    ) -> Self {
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
#[derive(Debug, Clone)]
pub enum UiEvent {
    TurnStart,
    /// All tool calls in a batch finished (one query loop round closed).
    RoundEnd,
    TextDelta(String),
    ThinkingDelta(String),
    ContextUsage {
        used: u64,
        window: u64,
    },
    /// Cumulative output token count for the current model response while it streams.
    OutputTokens(u64),
    ToolStart {
        name: String,
    },
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
        status: WatchState,
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
        /// Fetch failed: short actionable reason shown inside the menu (401
        /// used to masquerade as "the endpoint returned no models").
        failed: Option<String>,
    },
    /// Image finished loading asynchronously after the message was finalized
    /// (meta=None = load failed, placeholder shown).
    ImageReady {
        url: String,
        meta: Option<ImageMeta>,
    },
    TurnEnd,
    /// Non-fatal warning (e.g. MCP connection failure), shown above the input
    /// box; expires after `WARNING_TTL` (10s, filtered at render time).
    Warning(String),
    /// Async slash-command result (/compact /status /context): rendered after the messages.
    SlashOutput(String),
    /// Slash error/usage output (async producers): error tier — 8s floor +
    /// clear on next input.
    SlashError(String),
    /// Informational slash output (async producers): persists until the next
    /// input or Esc.
    SlashInfo(String),
    /// Pin/replace a persistent panel (login flows, long operations): shown
    /// above the prompt until `Unpin` with the same id.
    PinPanel {
        id: String,
        lines: Vec<String>,
    },
    Unpin {
        id: String,
    },
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

/// Permission prompt backed by the TUI modal. Shared by `tui_hooks` and the subagent prompt
/// surface attached to the registry, so a subagent's request lands in the same modal queue.
pub fn modal_ask(asks: mpsc::UnboundedSender<AskRequest>) -> Arc<crate::query::AskFn> {
    Arc::new(move |tool_name, reason| {
        let request = PermissionRequest::new(
            format!("Allow running {tool_name}"),
            reason,
            vec!["Allow".to_string(), "Deny".to_string()],
        );
        let (tx, rx) = oneshot::channel();
        if asks.send((request, tx)).is_err() {
            return Box::pin(async { false });
        }
        Box::pin(async move { matches!(rx.await, Ok(DialogAction::Confirm(0))) })
    })
}

/// Wire query's UiHooks to the TUI channels.
pub fn tui_hooks(
    events: mpsc::UnboundedSender<UiEvent>,
    asks: mpsc::UnboundedSender<AskRequest>,
) -> UiHooks {
    let tool_events = events.clone();
    let ready_events = events.clone();
    let round_events = events.clone();
    let context_events = events.clone();
    let warn_events = events.clone();
    let ask_asks = asks.clone();
    let round_tokens = Arc::new(std::sync::Mutex::new((0u64, None::<usize>)));
    let event_round_tokens = round_tokens.clone();
    UiHooks {
        on_event: Box::new(move |event| match event {
            StreamEvent::TextDelta { index, text } => {
                let tokens = {
                    let mut state = event_round_tokens.lock().unwrap_or_else(|e| e.into_inner());
                    if state.1.is_some_and(|previous| previous > *index) {
                        state.0 = 0;
                    }
                    state.1 = Some(*index);
                    state.0 = state.0.saturating_add(text.chars().count() as u64);
                    state.0.div_ceil(4)
                };
                let _ = events.send(UiEvent::TextDelta(text.clone()));
                let _ = events.send(UiEvent::OutputTokens(tokens));
            }
            StreamEvent::ThinkingDelta { index, thinking } => {
                let tokens = {
                    let mut state = event_round_tokens.lock().unwrap_or_else(|e| e.into_inner());
                    if state.1.is_some_and(|previous| previous > *index) {
                        state.0 = 0;
                    }
                    state.1 = Some(*index);
                    state.0 = state.0.saturating_add(thinking.chars().count() as u64);
                    state.0.div_ceil(4)
                };
                let _ = events.send(UiEvent::ThinkingDelta(thinking.clone()));
                let _ = events.send(UiEvent::OutputTokens(tokens));
            }
            StreamEvent::InputJsonDelta {
                index,
                partial_json,
            } => {
                let tokens = {
                    let mut state = event_round_tokens.lock().unwrap_or_else(|e| e.into_inner());
                    if state.1.is_some_and(|previous| previous > *index) {
                        state.0 = 0;
                    }
                    state.1 = Some(*index);
                    state.0 = state.0.saturating_add(partial_json.chars().count() as u64);
                    state.0.div_ceil(4)
                };
                let _ = events.send(UiEvent::OutputTokens(tokens));
            }
            StreamEvent::ToolUseStart { name, .. } => {
                let _ = events.send(UiEvent::ToolStart { name: name.clone() });
            }
            StreamEvent::StopReason {
                output_tokens: Some(tokens),
                ..
            } => {
                let mut state = event_round_tokens.lock().unwrap_or_else(|e| e.into_inner());
                state.0 = tokens.saturating_mul(4);
                let _ = events.send(UiEvent::OutputTokens(*tokens));
            }
            _ => {}
        }),
        on_context_usage: Arc::new(move |used, window| {
            let _ = context_events.send(UiEvent::ContextUsage { used, window });
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
            *round_tokens.lock().unwrap_or_else(|e| e.into_inner()) = (0, None);
            let _ = round_events.send(UiEvent::RoundEnd);
        }),
        on_warning: Box::new(move |message| {
            let _ = warn_events.send(UiEvent::Warning(message));
        }),
        ask: modal_ask(ask_asks),
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
                    Ok(DialogAction::Answer(text)) => Some(crate::query::AskAnswer::Other(text)),
                    Ok(DialogAction::Cancel) | Err(_) => None,
                }
            })
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tui_hooks_emit_live_token_samples_before_final_usage() {
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let (asks_tx, _asks_rx) = mpsc::unbounded_channel();
        let mut ui = tui_hooks(events_tx, asks_tx);

        (ui.on_context_usage)(12_345, 128_000);
        assert!(matches!(
            events_rx.try_recv(),
            Ok(UiEvent::ContextUsage {
                used: 12_345,
                window: 128_000
            })
        ));

        (ui.on_event)(&StreamEvent::TextDelta {
            index: 0,
            text: "abcdefghijkl".to_string(),
        });
        assert!(matches!(events_rx.try_recv(), Ok(UiEvent::TextDelta(_))));
        assert!(matches!(events_rx.try_recv(), Ok(UiEvent::OutputTokens(3))));

        (ui.on_event)(&StreamEvent::StopReason {
            stop_reason: Some("end_turn".to_string()),
            output_tokens: Some(10),
        });
        assert!(matches!(
            events_rx.try_recv(),
            Ok(UiEvent::OutputTokens(10))
        ));
    }

    /// AskUserQuestion's TUI hook: requests go through the permission modal
    /// (title/question/options); confirm → Some(index), Esc cancel → None.
    #[tokio::test]
    async fn ask_question_hook_maps_confirm_and_cancel() {
        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        let (asks_tx, mut asks_rx) = mpsc::unbounded_channel();
        let ui = tui_hooks(events_tx, asks_tx);

        let fut = (ui.ask_question)(
            "Tech stack".to_string(),
            "Which library?".to_string(),
            vec![
                ("A".to_string(), None),
                ("B".to_string(), Some("faster".to_string())),
            ],
        );
        let (request, tx) = asks_rx.try_recv().expect("modal request was sent");
        assert_eq!(request.title, "Tech stack");
        assert_eq!(request.question, "Which library?");
        assert_eq!(request.options, vec!["A", "B"]);
        assert!(
            request.free_text,
            "AskUserQuestion requests carry Other free-text input"
        );
        tx.send(DialogAction::Confirm(1)).unwrap();
        assert_eq!(
            fut.await,
            Some(crate::query::AskAnswer::Option(1)),
            "press 2 selects B"
        );

        let fut = (ui.ask_question)(
            "t".to_string(),
            "q?".to_string(),
            vec![("a".to_string(), None)],
        );
        let (_request, tx) = asks_rx.try_recv().expect("second modal request");
        tx.send(DialogAction::Cancel).unwrap();
        assert_eq!(fut.await, None, "Esc cancels → no answer");

        let fut = (ui.ask_question)(
            "t".to_string(),
            "q?".to_string(),
            vec![("a".to_string(), None)],
        );
        let (_request, tx) = asks_rx.try_recv().expect("third modal request");
        tx.send(DialogAction::Answer("custom".into())).unwrap();
        assert_eq!(
            fut.await,
            Some(crate::query::AskAnswer::Other("custom".to_string())),
            "Other free-text answer is backfilled"
        );
    }
}
