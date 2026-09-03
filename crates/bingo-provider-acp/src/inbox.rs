//! The client half of one conversation: where a turn's stream goes, and what
//! the agent is told when it asks this client something.
//!
//! An ACP agent brings its own permission machinery and the row that spawned it
//! says what it may do, so a well-configured adapter never knocks here
//! (ADR-0039 §4). One that knocks anyway is not refused on principle any more:
//! its question is put to whoever is at the session, in the agent's own words,
//! and the answer is one of the agent's own option ids (ADR-0039 §3). The
//! refusal — and the one line that says so — is what is left when there was
//! nobody to put it to.

use std::sync::atomic::{AtomicBool, Ordering};

use agent_client_protocol_schema::v1::{
    CreateElicitationRequest, CreateElicitationResponse, Error as RpcError,
    RequestPermissionRequest, RequestPermissionResponse, SessionNotification,
};
use async_trait::async_trait;
use bingo_sdk::{HostHandle, SessionId};
use tokio::sync::{Mutex, mpsc};

use crate::connection::Client;
use crate::{question, refusal};

/// Where a running turn's stream goes.
type Sink = mpsc::UnboundedSender<SessionNotification>;

pub struct Inbox {
    adapter: String,
    sink: Mutex<Option<Sink>>,
    /// True while `session/load` replays. The journal already holds those
    /// turns; writing them again would be the conversation twice.
    loading: AtomicBool,
    /// Whether this adapter has already been told, once, that it asked where
    /// nobody could answer.
    told: AtomicBool,
    host: Option<HostHandle>,
    /// Whose conversation this is. A cold ask has none (`crate::probe`): it is
    /// nobody's session, so a question put during one reaches nobody and falls
    /// closed like any other unanswerable one.
    session: Option<SessionId>,
}

impl Inbox {
    pub fn new(adapter: &str, host: Option<HostHandle>, session: Option<&SessionId>) -> Self {
        Inbox {
            adapter: adapter.to_string(),
            sink: Mutex::new(None),
            loading: AtomicBool::new(false),
            told: AtomicBool::new(false),
            host,
            session: session.cloned(),
        }
    }

    pub fn adapter(&self) -> &str {
        &self.adapter
    }

    /// Take the stream of the turn that is starting. The previous turn's sink,
    /// if any, is dropped with it.
    pub async fn listen(&self) -> mpsc::UnboundedReceiver<SessionNotification> {
        let (sink, updates) = mpsc::unbounded_channel();
        *self.sink.lock().await = Some(sink);
        updates
    }

    /// Whether what arrives now is a replay of turns the journal already holds.
    pub fn replaying(&self, loading: bool) {
        self.loading.store(loading, Ordering::Release);
    }

    /// The question, put to whoever is at this session. `None` when nothing
    /// chose an option: no session behind this conversation, a door that
    /// refused it, or a surface that declined what it was handed.
    async fn put(&self, request: &RequestPermissionRequest) -> Option<RequestPermissionResponse> {
        let (host, session) = (self.host.as_ref()?, self.session.as_ref()?);
        let answer = host
            .ask(
                session,
                question::asked(&self.adapter, request),
                question::answers(),
            )
            .await
            .ok()?;
        question::picked(request, &answer)
    }

    /// The one thing this plugin must not do silently: an agent asked, and the
    /// answer was made by a rule the person never sees unless it is said. Said
    /// once — an agent may ask on every call it makes.
    async fn say_where_the_answer_lives(&self) {
        if self.told.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some(host) = self.host.as_ref() else {
            return;
        };
        let (level, code, text) = refusal::told(&self.adapter);
        let _ = host.notice(level, &code, &text).await;
    }
}

#[async_trait]
impl Client for Inbox {
    async fn update(&self, notification: SessionNotification) {
        if self.loading.load(Ordering::Acquire) {
            return;
        }
        if let Some(sink) = self.sink.lock().await.as_ref() {
            let _ = sink.send(notification);
        }
    }

    /// One `session/request_permission` is one question (ADR-0039 §3). The
    /// agent always gets an answer: the option that was chosen, or — when none
    /// was — its own refusal, and one line to the person saying so.
    async fn permission(
        &self,
        request: RequestPermissionRequest,
    ) -> Result<RequestPermissionResponse, RpcError> {
        if let Some(answered) = self.put(&request).await {
            return Ok(answered);
        }
        self.say_where_the_answer_lives().await;
        Ok(refusal::refused(&request))
    }

    async fn elicitation(
        &self,
        _request: CreateElicitationRequest,
    ) -> Result<CreateElicitationResponse, RpcError> {
        self.say_where_the_answer_lives().await;
        Ok(refusal::declined())
    }
}
