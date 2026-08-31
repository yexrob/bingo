//! The surface: N conversations wide, one session each.
//!
//! `SurfaceKind::Concurrent` — it owns no terminal and runs beside whatever
//! does. Its whole job is routing: an arrival names a conversation, a
//! conversation names a session key, and a key names the [`Runner`] that owns
//! that conversation from then on.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    CancellationToken, ErrorCode, Exit, HostHandle, KernelError, Surface, SurfaceKind,
    SurfaceOptions,
};
use tokio::sync::mpsc;

use crate::adapter::{Arrival, ChannelAdapter, Inbox, Incoming};
use crate::conversation::Conversation;
use crate::gate::Gate;
use crate::lock::Claim;
use crate::runner::{Runner, SURFACE_ID};

/// Arrivals held while the runners are busy. Past this the adapters wait,
/// which is the same backpressure a chat app applies to itself.
const ARRIVALS: usize = 64;

/// What one conversation's runner is reached by.
type Chat = mpsc::Sender<Incoming>;

pub struct ChannelsSurface {
    adapters: Vec<Arc<dyn ChannelAdapter>>,
    gate: Gate,
}

impl std::fmt::Debug for ChannelsSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelsSurface")
            .field(
                "adapters",
                &self.adapters.iter().map(|a| a.id()).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl ChannelsSurface {
    pub fn new(adapters: Vec<Arc<dyn ChannelAdapter>>, gate: Gate) -> Self {
        Self { adapters, gate }
    }

    /// One claim per credential, held for the run (ADR-0016 §5).
    fn claim(&self, data_dir: &std::path::Path) -> Result<Vec<Claim>, KernelError> {
        self.adapters
            .iter()
            .map(|adapter| {
                Claim::take(data_dir, adapter.id(), &adapter.credential()).map_err(Into::into)
            })
            .collect()
    }

    fn adapter(&self, id: &str) -> Option<&Arc<dyn ChannelAdapter>> {
        self.adapters.iter().find(|adapter| adapter.id() == id)
    }

    /// Arrivals to runners, one runner per session key, for as long as any
    /// adapter is still talking.
    async fn route(&self, host: &HostHandle, cwd: PathBuf, mut arrivals: mpsc::Receiver<Arrival>) {
        let mut chats: BTreeMap<String, Chat> = BTreeMap::new();
        while let Some(arrival) = arrivals.recv().await {
            let Some(adapter) = self.adapter(&arrival.adapter).cloned() else {
                continue;
            };
            let Some(conversation) = engaged(&arrival.event) else {
                continue;
            };
            let key = format!("{}/{}", adapter.id(), conversation.path());
            if !chats.contains_key(&key) {
                match self
                    .start(host, adapter, conversation.clone(), cwd.clone())
                    .await
                {
                    Ok((key, chat)) => {
                        chats.insert(key, chat);
                    }
                    Err(error) => {
                        tracing::warn!(%error, %key, "the chat could not open a session");
                        continue;
                    }
                }
            }
            // A runner that has stopped leaves its conversation free to be
            // opened again by whatever is said next.
            if let Some(chat) = chats.get(&key)
                && chat.send(arrival.event).await.is_err()
            {
                chats.remove(&key);
            }
        }
    }

    async fn start(
        &self,
        host: &HostHandle,
        adapter: Arc<dyn ChannelAdapter>,
        conversation: Conversation,
        cwd: PathBuf,
    ) -> Result<(String, Chat), KernelError> {
        let (chat, inbound) = mpsc::channel(ARRIVALS);
        let runner = Runner::open(host, adapter, conversation, cwd, self.gate, inbound).await?;
        let key = runner.key().to_string();
        tokio::spawn(runner.run());
        Ok((key, chat))
    }
}

#[async_trait]
impl Surface for ChannelsSurface {
    fn id(&self) -> &str {
        SURFACE_ID
    }

    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Concurrent
    }

    async fn run(&self, host: HostHandle, opts: SurfaceOptions) -> Result<Exit, KernelError> {
        if self.adapters.is_empty() {
            return Err(KernelError::new(
                ErrorCode::InvalidInput,
                "no channel is configured: name one under `channels` in the settings, \
                 or pass --channels",
            ));
        }
        let _claims = self.claim(&opts.env.data_dir)?;
        let (post, arrivals) = mpsc::channel(ARRIVALS);
        let cancel = CancellationToken::new();
        let pumps = self.adapters.iter().map(|adapter| {
            adapter.run(Inbox::new(adapter.id(), post.clone()), cancel.child_token())
        });
        let pumping = futures::future::try_join_all(pumps);
        drop(post);
        let routing = self.route(&host, opts.cwd, arrivals);
        let outcome = tokio::select! {
            outcome = pumping => outcome.map(drop),
            // Every adapter hung up: there is nothing left to listen to.
            () = routing => Ok(()),
        };
        cancel.cancel();
        outcome?;
        Ok(Exit { code: 0 })
    }
}

/// Whether an arrival is this surface's business at all. A group engages only
/// when the bot is spoken to (ADR-0016 §4), and the adapter — which knows its
/// own id and its own mention syntax — is what decided that.
fn engaged(event: &Incoming) -> Option<&Conversation> {
    match event {
        Incoming::Message {
            conversation,
            addressed,
            ..
        } => addressed.then_some(conversation),
        Incoming::Click { conversation, .. } => Some(conversation),
    }
}

#[cfg(test)]
pub(crate) mod tests;
