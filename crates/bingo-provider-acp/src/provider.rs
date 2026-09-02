//! One configured adapter, as a `Provider`.
//!
//! A turn is one `session/prompt`, held open. Only the new user message
//! crosses: an ACP session is stateful and holds everything before it
//! (ADR-0035 §3), so replaying the folded context would tell the agent its own
//! history back. The system prompt does not cross either — the agent has its
//! own — nor do our tools, nor `Effort`, nor `max_tokens` (ADR-0035 §6).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_client_protocol_schema::v1::{
    CancelNotification, ContentBlock, PromptRequest, PromptResponse, SessionId as AcpSessionId,
    TextContent,
};
use async_trait::async_trait;
use bingo_sdk::{
    AuthStatus, CancellationToken, ContentPart, EndpointCapabilities, Message, ModelEvent,
    ModelInfo, ModelRequest, ModelStream, Provider, ProviderError, Role, SessionId,
};
use futures::stream;
use tokio::sync::mpsc;

use crate::config::{self, Adapter};
use crate::events::Mapper;
use crate::session::{Link, Sessions};
use crate::transcript;

type Yielded = Result<ModelEvent, ProviderError>;

pub struct AcpProvider {
    name: String,
    adapter: Adapter,
    sessions: Arc<Sessions>,
    /// What the last handshake said about images. Fails closed: an adapter
    /// nobody has shaken hands with yet is assumed to take none.
    images: AtomicBool,
}

impl AcpProvider {
    pub fn new(name: String, adapter: Adapter, sessions: Arc<Sessions>) -> Self {
        Self {
            name,
            adapter,
            sessions,
            images: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl Provider for AcpProvider {
    fn id(&self) -> &str {
        &self.name
    }

    /// Every adapter files its models under one family: they are all the same
    /// shape, and none of them is a model catalogue can describe.
    fn family(&self) -> &str {
        config::FAMILY
    }

    fn endpoint(&self, _model: &str) -> EndpointCapabilities {
        EndpointCapabilities {
            images: self.images.load(Ordering::Relaxed),
            count_tokens: false,
            caching: false,
        }
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ModelStream, ProviderError> {
        let session = request.session.clone().ok_or_else(nobody)?;
        let (history, asked) = split(&request.messages);
        let link = self
            .sessions
            .prepare(&self.name, &self.adapter, &session, history)
            .await?;
        self.images.store(
            link.capabilities.prompt_capabilities.image,
            Ordering::Relaxed,
        );
        Ok(hold(link, asked, cancel).await)
    }

    /// ACP has no model list. What a session calls its model is bingo's label
    /// for it; the agent chooses its own and is never told ours.
    async fn models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(Vec::new())
    }

    /// The adapter owns its own login — a Claude subscription, a ChatGPT
    /// account, an API key in its environment. Nothing bingo holds signs in
    /// for it, and saying `Missing` would send a person to the wrong place.
    fn auth(&self) -> AuthStatus {
        AuthStatus::NotApplicable
    }
}

fn nobody() -> ProviderError {
    ProviderError::Config {
        message: "an ACP adapter answers for a session, and this request names none".into(),
    }
}

/// The turns before this one, and what the person just said. ACP is stateful,
/// so only the second crosses.
fn split(messages: &[Message]) -> (&[Message], String) {
    match messages.split_last() {
        Some((last, before)) if last.role == Role::User => (before, said(last)),
        // No trailing user turn — a continuation the kernel asked for, or a
        // request built by hand. The whole fold is history and there is
        // nothing new to say.
        _ => (messages, String::new()),
    }
}

fn said(message: &Message) -> String {
    message
        .parts
        .iter()
        .filter_map(ContentPart::as_text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// One `session/prompt`, held open, with the stream folded as it arrives.
async fn hold(link: Arc<Link>, asked: String, cancel: CancellationToken) -> ModelStream {
    let updates = link.listen().await;
    let text = match link.take_preamble().await {
        Some(path) => transcript::first_prompt(&path, &asked),
        None => asked,
    };
    let prompt = PromptRequest::new(
        AcpSessionId::new(link.acp.as_str()),
        vec![ContentBlock::Text(TextContent::new(text))],
    );
    let (out, events) = mpsc::unbounded_channel();
    tokio::spawn(turn(link, updates, prompt, cancel, out));
    Box::pin(stream::unfold(events, |mut events| async move {
        events.recv().await.map(|event| (event, events))
    }))
}

/// The turn: updates folded as they arrive, an interrupt sent as
/// `session/cancel`, and the finish written when the prompt answers.
async fn turn(
    link: Arc<Link>,
    mut updates: mpsc::UnboundedReceiver<agent_client_protocol_schema::v1::SessionNotification>,
    prompt: PromptRequest,
    cancel: CancellationToken,
    out: mpsc::UnboundedSender<Yielded>,
) {
    let mut mapper = Mapper::default();
    let asking = link.connection.call(prompt);
    tokio::pin!(asking);
    let mut told = false;
    let answered = loop {
        tokio::select! {
            biased;
            Some(note) = updates.recv() => {
                if !emit(&out, mapper.update(note.update)) {
                    return;
                }
            }
            answer = &mut asking => break answer,
            () = cancel.cancelled(), if !told => {
                told = true;
                // ADR-0035 §6: one notification, then wait for the agent to
                // stop of its own accord. The child is not killed here — a
                // cancelled turn leaves the session, and the agent, alive.
                let _ = link
                    .connection
                    .notify(CancelNotification::new(AcpSessionId::new(link.acp.as_str())));
            }
        }
    };
    // Whatever the agent said between its last update and its answer.
    while let Ok(note) = updates.try_recv() {
        if !emit(&out, mapper.update(note.update)) {
            return;
        }
    }
    match answered {
        Ok(response) => finish(&out, &mut mapper, &response),
        Err(failed) => {
            let _ = out.send(Err(failed.into()));
        }
    }
}

fn finish(out: &mpsc::UnboundedSender<Yielded>, mapper: &mut Mapper, response: &PromptResponse) {
    emit(out, mapper.finish(response));
}

/// `false` once nobody is reading: the turn was dropped and there is no reason
/// to keep folding.
fn emit(out: &mpsc::UnboundedSender<Yielded>, events: Vec<ModelEvent>) -> bool {
    events.into_iter().all(|event| out.send(Ok(event)).is_ok())
}

/// Every configured adapter, as providers.
pub fn providers(rows: Vec<(String, Adapter)>, sessions: &Arc<Sessions>) -> Vec<Arc<dyn Provider>> {
    rows.into_iter()
        .map(|(name, adapter)| {
            Arc::new(AcpProvider::new(name, adapter, sessions.clone())) as Arc<dyn Provider>
        })
        .collect()
}

/// The session a request was built for, for a caller that has one to give.
pub fn for_session(mut request: ModelRequest, session: SessionId) -> ModelRequest {
    request.session = Some(session);
    request
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::Env;
    use serde_json::json;

    fn adapter() -> Adapter {
        serde_json::from_value(json!({ "command": "true" })).expect("an adapter")
    }

    fn provider() -> AcpProvider {
        AcpProvider::new(
            "claude".into(),
            adapter(),
            Sessions::new(Env::rooted(std::env::temp_dir())),
        )
    }

    fn request(messages: Vec<Message>) -> ModelRequest {
        ModelRequest {
            model: "agent".into(),
            max_tokens: 1,
            system: Vec::new(),
            messages,
            tools: Vec::new(),
            reasoning: None,
            session: None,
            provider_options: Default::default(),
        }
    }

    /// The name is what a person types; the family is the shape it speaks
    /// (ADR-0017, ADR-0035 §1).
    #[test]
    fn an_instance_answers_to_its_own_name_and_files_under_acp() {
        let provider = provider();
        assert_eq!(provider.id(), "claude");
        assert_eq!(provider.family(), "acp");
        assert_eq!(provider.auth(), AuthStatus::NotApplicable);
    }

    /// Fails closed until a handshake says otherwise, and never claims what
    /// ACP has no way to do.
    #[test]
    fn an_adapter_nobody_has_met_yet_promises_nothing() {
        let capabilities = provider().endpoint("agent");
        assert!(!capabilities.images);
        assert!(!capabilities.count_tokens);
        assert!(!capabilities.caching);
    }

    /// Only the new user turn crosses; everything before it is the agent's
    /// own history, or the file it is handed (ADR-0035 §3).
    #[test]
    fn only_the_last_user_turn_crosses_the_wire() {
        let messages = vec![
            Message::text(Role::User, "rename the module"),
            Message::text(Role::Assistant, "Renamed it."),
            Message::text(Role::User, "and the tests?"),
        ];
        let (history, asked) = split(&messages);
        assert_eq!(asked, "and the tests?");
        assert_eq!(history.len(), 2);

        let (history, asked) = split(&messages[..1]);
        assert_eq!(asked, "rename the module");
        assert!(history.is_empty(), "a first turn has no history to carry");
    }

    /// A fold that ends on a tool result is the kernel continuing a round, not
    /// a person saying something new.
    #[test]
    fn a_fold_with_no_new_user_turn_says_nothing_new() {
        let messages = vec![
            Message::text(Role::User, "go"),
            Message::assistant(vec![ContentPart::ToolUse {
                id: "c1".into(),
                name: "Read".into(),
                input: json!({}),
            }]),
        ];
        let (history, asked) = split(&messages);
        assert_eq!(asked, "");
        assert_eq!(history.len(), 2);
    }

    /// A request nobody stamped names no session, and an adapter that keeps a
    /// conversation per session cannot guess which one.
    #[tokio::test]
    async fn a_request_with_no_session_is_refused_before_anything_is_spawned() {
        let failed = provider()
            .stream(
                request(vec![Message::text(Role::User, "hi")]),
                CancellationToken::new(),
            )
            .await
            .err()
            .expect("no session, no adapter");
        assert!(matches!(failed, ProviderError::Config { .. }));
    }

    #[test]
    fn a_request_can_be_told_whose_turn_it_is() {
        let session = SessionId::mint();
        let stamped = for_session(request(Vec::new()), session.clone());
        assert_eq!(stamped.session, Some(session));
    }
}
