//! One conversation, on one session, for as long as the surface runs.
//!
//! It is a client like every other: it folds frames with `SessionState::apply`
//! and derives what to say from the fold (ADR-0002). What it adds is the other
//! direction — a reply that answers an open question resolves it, and anything
//! else is a prompt.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use bingo_sdk::{
    Activation, Answer, Applied, Attachment, ClientIdentity, ErrorCode, Event, Frame, FrameStream,
    HostHandle, Input, IntentId, InteractionId, KernelError, OpenOptions, Origin, SessionHandle,
    SessionId, SessionSelector, SessionSpec, SessionState,
};
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::adapter::{ChannelAdapter, Incoming, Mode};
use crate::conversation::{Conversation, Posted};
use crate::deliver::{Deliverer, Op};
use crate::error::ChannelError;
use crate::gate::Gate;
use crate::question::Question;

/// The surface id, and the `Origin.surface` of everything a chat submits.
pub const SURFACE_ID: &str = "channels";

/// A question this conversation is showing, and how it was drawn — which is
/// what decides how the buttons come off again.
struct Asked {
    question: Question,
    at: Posted,
    native: bool,
}

pub struct Runner {
    adapter: Arc<dyn ChannelAdapter>,
    conversation: Conversation,
    key: String,
    root: SessionId,
    /// One reducer per session in the tree, the root's from the snapshot.
    states: BTreeMap<SessionId, SessionState>,
    events: FrameStream,
    handle: SessionHandle,
    deliverer: Deliverer,
    /// The message the answer is streaming into, while there is one.
    streaming: Option<Posted>,
    /// The message a reply would hang under, from whoever spoke last.
    parent: Option<Posted>,
    asked: BTreeMap<InteractionId, Asked>,
    inbound: mpsc::Receiver<Incoming>,
}

impl Runner {
    /// Open or continue this conversation's session, keyed `<adapter>/<chat>
    /// [/<thread>]` — a key only this plugin mints (ADR-0016 §4).
    pub async fn open(
        host: &HostHandle,
        adapter: Arc<dyn ChannelAdapter>,
        conversation: Conversation,
        cwd: std::path::PathBuf,
        gate: Gate,
        inbound: mpsc::Receiver<Incoming>,
    ) -> Result<Self, KernelError> {
        let key = format!("{}/{}", adapter.id(), conversation.path());
        let attachment = attach(host, &key, cwd).await?;
        let Attachment {
            session,
            snapshot,
            events,
            handle,
        } = attachment;
        let deliverer = Deliverer::new(adapter.limits().clone(), gate, key.clone());
        Ok(Self {
            adapter,
            conversation,
            root: session.clone(),
            states: BTreeMap::from([(session, snapshot)]),
            events,
            handle,
            deliverer,
            streaming: None,
            parent: None,
            asked: BTreeMap::new(),
            key,
            inbound,
        })
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// Frames out, replies in, until the session's stream ends or the chat
    /// goes away.
    pub async fn run(mut self) {
        loop {
            let due = self.deliverer.due();
            tokio::select! {
                frame = self.events.next() => match frame {
                    Some(frame) => self.saw(&frame).await,
                    None => return,
                },
                _ = tick(due) => {
                    let ops = self.deliverer.tick(Instant::now());
                    self.perform(ops).await;
                }
                event = self.inbound.recv() => match event {
                    Some(event) => self.heard(event),
                    None => return,
                },
            }
        }
    }

    async fn saw(&mut self, frame: &Frame) {
        // A lag marker ends the live stream at the gap; the reducer left
        // `seq` at the last frame applied, so replaying from there fills it.
        if matches!(frame.event, Event::Lagged { .. }) {
            self.resync().await;
            return;
        }
        let Some(state) = self.state_of(frame) else {
            return;
        };
        if state.apply(frame) == Applied::Stale {
            return;
        }
        if !self.concerns_this_chat(frame) {
            return;
        }
        let state = self.states[&frame.session].clone();
        let ops = self.deliverer.apply(frame, &state, Instant::now());
        self.perform(ops).await;
    }

    /// A sub-session's prose is its parent's to report; its questions are
    /// nobody else's, so those reach the chat (ADR-0010 §3).
    fn concerns_this_chat(&self, frame: &Frame) -> bool {
        frame.session == self.root
            || matches!(
                frame.event,
                Event::InteractionOpened { .. }
                    | Event::InteractionResolved { .. }
                    | Event::InteractionCancelled { .. }
            )
    }

    fn state_of(&mut self, frame: &Frame) -> Option<&mut SessionState> {
        if !self.states.contains_key(&frame.session) {
            let Event::SessionUpdated { summary } = &frame.event else {
                return None;
            };
            self.states
                .insert(frame.session.clone(), SessionState::new(summary.clone()));
        }
        self.states.get_mut(&frame.session)
    }

    async fn resync(&mut self) {
        let since = self.states[&self.root].seq;
        match self.handle.events_since(since).await {
            Ok(events) => self.events = events,
            Err(error) => {
                tracing::warn!(%error, key = %self.key, "the journal could not be re-read")
            }
        }
    }

    async fn perform(&mut self, ops: Vec<Op>) {
        for op in ops {
            if let Err(error) = self.deliver(op).await {
                // A platform that refuses one message has not ended the
                // conversation; the next frame is still worth delivering.
                tracing::warn!(%error, key = %self.key, "the chat refused a message");
            }
        }
    }

    async fn deliver(&mut self, op: Op) -> Result<(), ChannelError> {
        match op {
            Op::Open => self.begin().await,
            Op::Replace { full } => self.replace(&full).await,
            Op::Finalize { text, question } => self.finalize(&text, question).await,
            Op::Status { text } => self.post(&text).await.map(drop),
            Op::Resolved { question, outcome } => self.settle(&question, &outcome).await,
        }
    }

    /// A message the answer streams into — only where there is something to
    /// stream with. Without an `edit()` the answer arrives whole and late, so
    /// the typing affordance is what a person gets meanwhile; a platform that
    /// streams needs none, because the message writing itself is the sign.
    async fn begin(&mut self) -> Result<(), ChannelError> {
        if self.adapter.edit().is_none() {
            return match self.adapter.typing() {
                Some(typing) => typing.poke(&self.conversation).await,
                None => Ok(()),
            };
        }
        self.streaming = Some(self.post_mode("", Mode::Stream).await?);
        Ok(())
    }

    async fn replace(&mut self, full: &str) -> Result<(), ChannelError> {
        match (self.adapter.edit(), &self.streaming) {
            (Some(edit), Some(at)) => edit.replace(at, full).await,
            _ => Ok(()),
        }
    }

    /// The answer, and then the question that stopped it.
    ///
    /// The two are separate deliveries and the second never depends on the
    /// first. A platform that refused the answer has not refused the question,
    /// and a question that never arrives is a turn that never ends: the
    /// session waits on an interaction nobody was ever shown, and every later
    /// message queues behind it. That is the whole "it worked for a few
    /// messages and then stuck" failure, so both are attempted and the first
    /// error is what is reported.
    async fn finalize(
        &mut self,
        text: &str,
        question: Option<Question>,
    ) -> Result<(), ChannelError> {
        let said = self.say(text).await;
        let asked = match question {
            Some(question) => self.ask(question).await,
            None => Ok(()),
        };
        said.and(asked)
    }

    /// The answer itself: finished into the message it was streaming into,
    /// else posted whole.
    ///
    /// A stream that will not close is not an answer that is gone. The card
    /// may have been auto-closed by the platform under a long turn, or the
    /// sequence refused; either way the text still exists and goes out as its
    /// own message rather than disappearing with the card.
    async fn say(&mut self, text: &str) -> Result<(), ChannelError> {
        let finished = match (self.adapter.edit(), self.streaming.take()) {
            (Some(edit), Some(at)) => Some(edit.finish(&at, text).await),
            _ => None,
        };
        match finished {
            Some(Ok(())) | None if text.is_empty() => Ok(()),
            Some(Ok(())) => Ok(()),
            Some(Err(error)) if !text.is_empty() => {
                tracing::warn!(
                    %error,
                    key = %self.key,
                    "the streamed message would not close; posting the answer whole"
                );
                self.post(text).await.map(drop)
            }
            Some(Err(error)) => Err(error),
            None => self.post(text).await.map(drop),
        }
    }

    /// A question is its own message, never buttons on a live stream: a
    /// platform that is mid-stream will not deliver a callback (ADR-0016 §6).
    async fn ask(&mut self, question: Question) -> Result<(), ChannelError> {
        let native = self
            .adapter
            .buttons()
            .filter(|_| question.buttons(self.adapter.limits()).is_some());
        // Buttons are an affordance, not the question. A platform that refused
        // the card can still be asked in words, and losing the question here
        // would leave the session waiting for an answer nobody was shown.
        let posted = match native {
            Some(buttons) => match buttons.ask(&self.conversation, &question).await {
                Ok(at) => Ok((at, true)),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        key = %self.key,
                        "the question's buttons were refused; asking in words"
                    );
                    self.post(&question.numbered()).await.map(|at| (at, false))
                }
            },
            None => self.post(&question.numbered()).await.map(|at| (at, false)),
        };
        let (at, native) = posted?;
        self.asked.insert(
            question.id.clone(),
            Asked {
                question,
                at,
                native,
            },
        );
        Ok(())
    }

    /// The buttons come off however they went on, and the outcome goes where
    /// they were. A question drawn as a numbered list has no live button to
    /// strip, so if the platform will not edit that message the outcome is
    /// said in a new one rather than lost.
    async fn settle(&mut self, id: &InteractionId, outcome: &str) -> Result<(), ChannelError> {
        let Some(asked) = self.asked.remove(id) else {
            return Ok(());
        };
        if asked.native
            && let Some(buttons) = self.adapter.buttons()
        {
            return buttons.settle(&asked.at, &asked.question, outcome).await;
        }
        let settled = format!("{}\n\n{outcome}", asked.question.prompt);
        let edited = match self.adapter.edit() {
            Some(edit) => edit.replace(&asked.at, &settled).await,
            None => Err(ChannelError::Unsupported("editing")),
        };
        match edited {
            Ok(()) => Ok(()),
            Err(_) => self.post(&settled).await.map(drop),
        }
    }

    fn post(
        &self,
        text: &str,
    ) -> impl std::future::Future<Output = Result<Posted, ChannelError>> + Send + use<> {
        self.post_mode(text, Mode::Once)
    }

    fn post_mode(
        &self,
        text: &str,
        mode: Mode,
    ) -> impl std::future::Future<Output = Result<Posted, ChannelError>> + Send + use<> {
        post(
            Arc::clone(&self.adapter),
            self.conversation.clone(),
            self.parent.clone(),
            text.to_string(),
            mode,
        )
    }

    fn heard(&mut self, event: Incoming) {
        match event {
            Incoming::Message {
                principal,
                text,
                parent,
                ..
            } => {
                self.parent = parent;
                self.said(&principal, text);
            }
            Incoming::Click {
                question, choice, ..
            } => self.clicked(&question, &choice),
        }
    }

    /// A reply that answers the open question is that answer; anything else
    /// is the next thing to work on.
    fn said(&mut self, principal: &str, text: String) {
        match self.answering(&text) {
            Some((id, answer)) => self.answer(id, answer),
            None => self.handle.submit(
                IntentId::mint(),
                Input::text(
                    text,
                    Origin {
                        surface: SURFACE_ID.into(),
                        principal: Some(principal.to_string()),
                        conversation: Some(self.key.clone()),
                    },
                ),
            ),
        }
    }

    fn answering(&self, text: &str) -> Option<(InteractionId, Answer)> {
        self.asked
            .values()
            .find_map(|asked| Some((asked.question.id.clone(), asked.question.parse(text)?)))
    }

    fn clicked(&mut self, id: &InteractionId, choice: &str) {
        let Some(answer) = self.asked.get(id).and_then(|a| a.question.pick(choice)) else {
            return;
        };
        self.answer(id.clone(), answer);
    }

    /// Always `Pointer`, on both rungs. The kernel reads the activation for
    /// one thing: whether a keystroke could have landed on a prompt that had
    /// just appeared under it, which it guards against for 400 ms. Neither a
    /// button press nor a message composed and sent in a chat can be that
    /// accident, and an answer silently refused as `NOT_READY` would look to
    /// a person like a chat that had stopped listening.
    fn answer(&self, id: InteractionId, answer: Answer) {
        self.handle
            .answer(IntentId::mint(), id, answer, Activation::Pointer);
    }
}

/// Post to the chat: under the message that started this where the platform
/// threads, and as a message of its own where it does not.
async fn post(
    adapter: Arc<dyn ChannelAdapter>,
    to: Conversation,
    parent: Option<Posted>,
    text: String,
    mode: Mode,
) -> Result<Posted, ChannelError> {
    match (adapter.threads(), &parent) {
        (Some(threads), Some(parent)) => threads.reply(&to, parent, &text, mode).await,
        _ => adapter.send(&to, &text, mode).await,
    }
}

/// Sleep until the coalescer's timer, or never when nothing is held.
async fn tick(due: Option<Instant>) {
    match due {
        Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
        None => std::future::pending().await,
    }
}

/// Continue the session this chat already has, or start one under its key.
async fn attach(
    host: &HostHandle,
    key: &str,
    cwd: std::path::PathBuf,
) -> Result<Attachment, KernelError> {
    let who = ClientIdentity {
        name: key.to_string(),
        surface: SURFACE_ID.to_string(),
    };
    // The whole tree: a sub-agent's permission prompt reaches a person only
    // through the attachment that can see it (ADR-0010 §3).
    let options = OpenOptions::with_children();
    match host
        .open(
            SessionSelector::ByKey {
                key: key.to_string(),
            },
            who.clone(),
            options,
        )
        .await
    {
        Err(error) if error.code == ErrorCode::SessionNotFound => {
            host.open(
                SessionSelector::Create {
                    spec: SessionSpec {
                        cwd,
                        key: Some(key.to_string()),
                        ..SessionSpec::default()
                    },
                },
                who,
                options,
            )
            .await
        }
        other => other,
    }
}
