use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::watch;

use crate::api::client::{AssistantAccumulator, ClientError};
use crate::api::contract::{NeutralRequest, StreamApiErrorKind, StreamEvent, ThinkingLevel};
use crate::api::types::{ContentBlock, Message, Role};
use crate::tool::executor::cancel_requested;
use crate::tool::{Tool, tool_params};

use crate::api::providers::{RETRY_INITIAL_DELAY, RETRY_MAX_DELAY, backoff_delay, jitter_unit};
use crate::engine::events::{EngineEvent, EngineHost};
use crate::query::{InboxWake, QueryError, RECONNECT_WARNING_PREFIX, Session};

/// A retry restarts the entire model response, so the failed attempt is discarded before any
/// assistant content or tool call is committed to history.
pub(super) const STREAM_API_MAX_RETRIES: u32 = 10;

/// In-stream retry pacing. Test builds shrink only the delay data (`active`), so the loop
/// and its delay selection run the same code at test speed.
pub(super) struct StreamRetryPolicy {
    pub(super) max_retries: u32,
    initial_delay: Duration,
    max_delay: Duration,
    /// Bound on a server-directed delay: an absurd `retry_after` must not park the turn
    /// for an hour behind a suppressed first notice.
    max_server_delay: Duration,
}

impl Default for StreamRetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: STREAM_API_MAX_RETRIES,
            initial_delay: RETRY_INITIAL_DELAY,
            max_delay: RETRY_MAX_DELAY,
            max_server_delay: Duration::from_secs(60),
        }
    }
}

impl StreamRetryPolicy {
    fn active() -> Self {
        if cfg!(test) {
            Self {
                initial_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(2),
                max_server_delay: Duration::from_millis(2),
                ..Self::default()
            }
        } else {
            Self::default()
        }
    }

    fn delay(&self, retry: u32, retry_after: Option<Duration>) -> Duration {
        match retry_after {
            Some(server) => server.min(self.max_server_delay),
            None => backoff_delay(retry, self.initial_delay, self.max_delay, jitter_unit()),
        }
    }
}

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
    host: &EngineHost,
    mut cancel: Option<&mut watch::Receiver<bool>>,
    mut inbox: Option<&mut InboxWake>,
) -> Result<Turn, OneTurnError> {
    let model = session.runtime.model.borrow().clone();
    let thinking = session.runtime.thinking.borrow().clone();
    // Thinking gate: models that reject the parameter (DeepSeek family) get
    // none regardless of the configured level — the UI shows the same fact
    // when the level is set, so display and wire agree.
    let thinking = if session.client.models().supports_thinking(&model) {
        ThinkingLevel::parse(thinking.as_deref())
    } else {
        None
    };
    let max_tokens = crate::budget::max_tokens_for(&session.client.models(), &model);
    // Capability block refreshed per request: /model and /provider switch the
    // runtime without touching Session::system, and the vision/thinking facts
    // must describe the model actually speaking.
    let system = crate::system::with_model_capabilities(
        &session.system,
        &model,
        &session.runtime.provider.borrow().clone(),
        &session.client.models(),
    );
    let request = NeutralRequest {
        model,
        max_tokens,
        system,
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
        host.events.emit_stream(&event);
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
                    host.events.emit(EngineEvent::ToolReady {
                        tool_call_id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                        standalone: false,
                    });
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
    host: &EngineHost,
    mut cancel: Option<&mut watch::Receiver<bool>>,
    mut inbox: Option<&mut InboxWake>,
) -> Result<Turn, QueryError> {
    let policy = StreamRetryPolicy::active();
    let mut retries = 0u32;
    loop {
        let inbox_output_chars = inbox.as_deref().map_or(0, |inbox| inbox.output_chars);
        match one_turn(
            session,
            messages,
            tools,
            host,
            cancel.as_deref_mut(),
            inbox.as_deref_mut(),
        )
        .await
        {
            Ok(turn) => return Ok(turn),
            Err(OneTurnError::Query(error)) => return Err(error),
            Err(OneTurnError::Api(error)) => {
                if !error.kind.retryable(&error.message) || retries >= policy.max_retries {
                    return Err(QueryError::Protocol(error.message));
                }
                retries += 1;
                let delay = policy.delay(retries, error.retry_after);
                if error.emitted_output {
                    if let Some(inbox) = inbox.as_deref_mut() {
                        inbox.output_chars = inbox_output_chars;
                    }
                    host.events.emit(EngineEvent::StreamRetry);
                }
                if retries > 1 {
                    host.events.warn(format!(
                        "{RECONNECT_WARNING_PREFIX}{retries}/{max}",
                        max = policy.max_retries
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
    host: &EngineHost,
    cancel: Option<&mut watch::Receiver<bool>>,
    inbox: Option<&mut InboxWake>,
) -> Result<Turn, QueryError> {
    match one_turn_with_stream_retries(session, messages, tools, host, cancel, inbox).await {
        Err(error @ QueryError::Client(ClientError::ContextOverflow { .. })) => {
            session
                .compact_failures
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(error)
        }
        outcome => outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_directed_retry_delay_wins_but_is_clamped() {
        let policy = StreamRetryPolicy::default();
        assert_eq!(
            policy.delay(10, Some(Duration::from_millis(125))),
            Duration::from_millis(125),
            "the server-provided retry delay wins over local backoff"
        );
        assert_eq!(
            policy.delay(1, Some(Duration::from_secs(3600))),
            Duration::from_secs(60),
            "an absurd server delay is clamped instead of parking the turn"
        );
    }

    #[test]
    fn local_backoff_stays_within_the_jitter_bounds() {
        let policy = StreamRetryPolicy::default();
        let delay = policy.delay(1, None);
        assert!(
            (Duration::from_millis(450)..=Duration::from_millis(550)).contains(&delay),
            "{delay:?}"
        );
    }
}
