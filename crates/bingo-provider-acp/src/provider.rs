//! One configured adapter, as a `Provider`.
//!
//! A turn is one `session/prompt`, held open. Only the new user message
//! crosses: an ACP session is stateful and holds everything before it
//! (ADR-0035 §3), so replaying the folded context would tell the agent its own
//! history back. The system prompt does not cross either — the agent has its
//! own — nor `max_tokens` (ADR-0035 §6). `Effort` and the model do, but not
//! on this wire: they are the agent's own knobs, turned between turns through
//! the options it declared (ADR-0037, `crate::knobs`). The tools do, but not
//! on this wire: they were handed over at `session/new` as an MCP server the
//! agent dials, and the request's list of them only says what that server now
//! offers (ADR-0036 §1).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_client_protocol_schema::v1::{
    CancelNotification, ContentBlock, PromptRequest, PromptResponse, SessionId as AcpSessionId,
    TextContent,
};
use async_trait::async_trait;
use bingo_sdk::{
    AuthStatus, CancellationToken, ContentPart, Effort, EndpointCapabilities, Message, ModelEvent,
    ModelInfo, ModelRequest, ModelStream, Provider, ProviderError, Role, SessionId, ToolSpec,
};
use futures::stream;
use tokio::sync::mpsc;

use crate::config::{self, AGENT, Adapter};
use crate::error::AcpError;
use crate::events::Mapper;
use crate::knobs::Wanted;
use crate::session::{Link, Sessions};

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

    /// The request, taken apart into what it takes to ask it — and taken
    /// apart by moving: the turn keeps every piece for as long as it might
    /// have to ask again, and a copy of the fold would be the conversation
    /// twice in memory.
    fn asking(&self, request: ModelRequest) -> Result<Asking, ProviderError> {
        let session = request.session.ok_or_else(nobody)?;
        let (history, asked) = split(request.messages);
        Ok(Asking {
            sessions: self.sessions.clone(),
            name: self.name.clone(),
            adapter: self.adapter.clone(),
            session,
            history,
            asked,
            tools: request.tools,
            reasoning: request.reasoning,
            model: request.model,
        })
    }
}

/// One request's question, and everything it takes to put it again.
///
/// A child that died between turns is replaced rather than asked (ADR-0035
/// §3). `Sessions::prepare` finds most of those deaths before the prompt is
/// written, but that check is an optimisation and not the rule: a child can go
/// in the moment between it and the write, and the rule has to hold wherever
/// the death is discovered. So the turn keeps what it was built from, and a
/// prompt that comes back as a dead pipe having said nothing is asked once
/// more on a new child.
struct Asking {
    sessions: Arc<Sessions>,
    name: String,
    adapter: Adapter,
    session: SessionId,
    /// The turns before this one: what the restore ladder is handed when it
    /// has to open a conversation from nothing.
    history: Vec<Message>,
    /// What the person just said, which is all of the fold that crosses.
    asked: String,
    tools: Vec<ToolSpec>,
    reasoning: Option<Effort>,
    model: String,
}

impl Asking {
    /// A link ready to carry this request: the ladder climbed if it has to be,
    /// the doors handed this request's offer, the knobs turned to what it asks
    /// for. All of it, both times — a turn asked again is not a lesser turn.
    async fn ready(&self) -> Result<Arc<Link>, ProviderError> {
        let link = self
            .sessions
            .prepare(&self.name, &self.adapter, &self.session, &self.history)
            .await?;
        // The offer is this request's own tool list (ADR-0036 §1). A request
        // that moves it is what `tools/list_changed` is made of, so the bridge
        // hears about it before the prompt that will be answered with it.
        if link.observe(&self.tools).await {
            self.sessions.offer_changed().await;
        }
        // ADR-0037 §4: between turns, never inside one. What the request asks
        // for is applied to the agent before the prompt that will be answered
        // under it — and only what moved crosses.
        self.sessions
            .tune(
                &self.name,
                &link,
                Wanted {
                    effort: self.reasoning,
                    model: &self.model,
                },
            )
            .await;
        Ok(link)
    }

    /// The same, on a child of its own. The one that was there is let go of
    /// first — and the person told it went — so the ladder opens a
    /// conversation instead of handing back the dead one.
    async fn afresh(&self) -> Result<Arc<Link>, ProviderError> {
        self.sessions.bury(&self.name, &self.session).await;
        self.ready().await
    }

    /// What crosses: what the link has to say first, then what the person
    /// said. Taken from the link that will carry it, never before — a freshly
    /// opened child has its own first words, and a turn asked again on one
    /// must say them.
    async fn prompt(&self, link: &Link) -> PromptRequest {
        let text = match link.take_preamble().await {
            Some(said) => format!("{said}\n\n{}", self.asked),
            None => self.asked.clone(),
        };
        PromptRequest::new(
            AcpSessionId::new(link.acp.as_str()),
            vec![ContentBlock::Text(TextContent::new(text))],
        )
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
        let asking = self.asking(request)?;
        let link = asking.ready().await?;
        self.images.store(
            link.capabilities.prompt_capabilities.image,
            Ordering::Relaxed,
        );
        Ok(hold(link, asking, cancel))
    }

    /// The agent's own catalogue (ADR-0037 §2), served through the door every
    /// endpoint-answered list rides (ADR-0026).
    ///
    /// An external agent's models are per-session state and nothing else:
    /// there is no door in the protocol that answers "what do you serve"
    /// before a session is open, which is why they are harvested at all three
    /// that do — `session/new`, `session/load` and `session/resume` — and
    /// refreshed from every set the client makes.
    ///
    /// With no conversation to read them from, one is opened for the asking
    /// and dropped (`crate::probe`): a session is the only door, so the cold
    /// answer is to knock on it. A knock nobody answers is `agent` alone and a
    /// notice — a catalogue must not fail.
    async fn models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(catalogue(
            self.sessions.models(&self.name, &self.adapter).await,
        ))
    }

    /// The adapter owns its own login — a Claude subscription, a ChatGPT
    /// account, an API key in its environment. Nothing bingo holds signs in
    /// for it, and saying `Missing` would send a person to the wrong place.
    fn auth(&self) -> AuthStatus {
        AuthStatus::NotApplicable
    }
}

/// `agent` first and always: it is the one id an ACP instance serves whatever
/// the agent has said, and it means "whatever you would have used". An agent
/// that happens to call one of its own models that too is served once.
fn catalogue(theirs: Vec<ModelInfo>) -> Vec<ModelInfo> {
    std::iter::once(ModelInfo {
        id: AGENT.to_string(),
        display: None,
    })
    .chain(theirs.into_iter().filter(|model| model.id != AGENT))
    .collect()
}

fn nobody() -> ProviderError {
    ProviderError::Config {
        message: "an ACP adapter answers for a session, and this request names none".into(),
    }
}

/// The turns before this one, and what the person just said. ACP is stateful,
/// so only the second crosses. Consuming: both halves outlive the request they
/// came from, because the turn may have to be asked again.
fn split(mut messages: Vec<Message>) -> (Vec<Message>, String) {
    match messages.last() {
        Some(last) if last.role == Role::User => {
            let asked = said(last);
            messages.pop();
            (messages, asked)
        }
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

/// The turn, as a stream. The asking runs on a task of its own so the stream
/// is there to be read the moment it is handed over.
fn hold(link: Arc<Link>, asking: Asking, cancel: CancellationToken) -> ModelStream {
    let (out, events) = mpsc::unbounded_channel();
    tokio::spawn(round(link, asking, cancel, Telling::new(out)));
    Box::pin(stream::unfold(events, |mut events| async move {
        events.recv().await.map(|event| (event, events))
    }))
}

/// The turn's own stream, and whether anything has gone down it. One fact,
/// because it is the fact that says whether the question may be put again: a
/// turn that has said nothing can be asked afresh, and one that has said
/// something cannot, because the second telling would be the first one twice.
struct Telling {
    out: mpsc::UnboundedSender<Yielded>,
    said: bool,
}

impl Telling {
    fn new(out: mpsc::UnboundedSender<Yielded>) -> Self {
        Self { out, said: false }
    }

    /// `false` once nobody is reading: the turn was dropped and there is no
    /// reason to keep folding.
    fn tell(&mut self, events: Vec<ModelEvent>) -> bool {
        self.said |= !events.is_empty();
        events
            .into_iter()
            .all(|event| self.out.send(Ok(event)).is_ok())
    }

    fn failed(&self, error: ProviderError) {
        let _ = self.out.send(Err(error));
    }
}

/// How one `session/prompt` ended.
enum Attempt {
    /// Answered, refused, cancelled or dropped: whatever there was to say has
    /// been said, and the turn is over.
    Done,
    /// The child was gone before it said a word. Nothing of this attempt
    /// reached the stream, so putting the question again is asking it rather
    /// than repeating it.
    Lost(AcpError),
}

/// The turn, and — once, and only if the first attempt said nothing at all —
/// the same turn on a new child (ADR-0035 §3).
///
/// The death that ended the first attempt is not what the person is told: the
/// notice that a child went is said where it is buried, and the turn's own
/// answer is whatever the second child makes of the question.
async fn round(link: Arc<Link>, asking: Asking, cancel: CancellationToken, mut out: Telling) {
    let Attempt::Lost(_) = attempt(&link, &asking, &cancel, &mut out).await else {
        return;
    };
    match asking.afresh().await {
        Ok(fresh) => {
            if let Attempt::Lost(gone) = attempt(&fresh, &asking, &cancel, &mut out).await {
                out.failed(gone.into());
            }
        }
        Err(refused) => out.failed(refused),
    }
}

/// One `session/prompt`, held open: updates folded as they arrive, an
/// interrupt sent as `session/cancel`, and the finish written when the prompt
/// answers.
async fn attempt(
    link: &Arc<Link>,
    asking: &Asking,
    cancel: &CancellationToken,
    out: &mut Telling,
) -> Attempt {
    let mut updates = link.listen().await;
    let prompt = asking.prompt(link).await;
    let mut mapper = Mapper::default();
    let waiting = link.connection.call(prompt);
    tokio::pin!(waiting);
    let mut told = false;
    let answered = loop {
        tokio::select! {
            biased;
            Some(note) = updates.recv() => {
                if !out.tell(mapper.update(note.update)) {
                    return Attempt::Done;
                }
            }
            answer = &mut waiting => break answer,
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
        if !out.tell(mapper.update(note.update)) {
            return Attempt::Done;
        }
    }
    settle(answered, &mut mapper, cancel, out)
}

/// What the prompt answered with, and what it leaves the turn. A dead pipe
/// with nothing said and no interrupt to explain it is the one outcome worth a
/// second child; the agent's own refusal, a shape this build cannot read, and
/// a death after the first word are all this turn's answer.
fn settle(
    answered: Result<PromptResponse, AcpError>,
    mapper: &mut Mapper,
    cancel: &CancellationToken,
    out: &mut Telling,
) -> Attempt {
    match answered {
        Ok(response) => {
            out.tell(mapper.finish(&response));
            Attempt::Done
        }
        Err(gone) if transport(&gone) && !out.said && !cancel.is_cancelled() => Attempt::Lost(gone),
        Err(failed) => {
            out.failed(failed.into());
            Attempt::Done
        }
    }
}

/// A failure of the pipe rather than of the agent: the child is gone, or has
/// stopped speaking. Anything the agent itself answered — a refusal in its own
/// words, an answer this build could not read — happened, and asking again
/// would only be asking it twice.
fn transport(error: &AcpError) -> bool {
    matches!(error, AcpError::Transport(_))
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
        let (history, asked) = split(messages.clone());
        assert_eq!(asked, "and the tests?");
        assert_eq!(history.len(), 2);

        let mut only_the_first = messages;
        only_the_first.truncate(1);
        let (history, asked) = split(only_the_first);
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
        let (history, asked) = split(messages);
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

    /// The one failure worth a second child is the pipe's. An agent that
    /// refused in its own words has answered, and asking it again would be
    /// asking it twice (ADR-0035 §3).
    #[test]
    fn only_a_dead_pipe_is_worth_asking_again() {
        use agent_client_protocol_schema::v1::Error as RpcError;
        assert!(transport(&AcpError::transport("the adapter is gone")));
        assert!(!transport(&AcpError::Refused(RpcError::new(
            -32000,
            "run `claude login`"
        ))));
        assert!(!transport(&AcpError::protocol("session/prompt: no stop")));
        assert!(!transport(&AcpError::Spawn("no such command".into())));
    }

    /// What decides whether the question may be put again is whether anything
    /// of the answer is already on the stream — and an update this build has
    /// no meaning for leaves a turn as silent as it was.
    #[test]
    fn a_turn_has_said_nothing_until_an_event_goes_out() {
        let (out, _reading) = mpsc::unbounded_channel();
        let mut telling = Telling::new(out);
        assert!(!telling.said);
        assert!(telling.tell(Vec::new()));
        assert!(!telling.said, "an update that meant nothing said nothing");
        assert!(telling.tell(vec![ModelEvent::TextStart { id: "t1".into() }]));
        assert!(telling.said);
    }

    #[test]
    fn a_request_can_be_told_whose_turn_it_is() {
        let session = SessionId::mint();
        let stamped = for_session(request(Vec::new()), session.clone());
        assert_eq!(stamped.session, Some(session));
    }
}
