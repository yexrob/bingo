//! What one platform is, to this surface (ADR-0016 §1).
//!
//! Capabilities are **accessors that hand over the mechanism**, never
//! booleans: `edit()` returns the thing that edits, or nothing. An adapter
//! that has not implemented editing cannot claim it, so the claim and the
//! renderer cannot drift apart — which is the bug the research found in every
//! adapter set that declares its capabilities as flags.

use async_trait::async_trait;
use bingo_sdk::{CancellationToken, InteractionId};
use tokio::sync::mpsc;

use crate::conversation::{Conversation, Posted};
use crate::error::ChannelError;
use crate::limits::Limits;
use crate::question::Question;

/// What a platform tells this surface.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    Message {
        conversation: Conversation,
        /// The platform's id for whoever spoke. It becomes `Origin.principal`
        /// and is stamped by the adapter, never read out of the text.
        principal: String,
        text: String,
        /// The bot was spoken to: always in a direct chat, and in a group
        /// only on a mention (ADR-0016 §4).
        addressed: bool,
        /// The message a reply would hang under, where the platform threads.
        parent: Option<Posted>,
    },
    Click {
        conversation: Conversation,
        principal: String,
        question: InteractionId,
        /// The `Choice::key` the button carried.
        choice: String,
    },
}

/// One arrival, and which adapter it came through.
#[derive(Clone, Debug)]
pub struct Arrival {
    pub adapter: String,
    pub event: Incoming,
}

/// Where an adapter puts what it hears. One per adapter, so an adapter never
/// has to know its own place in the surface's list.
#[derive(Clone, Debug)]
pub struct Inbox {
    adapter: String,
    arrivals: mpsc::Sender<Arrival>,
}

impl Inbox {
    pub fn new(adapter: impl Into<String>, arrivals: mpsc::Sender<Arrival>) -> Self {
        Self {
            adapter: adapter.into(),
            arrivals,
        }
    }

    /// Hand an event to the surface. A closed inbox means the surface is
    /// stopping, which is the adapter's cue to stop too.
    pub async fn post(&self, event: Incoming) -> Result<(), ChannelError> {
        self.arrivals
            .send(Arrival {
                adapter: self.adapter.clone(),
                event,
            })
            .await
            .map_err(|_| ChannelError::Transport("the channel surface has stopped".into()))
    }
}

/// Whether a message is finished the moment it is posted, or is the one an
/// answer will stream into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Once,
    Stream,
}

#[async_trait]
pub trait ChannelAdapter: Send + Sync {
    /// The first segment of every session key this adapter's chats mint.
    fn id(&self) -> &str;

    fn limits(&self) -> &Limits;

    /// What this adapter runs as, for the lock file. A public identifier —
    /// an app id, a bot name — never the secret behind it (ADR-0016 §5).
    fn credential(&self) -> String;

    /// Pump the platform's events into `inbox` until `cancel` fires. Returns
    /// only when it has stopped for good; a reconnect is its own business.
    async fn run(&self, inbox: Inbox, cancel: CancellationToken) -> Result<(), ChannelError>;

    /// Post prose. Under `Mode::Stream` the message returned is one that
    /// `edit()` may replace, if this adapter has an `edit()` at all.
    async fn send(&self, to: &Conversation, text: &str, mode: Mode)
    -> Result<Posted, ChannelError>;

    fn edit(&self) -> Option<&dyn Edit> {
        None
    }

    fn buttons(&self) -> Option<&dyn Buttons> {
        None
    }

    fn typing(&self) -> Option<&dyn Typing> {
        None
    }

    fn threads(&self) -> Option<&dyn Threads> {
        None
    }
}

/// Replacing what a message says. The text is always the whole of it: a
/// platform that prefers deltas diffs for itself, and Feishu's prefix-diff
/// typing punishes anything else (ADR-0016 §2).
#[async_trait]
pub trait Edit: Send + Sync {
    async fn replace(&self, at: &Posted, text: &str) -> Result<(), ChannelError>;

    /// The last text, and no more after it. A platform that holds a stream
    /// open closes it here.
    async fn finish(&self, at: &Posted, text: &str) -> Result<(), ChannelError>;
}

/// Native buttons under a question, and taking them off again.
#[async_trait]
pub trait Buttons: Send + Sync {
    async fn ask(&self, to: &Conversation, question: &Question) -> Result<Posted, ChannelError>;

    /// Buttons stripped, outcome appended (ADR-0016 §3).
    async fn settle(
        &self,
        at: &Posted,
        question: &Question,
        outcome: &str,
    ) -> Result<(), ChannelError>;
}

/// The "…is typing" affordance, for platforms that have one.
#[async_trait]
pub trait Typing: Send + Sync {
    async fn poke(&self, to: &Conversation) -> Result<(), ChannelError>;
}

/// Hanging a message under another, so the platform keeps the thread.
#[async_trait]
pub trait Threads: Send + Sync {
    async fn reply(
        &self,
        to: &Conversation,
        parent: &Posted,
        text: &str,
        mode: Mode,
    ) -> Result<Posted, ChannelError>;
}
