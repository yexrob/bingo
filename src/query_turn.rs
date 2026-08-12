use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::watch;

use crate::api::client::{AssistantAccumulator, ClientError};
use crate::api::contract::{NeutralRequest, StreamApiErrorKind, StreamEvent, ThinkingLevel};
use crate::api::types::{ContentBlock, DEFAULT_MAX_TOKENS, Message, Role};
use crate::tool::executor::cancel_requested;
use crate::tool::{Tool, tool_params};

use crate::query::{InboxWake, QueryError, Session, UiHooks};

/// A retry restarts the entire model response, so the failed attempt is discarded before any
/// assistant content or tool call is committed to history.
pub(super) const STREAM_API_MAX_RETRIES: u32 = 10;
pub(super) const STREAM_API_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(500);
pub(super) const STREAM_API_RETRY_MAX_DELAY: Duration = Duration::from_secs(32);

/// Single-turn result: assistant message + the turn's tool_use blocks + stop_reason.
pub(super) struct Turn {
    pub(super) assistant: Message,
    pub(super) tool_uses: Vec<ContentBlock>,
    pub(super) stop_reason: Option<String>,
    /// Cancelled while reading the stream (assistant incomplete, whole turn discarded).
    pub(super) aborted: bool,
}

#[derive(Debug)]
pub(super) struct StreamApiError {
    message: String,
    kind: StreamApiErrorKind,
    retry_after: Option<Duration>,
    emitted_output: bool,
}

#[derive(Debug)]
pub(super) enum OneTurnError {
    Query(QueryError),
    Api(StreamApiError),
}

impl From<QueryError> for OneTurnError {
    fn from(error: QueryError) -> Self {
        Self::Query(error)
    }
}

impl From<ClientError> for OneTurnError {
    fn from(error: ClientError) -> Self {
        Self::Query(QueryError::Client(error))
    }
}

pub(super) fn retryable_stream_api_error(kind: StreamApiErrorKind, message: &str) -> bool {
    match kind {
        StreamApiErrorKind::Retryable => return true,
        StreamApiErrorKind::NonRetryable => return false,
        StreamApiErrorKind::Unknown => {}
    }

    let message = message.to_ascii_lowercase();
    if [
        "insufficient_quota",
        "usage_not_included",
        "invalid_prompt",
        "context_length_exceeded",
        "context overflow",
        "context window",
        "prompt is too long",
        "input is too long",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
    {
        return false;
    }
    let status_5xx = message
        .split(|character: char| !character.is_ascii_digit())
        .any(|digits| matches!(digits.parse::<u16>(), Ok(status) if (500..600).contains(&status)));
    status_5xx
        || [
            "overloaded",
            "server_is_overloaded",
            "server_error",
            "server error",
            "internal_error",
            "internal error",
            "service_unavailable",
            "service unavailable",
            "too_many_requests",
            "too many requests",
            "rate_limit",
            "rate limit",
            "resource_exhausted",
            "resource exhausted",
            "429",
            "try again later",
        ]
        .iter()
        .any(|pattern| message.contains(pattern))
}

pub(super) fn stream_api_backoff(retry: u32, jitter_unit: f64) -> Duration {
    let exponent = retry.saturating_sub(1).min(6);
    let base = STREAM_API_RETRY_INITIAL_DELAY.saturating_mul(1u32 << exponent);
    let jitter = 0.9 + jitter_unit.clamp(0.0, 1.0) * 0.2;
    Duration::from_secs_f64(base.as_secs_f64() * jitter).min(STREAM_API_RETRY_MAX_DELAY)
}

pub(super) fn stream_api_retry_delay(retry: u32, retry_after: Option<Duration>) -> Duration {
    #[cfg(test)]
    if retry_after.is_none() {
        return Duration::ZERO;
    }

    retry_after.unwrap_or_else(|| stream_api_backoff(retry, stream_retry_jitter_unit()))
}

fn stream_retry_jitter_unit() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0);
    f64::from(nanos) / 1_000_000_000.0
}

async fn wait_for_stream_retry(
    delay: Duration,
    cancel: Option<&mut watch::Receiver<bool>>,
    inbox: Option<&mut InboxWake>,
) -> bool {
    match (cancel, inbox) {
        (Some(cancel), Some(inbox)) => {
            if *cancel.borrow_and_update() {
                return false;
            }
            let sleep = tokio::time::sleep(delay);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    _ = &mut sleep => return true,
                    _ = cancel_requested(cancel) => return false,
                    _ = inbox.changed() => {}
                }
            }
        }
        (Some(cancel), None) => {
            if *cancel.borrow_and_update() {
                return false;
            }
            tokio::select! {
                _ = tokio::time::sleep(delay) => true,
                _ = cancel_requested(cancel) => false,
            }
        }
        (None, Some(inbox)) => {
            let sleep = tokio::time::sleep(delay);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    _ = &mut sleep => return true,
                    _ = inbox.changed() => {}
                }
            }
        }
        (None, None) => {
            tokio::time::sleep(delay).await;
            true
        }
    }
}

/// One turn: request the model once and accumulate the assistant reply.
async fn one_turn(
    session: &Arc<Session>,
    messages: &[Message],
    tools: &[Box<dyn Tool>],
    ui: &mut UiHooks,
    mut cancel: Option<&mut watch::Receiver<bool>>,
    mut inbox: Option<&mut InboxWake>,
) -> Result<Turn, OneTurnError> {
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
    let mut emitted_output = false;
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
        emitted_output |= matches!(
            &event,
            StreamEvent::TextDelta { text, .. } if !text.is_empty()
        ) || matches!(
            &event,
            StreamEvent::ThinkingDelta { thinking, .. } if !thinking.is_empty()
        ) || matches!(&event, StreamEvent::ToolUseStart { .. });
        if let Err(e) = acc.push(&event) {
            return Err(QueryError::Protocol(e).into());
        }
        match &event {
            StreamEvent::ApiError {
                message,
                kind,
                retry_after,
            } => {
                return Err(OneTurnError::Api(StreamApiError {
                    message: message.clone(),
                    kind: *kind,
                    retry_after: *retry_after,
                    emitted_output,
                }));
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

pub(super) async fn one_turn_with_stream_retries(
    session: &Arc<Session>,
    messages: &[Message],
    tools: &[Box<dyn Tool>],
    ui: &mut UiHooks,
    mut cancel: Option<&mut watch::Receiver<bool>>,
    mut inbox: Option<&mut InboxWake>,
) -> Result<Turn, QueryError> {
    let mut retries = 0u32;
    loop {
        let inbox_output_chars = inbox.as_deref().map_or(0, |inbox| inbox.output_chars);
        match one_turn(
            session,
            messages,
            tools,
            ui,
            cancel.as_deref_mut(),
            inbox.as_deref_mut(),
        )
        .await
        {
            Ok(turn) => return Ok(turn),
            Err(OneTurnError::Query(error)) => return Err(error),
            Err(OneTurnError::Api(error)) => {
                if !retryable_stream_api_error(error.kind, &error.message)
                    || retries >= STREAM_API_MAX_RETRIES
                {
                    return Err(QueryError::Protocol(error.message));
                }
                retries += 1;
                let delay = stream_api_retry_delay(retries, error.retry_after);
                if error.emitted_output {
                    if let Some(inbox) = inbox.as_deref_mut() {
                        inbox.output_chars = inbox_output_chars;
                    }
                    (ui.on_stream_retry)();
                }
                if retries > 1 {
                    (ui.on_warning)(format!(
                        "Reconnecting... {retries}/{STREAM_API_MAX_RETRIES}"
                    ));
                }
                if !wait_for_stream_retry(delay, cancel.as_deref_mut(), inbox.as_deref_mut()).await
                {
                    return Ok(Turn {
                        assistant: Message {
                            role: Role::Assistant,
                            content: Vec::new(),
                        },
                        tool_uses: Vec::new(),
                        stop_reason: None,
                        aborted: true,
                    });
                }
            }
        }
    }
}

pub(super) async fn retry_after_overflow(
    session: &Arc<Session>,
    messages: &[Message],
    tools: &[Box<dyn Tool>],
    ui: &mut UiHooks,
    cancel: Option<&mut watch::Receiver<bool>>,
    inbox: Option<&mut InboxWake>,
) -> Result<Turn, QueryError> {
    match one_turn_with_stream_retries(session, messages, tools, ui, cancel, inbox).await {
        Err(error @ QueryError::Client(ClientError::ContextOverflow { .. })) => {
            session
                .compact_failures
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(error)
        }
        outcome => outcome,
    }
}
