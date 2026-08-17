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

/// What approving a permission request would actually do, rendered above the
/// options. The prompt names the tool; this shows the act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskPreview {
    /// The shell command that would run.
    Command(String),
    /// A dry-run unified diff of the file change that would be made — computed
    /// without touching the file.
    Diff(String),
}

/// Which dialog shape a request wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AskKind {
    /// AskUserQuestion: the model's own options plus an Other free-text row.
    #[default]
    Question,
    /// The permission gate: the three-option approval shape, whose refusal
    /// option opens a feedback row instead of resolving immediately.
    Permission,
}

/// Approval options, in CC's wording. Option 2 is only offered when a session
/// rule could actually be installed ([`PermissionRequest::scope`]).
pub const ASK_YES: &str = "Yes";
pub const ASK_YES_SESSION: &str = "Yes, and don't ask again this session";
pub const ASK_NO: &str = "No, and tell bingo what to do differently (esc)";

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
    /// A free-form input row is open: AskUserQuestion's "Other" (set from the
    /// start), or a permission prompt's refusal feedback (opened on demand).
    pub free_text: bool,
    /// Dialog shape.
    pub kind: AskKind,
    /// What approving would do (permission prompts).
    pub preview: Option<AskPreview>,
    /// The session-scoped allow rule the "don't ask again" option installs.
    /// `None`: that option is not offered, because nothing could make the gate
    /// stop asking about this call.
    pub scope: Option<String>,
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
            kind: AskKind::Question,
            preview: None,
            scope: None,
        }
    }

    /// Index of the "don't ask again this session" option, when it is offered.
    pub fn session_option(&self) -> Option<usize> {
        (self.kind == AskKind::Permission && self.scope.is_some()).then_some(1)
    }

    /// Index of the refusal option (always last on a permission prompt).
    pub fn refusal_option(&self) -> Option<usize> {
        (self.kind == AskKind::Permission).then(|| self.options.len().saturating_sub(1))
    }
}

/// Which conversation an event, or a page, belongs to.
///
/// Main is a key like any other (D134). It differs in exactly one way — it
/// talks to the user by default — and that difference lives in the composer,
/// not in the store.
///
/// `Room` never appears on an [`Addressed`] event: a room is a log, not a turn
/// loop, so nothing streams into it. It is a key because the console keeps a
/// store per *page*, and a room is one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConvKey {
    Main,
    Agent(String),
    Room(String),
}

impl ConvKey {
    pub fn is_main(&self) -> bool {
        matches!(self, ConvKey::Main)
    }

    /// The instance whose stream this addresses, if any.
    pub fn agent(&self) -> Option<&str> {
        match self {
            ConvKey::Agent(name) => Some(name),
            _ => None,
        }
    }
}

/// One [`UiEvent`] and the conversation that produced it.
#[derive(Debug, Clone)]
pub struct Addressed {
    pub to: ConvKey,
    pub event: UiEvent,
}

/// A [`UiEvent`] sender bound to one conversation.
///
/// The binding is what makes a subagent's stream reach the same handler main's
/// does: the producer says what happened, the sink says whose turn it happened
/// in, and nothing downstream has to guess.
#[derive(Debug, Clone)]
pub struct EventSink {
    to: ConvKey,
    tx: mpsc::UnboundedSender<Addressed>,
}

impl EventSink {
    pub fn new(to: ConvKey, tx: mpsc::UnboundedSender<Addressed>) -> Self {
        Self { to, tx }
    }

    /// A sink nobody is listening to: an embedded or headless run, whose turns
    /// reach no screen. Sends are dropped the same way a closed channel's are,
    /// so no producer has to branch on having an audience.
    pub fn detached() -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        Self {
            to: ConvKey::Main,
            tx,
        }
    }

    /// Re-point at another conversation over the same channel.
    pub fn bound_to(&self, to: ConvKey) -> Self {
        Self {
            to,
            tx: self.tx.clone(),
        }
    }

    /// A closed channel means the console is gone; a turn still finishing then
    /// has nobody to tell, which is not an error.
    pub fn send(&self, event: UiEvent) {
        let _ = self.tx.send(Addressed {
            to: self.to.clone(),
            event,
        });
    }
}

/// Event channel from the agent task to components.
#[derive(Debug, Clone)]
pub enum UiEvent {
    TurnStart,
    /// Discard all live output and tool rows produced by the current model-response attempt before
    /// a transparent stream reconnect. Persisted history is unchanged because the attempt has not
    /// committed yet.
    StreamRetry,
    /// All tool calls in a batch finished (one query loop round closed).
    RoundEnd,
    TextDelta(String),
    ThinkingDelta(String),
    ContextUsage(crate::context_usage::ContextUsage),
    /// Cumulative output token count for the current model response while it streams.
    /// `authoritative`: the end-of-round usage total (message_delta), an accounting
    /// correction rather than freshly streamed output — the rate sampler must not
    /// read the jump as an instantaneous burst.
    OutputTokens {
        tokens: u64,
        authoritative: bool,
    },
    ToolStart {
        name: String,
    },
    /// Tool block fully received while streaming (including input): the fold decision point.
    /// standalone=true: non-model tools like the `!` command — summary only, not part of a fold group.
    ToolReady {
        tool_call_id: String,
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
        /// This transition put a task notification in **main's** queue (D106):
        /// the flow prints one line when one arrives, and only the registry can
        /// answer whether one did.
        notifies_main: bool,
        /// The run was born from an `Agent` call (D114). The flow's whitelist:
        /// only a dispatched run staples a row into a streaming turn or prints
        /// the dim `●` notice; deliveries and continuations stay in the tree
        /// and the dialog.
        dispatch: bool,
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
    /// Prose that entered this conversation from outside its own turn: the task
    /// an agent was dispatched with, the batch of mail a continuation absorbed,
    /// a room relay claimed at a round boundary.
    ///
    /// Main's equivalent is the user pressing Enter, which the console holds
    /// already — so main's hook does nothing and this is the one event an agent
    /// produces that main does not. The text is the prompt exactly as the model
    /// received it, markers and all; the console files it with the same
    /// attribution walk that reads a committed history, so the two cannot
    /// disagree about who said what.
    Inbound(String),
    /// A direct message *landing in this conversation's inbox* — the moment it
    /// was sent, not the moment the receiver got round to reading it (D135).
    ///
    /// [`Inbound`](UiEvent::Inbound) is the reading: it fires when a run absorbs
    /// its prompt, which for a busy instance is its next tool barrier, minutes
    /// later. A user watching that instance could see what *they* had asked for
    /// (the console echoed it at send time) and nothing of what main had. So the
    /// echo moves to the one place every sender passes through
    /// ([`crate::agents::AgentRegistry::deliver`]) and covers all of them, and
    /// the absorbed prompt's DM lines are dropped as the repeat they are.
    Mail {
        from: String,
        text: String,
    },
    /// The running turn took these queued messages into its own context at a tool
    /// barrier (D83). They are already in the request, so the composer must drop them
    /// from its queue and show them in the flow where the model read them — the turn
    /// side is the authority here, and a pull-back racing this event loses.
    Steered {
        items: Vec<crate::steer::SteerItem>,
    },
    /// A foreground shell command's output so far (D84): the last few lines, dim,
    /// under the running tool row, replaced on every sample. No tool id travels with
    /// it — Phase 2 runs non-concurrency-safe tools serially, so exactly one
    /// foreground command can be in flight, and the renderer finds it the same way
    /// [`UiEvent::ToolDone`] does. The rows live in the redrawn tail region and never
    /// reach scrollback: a running tool row keeps its message unsettled.
    BashTail(crate::live::LiveTail),
    /// The user interrupted the turn. `marker` is the exact string the transcript
    /// recorded (`crate::query::INTERRUPT_MARKER` / `…_TOOL_USE`), echoed into the
    /// message flow so the screen and the model read the same sentence — a transient
    /// warning would have expired while the marker stayed in the history.
    Interrupted {
        marker: &'static str,
    },
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
    /// A rewind that finished off the key path (D91): its state line, for the
    /// flow rather than for a tier that expires.
    RewindDone(String),
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
    use crate::query::AskOutcome;
    Arc::new(move |ask| {
        let mut options = vec![ASK_YES.to_string()];
        if ask.scope.is_some() {
            options.push(ASK_YES_SESSION.to_string());
        }
        options.push(ASK_NO.to_string());
        let mut request =
            PermissionRequest::new(format!("Allow running {}", ask.tool), ask.reason, options);
        request.kind = AskKind::Permission;
        request.scope = ask.scope.map(str::to_string);
        request.preview = ask_preview(ask);
        let session_option = request.session_option();
        let (tx, rx) = oneshot::channel();
        if asks.send((request, tx)).is_err() {
            return Box::pin(async { AskOutcome::Deny { feedback: None } });
        }
        Box::pin(async move {
            match rx.await {
                Ok(DialogAction::Confirm(0)) => AskOutcome::Allow,
                Ok(DialogAction::Confirm(index)) if Some(index) == session_option => {
                    AskOutcome::AllowSession
                }
                Ok(DialogAction::Answer(feedback)) => AskOutcome::Deny {
                    feedback: Some(feedback),
                },
                // The refusal option, Esc, and a dialog the turn outlived all
                // land here: fail closed, and say nothing on the user's behalf.
                _ => AskOutcome::Deny { feedback: None },
            }
        })
    })
}

/// The preview rows: a file change shows the diff it would make, anything
/// carrying a shell command shows the command.
fn ask_preview(ask: &crate::query::AskContext<'_>) -> Option<AskPreview> {
    if let Some(diff) = ask.diff {
        return Some(AskPreview::Diff(diff.to_string()));
    }
    ask.input
        .get("command")
        .and_then(|v| v.as_str())
        .map(|command| AskPreview::Command(command.to_string()))
}

/// Wire query's UiHooks to the TUI channels.
pub fn tui_hooks(
    events: EventSink,
    asks: mpsc::UnboundedSender<AskRequest>,
    steer: crate::steer::SteerQueue,
    live: Arc<crate::live::LiveBash>,
) -> UiHooks {
    let tool_events = events.clone();
    let steer_events = events.clone();
    let ready_events = events.clone();
    let retry_events = events.clone();
    let round_events = events.clone();
    let context_events = events.clone();
    let warn_events = events.clone();
    let ask_asks = asks.clone();
    let round_tokens = Arc::new(std::sync::Mutex::new((0u64, None::<usize>)));
    let event_round_tokens = round_tokens.clone();
    let retry_round_tokens = round_tokens.clone();
    UiHooks {
        on_event: Box::new(move |event| match event {
            StreamEvent::TextDelta { index, text } => {
                let tokens = {
                    let mut state = event_round_tokens.lock().unwrap_or_else(|e| e.into_inner());
                    if state.1.is_some_and(|previous| previous > *index) {
                        state.0 = 0;
                    }
                    state.1 = Some(*index);
                    state.0 = state.0.saturating_add(crate::compact::text_units(text));
                    state.0.div_ceil(4)
                };
                events.send(UiEvent::TextDelta(text.clone()));
                events.send(UiEvent::OutputTokens {
                    tokens,
                    authoritative: false,
                });
            }
            StreamEvent::ThinkingDelta { index, thinking } => {
                let tokens = {
                    let mut state = event_round_tokens.lock().unwrap_or_else(|e| e.into_inner());
                    if state.1.is_some_and(|previous| previous > *index) {
                        state.0 = 0;
                    }
                    state.1 = Some(*index);
                    state.0 = state.0.saturating_add(crate::compact::text_units(thinking));
                    state.0.div_ceil(4)
                };
                events.send(UiEvent::ThinkingDelta(thinking.clone()));
                events.send(UiEvent::OutputTokens {
                    tokens,
                    authoritative: false,
                });
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
                    state.0 = state
                        .0
                        .saturating_add(crate::compact::text_units(partial_json));
                    state.0.div_ceil(4)
                };
                events.send(UiEvent::OutputTokens {
                    tokens,
                    authoritative: false,
                });
            }
            StreamEvent::ToolUseStart { name, .. } => {
                events.send(UiEvent::ToolStart { name: name.clone() });
            }
            StreamEvent::StopReason {
                output_tokens: Some(tokens),
                ..
            } => {
                let mut state = event_round_tokens.lock().unwrap_or_else(|e| e.into_inner());
                state.0 = tokens.saturating_mul(4);
                events.send(UiEvent::OutputTokens {
                    tokens: *tokens,
                    authoritative: true,
                });
            }
            _ => {}
        }),
        on_stream_retry: Box::new(move || {
            *retry_round_tokens.lock().unwrap_or_else(|e| e.into_inner()) = (0, None);
            retry_events.send(UiEvent::StreamRetry);
        }),
        on_context_usage: Arc::new(move |usage| {
            context_events.send(UiEvent::ContextUsage(usage));
        }),
        on_tool_ready: Box::new(move |tool_call_id, name, input, standalone| {
            ready_events.send(UiEvent::ToolReady {
                tool_call_id,
                name,
                input,
                standalone,
            });
        }),
        on_tool_done: Box::new(move |done| {
            tool_events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
                tool_call_id: done.tool_call_id.clone(),
                name: done.name.clone(),
                summary: done.summary.clone(),
                output: done.output.clone(),
                status: done.status,
                diff: done.diff.clone(),
                duration_ms: done.duration_ms,
            }));
        }),
        on_round_end: Box::new(move || {
            *round_tokens.lock().unwrap_or_else(|e| e.into_inner()) = (0, None);
            round_events.send(UiEvent::RoundEnd);
        }),
        on_warning: Box::new(move |message| {
            warn_events.send(UiEvent::Warning(message));
        }),
        // Main's inbound is the composer: the console put the line in its own
        // transcript before the turn ever started, and echoing it here would
        // print it twice.
        on_inbound: Box::new(|_| {}),
        // Take and announce in one step, under the queue's own lock: whoever takes the
        // items owns them, and the composer learns of it from the same act. Splitting
        // the two would open a window in which an item is in the request and still
        // pending on screen.
        steer: Arc::new(move || {
            let items = steer.take();
            if !items.is_empty() {
                steer_events.send(UiEvent::Steered {
                    items: items.clone(),
                });
            }
            items
        }),
        live,
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
        let mut ui = tui_hooks(
            EventSink::new(ConvKey::Main, events_tx),
            asks_tx,
            crate::steer::SteerQueue::new(),
            crate::live::LiveBash::detached(),
        );

        (ui.on_context_usage)(crate::context_usage::ContextUsage::new(
            12_345, 128_000, 100_000,
        ));
        assert!(matches!(
            events_rx.try_recv(),
            Ok(Addressed { to: ConvKey::Main, event: UiEvent::ContextUsage(usage) })
                if usage.used == 12_345 && usage.window == 128_000
        ));

        (ui.on_event)(&StreamEvent::TextDelta {
            index: 0,
            text: "abcdefghijkl".to_string(),
        });
        let next = |rx: &mut mpsc::UnboundedReceiver<Addressed>| rx.try_recv().map(|a| a.event);
        assert!(matches!(next(&mut events_rx), Ok(UiEvent::TextDelta(_))));
        assert!(matches!(
            next(&mut events_rx),
            Ok(UiEvent::OutputTokens {
                tokens: 3,
                authoritative: false
            })
        ));

        (ui.on_stream_retry)();
        assert!(matches!(next(&mut events_rx), Ok(UiEvent::StreamRetry)));

        (ui.on_event)(&StreamEvent::StopReason {
            stop_reason: Some("end_turn".to_string()),
            output_tokens: Some(10),
        });
        assert!(matches!(
            next(&mut events_rx),
            Ok(UiEvent::OutputTokens {
                tokens: 10,
                authoritative: true
            })
        ));
    }

    /// AskUserQuestion's TUI hook: requests go through the permission modal
    /// (title/question/options); confirm → Some(index), Esc cancel → None.
    #[tokio::test]
    async fn ask_question_hook_maps_confirm_and_cancel() {
        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        let (asks_tx, mut asks_rx) = mpsc::unbounded_channel();
        let ui = tui_hooks(
            EventSink::new(ConvKey::Main, events_tx),
            asks_tx,
            crate::steer::SteerQueue::new(),
            crate::live::LiveBash::detached(),
        );

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
