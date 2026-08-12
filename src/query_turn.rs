use std::sync::Arc;

use futures_util::StreamExt;
use tokio::sync::watch;

use crate::api::client::{AssistantAccumulator, ClientError};
use crate::api::contract::{NeutralRequest, StreamEvent, ThinkingLevel};
use crate::api::types::{ContentBlock, DEFAULT_MAX_TOKENS, Message};
use crate::tool::executor::cancel_requested;
use crate::tool::{Tool, tool_params};

use crate::query::{InboxWake, QueryError, Session, UiHooks};

/// Single-turn result: assistant message + the turn's tool_use blocks + stop_reason.
pub(super) struct Turn {
    pub(super) assistant: Message,
    pub(super) tool_uses: Vec<ContentBlock>,
    pub(super) stop_reason: Option<String>,
    /// Cancelled while reading the stream (assistant incomplete, whole turn discarded).
    pub(super) aborted: bool,
}

/// One turn: request the model once and accumulate the assistant reply.
pub(super) async fn one_turn(
    session: &Arc<Session>,
    messages: &[Message],
    tools: &[Box<dyn Tool>],
    ui: &mut UiHooks,
    mut cancel: Option<&mut watch::Receiver<bool>>,
    mut inbox: Option<&mut InboxWake>,
) -> Result<Turn, QueryError> {
    let model = session.runtime.model.borrow().clone();
    let thinking = session.runtime.thinking.borrow().clone();
    // Thinking gate: models that reject the parameter (DeepSeek family) get
    // none regardless of the configured level — the UI shows the same fact
    // when the level is set, so display and wire agree.
    let thinking = if crate::api::models::supports_thinking(&model) {
        ThinkingLevel::parse(thinking.as_deref())
    } else {
        None
    };
    let request = NeutralRequest {
        model,
        max_tokens: DEFAULT_MAX_TOKENS,
        system: session.system.clone(),
        messages: messages.to_vec(),
        tools: tool_params(tools),
        stream: true,
        thinking,
    };
    // The connect phase is also interruptible (Esc gives up immediately on a hanging/
    // retrying connection, without waiting for output to start).
    let mut acc = AssistantAccumulator::new();
    let aborted_turn = |acc: &AssistantAccumulator| Turn {
        assistant: acc.message(),
        tool_uses: Vec::new(),
        stop_reason: None,
        aborted: true,
    };
    let stream_request = session.client.stream(&request);
    futures_util::pin_mut!(stream_request);
    let mut stream = loop {
        let result = match (cancel.as_deref_mut(), inbox.as_deref_mut()) {
            (Some(cancel), Some(inbox)) => {
                if *cancel.borrow_and_update() {
                    return Ok(aborted_turn(&acc));
                }
                tokio::select! {
                    stream = &mut stream_request => Some(stream),
                    _ = cancel_requested(cancel) => return Ok(aborted_turn(&acc)),
                    _ = inbox.changed() => None,
                }
            }
            (Some(cancel), None) => {
                if *cancel.borrow_and_update() {
                    return Ok(aborted_turn(&acc));
                }
                tokio::select! {
                    stream = &mut stream_request => Some(stream),
                    _ = cancel_requested(cancel) => return Ok(aborted_turn(&acc)),
                }
            }
            (None, Some(inbox)) => tokio::select! {
                stream = &mut stream_request => Some(stream),
                _ = inbox.changed() => None,
            },
            (None, None) => Some((&mut stream_request).await),
        };
        if let Some(stream) = result {
            break stream?;
        }
    };
    let mut tool_uses = Vec::new();
    let mut aborted = false;
    loop {
        let event = match (cancel.as_deref_mut(), inbox.as_deref_mut()) {
            (Some(cancel), Some(inbox)) => tokio::select! {
                maybe = stream.next() => maybe,
                _ = cancel_requested(cancel) => {
                    aborted = true;
                    None
                }
                _ = inbox.changed() => continue,
            },
            (Some(cancel), None) => tokio::select! {
                maybe = stream.next() => maybe,
                _ = cancel_requested(cancel) => {
                    aborted = true;
                    None
                }
            },
            (None, Some(inbox)) => tokio::select! {
                maybe = stream.next() => maybe,
                _ = inbox.changed() => continue,
            },
            (None, None) => stream.next().await,
        };
        let Some(event) = event else { break };
        let event = event?;
        if let StreamEvent::TextDelta { text, .. } = &event
            && let Some(inbox) = inbox.as_deref_mut()
        {
            inbox.output_chars += text.chars().count();
        }
        (ui.on_event)(&event);
        if let Err(e) = acc.push(&event) {
            return Err(QueryError::Protocol(e));
        }
        match &event {
            StreamEvent::ApiError { message } => {
                return Err(QueryError::Protocol(message.clone()));
            }
            StreamEvent::BlockStop { index } => {
                if let Some(ContentBlock::ToolUse { id, name, input }) = acc.content.get(*index) {
                    tool_uses.push(ContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    });
                    (ui.on_tool_ready)(name.clone(), input.clone(), false);
                }
            }
            _ => {}
        }
    }
    acc.finish();
    Ok(Turn {
        assistant: acc.message(),
        tool_uses,
        stop_reason: acc.stop_reason,
        aborted,
    })
}

pub(super) async fn retry_after_overflow(
    session: &Arc<Session>,
    messages: &[Message],
    tools: &[Box<dyn Tool>],
    ui: &mut UiHooks,
    cancel: Option<&mut watch::Receiver<bool>>,
    inbox: Option<&mut InboxWake>,
) -> Result<Turn, QueryError> {
    match one_turn(session, messages, tools, ui, cancel, inbox).await {
        Err(error @ QueryError::Client(ClientError::ContextOverflow { .. })) => {
            session
                .compact_failures
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(error)
        }
        outcome => outcome,
    }
}
