//! A subscription forwarded to notifications: one task per open session, one
//! for the gateway. Dropping the forwarder stops the task, so `session/close`,
//! a reopen and a resync are all "replace the value in the map".

use bingo_sdk::SessionHandle;
use futures::{Stream, StreamExt};
use serde::Serialize;
use tokio::sync::mpsc::Sender;
use tokio::task::JoinHandle;

use crate::codec::{Message, Notification};

/// A task draining one stream into one notification method.
#[derive(Debug)]
pub(crate) struct Pump(JoinHandle<()>);

impl Pump {
    pub(crate) fn spawn<S, T>(method: &'static str, stream: S, out: Sender<Message>) -> Pump
    where
        S: Stream<Item = T> + Unpin + Send + 'static,
        T: Serialize + Send + 'static,
    {
        Pump(tokio::spawn(drain(method, stream, out)))
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn drain<S, T>(method: &'static str, mut stream: S, out: Sender<Message>)
where
    S: Stream<Item = T> + Unpin + Send + 'static,
    T: Serialize + Send + 'static,
{
    while let Some(item) = stream.next().await {
        let params = match serde_json::to_value(&item) {
            Ok(params) => params,
            // Undeliverable, and the client's fold tolerates a gap in seq.
            Err(error) => {
                tracing::error!(%error, method, "a notification would not serialise");
                continue;
            }
        };
        if out
            .send(Message::Notification(Notification::new(method, params)))
            .await
            .is_err()
        {
            break;
        }
    }
}

/// One open session: where its frames go, and how a write reaches its actor.
#[derive(Debug)]
pub(crate) struct Forwarder {
    pub(crate) handle: SessionHandle,
    _events: Pump,
}

impl Forwarder {
    pub(crate) fn new(handle: SessionHandle, events: Pump) -> Self {
        Self {
            handle,
            _events: events,
        }
    }
}
