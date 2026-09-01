//! One plugin provider as a bingo provider.
//!
//! The kernel keeps seeing `Arc<dyn Provider>` and never learns which of them
//! are processes: this struct implements the sdk's own trait and its `stream`
//! is a wire call (ADR-0030 §1). N remote providers are N of these, differing
//! by the handshake data they were built from.
//!
//! A stream is the one crossing that is not a call and a reply. The open goes
//! out as `provider/stream`, the events come back as `provider/delta`
//! notifications through a bounded queue, and the reply to the open is the
//! close. Whoever stops reading — the turn was interrupted, the stream was
//! dropped — sends `provider/cancel`; a process that ignores it, or goes quiet
//! with nothing else to say, hits the idle deadline and the stream yields the
//! timeout the kernel already retries on.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    CancellationToken, EndpointCapabilities, ModelEvent, ModelInfo, ModelRequest, ModelStream,
    Provider, ProviderError,
};
use serde_json::Value;
use tokio::sync::mpsc::Receiver;
use tokio::task::{JoinError, JoinHandle};

use crate::codec::TRANSPORT_ERROR;
use crate::connection::{Connection, Reply, StreamWatch};
use crate::deadline;
use crate::wire::{
    ProviderCancelParams, ProviderSpec, ProviderStreamParams, ProviderStreamResult, name,
};

/// How many events may wait for the kernel before the process is made to wait.
/// The bound is the whole of the backpressure: a plugin that outruns the turn
/// reading it blocks on its own pipe rather than filling this process's memory.
const BUFFER: usize = 64;

/// A provider a plugin process declared, bound to the pipe that serves it.
pub struct RemoteProvider {
    /// The plugin's name, which is what an error is reported under; the
    /// provider's own id is the one a person types.
    plugin: String,
    spec: ProviderSpec,
    connection: Arc<Connection>,
}

impl RemoteProvider {
    pub fn new(plugin: &str, spec: ProviderSpec, connection: Arc<Connection>) -> Self {
        Self {
            plugin: plugin.to_string(),
            spec,
            connection,
        }
    }

    fn params(&self, call: &str, request: ModelRequest) -> ProviderStreamParams {
        ProviderStreamParams {
            id: self.spec.id.clone(),
            call: call.to_string(),
            request,
        }
    }
}

#[async_trait]
impl Provider for RemoteProvider {
    fn id(&self) -> &str {
        &self.spec.id
    }

    /// The shelf the declaration named, and the id itself when it named none —
    /// the sdk trait's own default, said where the declaration is.
    fn family(&self) -> &str {
        self.spec.family.as_deref().unwrap_or_else(|| self.id())
    }

    /// Fail closed (ADR-0015 §4): what the declaration says this endpoint does,
    /// for a model the declaration names, and nothing at all for one it does
    /// not. A process cannot earn a capability by being asked about it.
    fn endpoint(&self, model: &str) -> EndpointCapabilities {
        if self.spec.models.iter().any(|known| known.id == model) {
            self.spec.endpoint
        } else {
            EndpointCapabilities::default()
        }
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ModelStream, ProviderError> {
        let call = self.connection.next_call();
        let params = serde_json::to_value(self.params(&call, request)).map_err(|error| {
            ProviderError::Request {
                message: format!("{}: {error}", self.plugin),
            }
        })?;
        Ok(open(
            Opening {
                plugin: self.plugin.clone(),
                connection: Arc::clone(&self.connection),
                call,
                params,
            },
            cancel,
        ))
    }

    /// The models the handshake declared. Asking costs no call: a plugin says
    /// what it serves once, when it says what it is.
    async fn models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(self.spec.models.clone())
    }
}

/// What one stream is opened from.
struct Opening {
    plugin: String,
    connection: Arc<Connection>,
    call: String,
    params: Value,
}

/// Route the deltas, send the open, and fold the two into one stream.
///
/// The route is in place before the request is written: a process that answers
/// at once must not have its first event arrive where nothing is listening.
fn open(opening: Opening, cancel: CancellationToken) -> ModelStream {
    let Opening {
        plugin,
        connection,
        call,
        params,
    } = opening;
    let (sender, events) = tokio::sync::mpsc::channel(BUFFER);
    let watch = connection.watch_stream(&call, sender);
    let closing = tokio::spawn({
        let connection = Arc::clone(&connection);
        async move { connection.request(name::PROVIDER_STREAM, params).await }
    });
    let body = Body {
        plugin,
        events,
        closing,
        end: None,
        done: false,
        cancel,
        guard: Guard {
            connection,
            call,
            told: false,
            _watch: watch,
        },
    };
    Box::pin(futures::stream::unfold(body, |mut body| async move {
        body.next().await.map(|item| (item, body))
    }))
}

/// What the next wait ended with.
enum Waited {
    Event(ModelEvent),
    /// The reply to the open arrived: the stream is over, one way or another.
    Closed(Result<Reply, JoinError>),
    Cancelled,
    Idle,
}

/// One stream being read.
struct Body {
    plugin: String,
    events: Receiver<ModelEvent>,
    closing: JoinHandle<Reply>,
    /// The close, held until everything the process already sent is out.
    end: Option<Result<(), ProviderError>>,
    done: bool,
    cancel: CancellationToken,
    guard: Guard,
}

impl Body {
    async fn next(&mut self) -> Option<Result<ModelEvent, ProviderError>> {
        loop {
            if self.done {
                return None;
            }
            // What the process already sent comes out first, so a close that
            // overtakes a delta in the queue never swallows it.
            if let Ok(event) = self.events.try_recv() {
                return Some(Ok(event));
            }
            if let Some(end) = self.end.take() {
                return self.finish(end);
            }
            match self.wait().await {
                Waited::Event(event) => return Some(Ok(event)),
                // Back to the top: the queue is drained before the close is.
                Waited::Closed(reply) => self.end = Some(self.end_of(reply)),
                Waited::Cancelled => return self.stop(None).await,
                Waited::Idle => return self.stop(Some(ProviderError::Timeout)).await,
            }
        }
    }

    /// The open is answered at most once, so its handle is never polled after
    /// it has completed: the arm above leaves through `end`.
    async fn wait(&mut self) -> Waited {
        tokio::select! {
            biased;
            Some(event) = self.events.recv() => Waited::Event(event),
            reply = &mut self.closing => Waited::Closed(reply),
            () = self.cancel.cancelled() => Waited::Cancelled,
            () = tokio::time::sleep(deadline::PROVIDER_IDLE) => Waited::Idle,
        }
    }

    /// The process closed the stream itself: nothing is left to cancel, and
    /// the error it named — if it named one — is the stream's last item.
    fn finish(
        &mut self,
        end: Result<(), ProviderError>,
    ) -> Option<Result<ModelEvent, ProviderError>> {
        self.done = true;
        self.guard.settled();
        end.err().map(Err)
    }

    /// Nobody is reading any more: tell the process, and end.
    async fn stop(
        &mut self,
        why: Option<ProviderError>,
    ) -> Option<Result<ModelEvent, ProviderError>> {
        self.done = true;
        self.guard.tell().await;
        why.map(Err)
    }

    /// What the close means. A process that answered says so in the error the
    /// trait speaks; a pipe that died says it the only way it can, in the kind
    /// a stream that dropped mid-response always speaks — which is retryable,
    /// so the kernel's ladder is untouched.
    fn end_of(&self, closed: Result<Reply, JoinError>) -> Result<(), ProviderError> {
        match closed {
            Ok(Ok(value)) => self.closed(value),
            Ok(Err(error)) if error.code == TRANSPORT_ERROR => Err(ProviderError::Transport {
                message: format!("{}: {}", self.plugin, error.message),
            }),
            Ok(Err(error)) => Err(self.broke(error.message)),
            Err(error) => Err(self.broke(error.to_string())),
        }
    }

    fn closed(&self, value: Value) -> Result<(), ProviderError> {
        match serde_json::from_value::<ProviderStreamResult>(value) {
            Ok(result) => result.error.map_or(Ok(()), Err),
            Err(error) => Err(self.broke(error.to_string())),
        }
    }

    fn broke(&self, why: String) -> ProviderError {
        ProviderError::Stream {
            message: format!("{}: {why}", self.plugin),
        }
    }
}

impl Drop for Body {
    fn drop(&mut self) {
        // Nobody will read the close now, and the request would outlive the
        // stream it belongs to.
        self.closing.abort();
    }
}

/// A stream's end, however it comes. The route goes with it, and a process
/// that was never told the stream is over is told now: letting go of a stream
/// is what `provider/cancel` says.
struct Guard {
    connection: Arc<Connection>,
    call: String,
    told: bool,
    /// Held for its `Drop`: the delta route lives exactly as long as this.
    _watch: StreamWatch,
}

impl Guard {
    /// The process ended the stream itself; there is nothing to stop.
    fn settled(&mut self) {
        self.told = true;
    }

    async fn tell(&mut self) {
        if self.told {
            return;
        }
        self.told = true;
        cancel(&self.connection, &self.call).await;
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.told {
            return;
        }
        // A drop cannot wait; a cancel nobody sends is a process streaming
        // into a queue nobody reads.
        let (connection, call) = (Arc::clone(&self.connection), self.call.clone());
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move { cancel(&connection, &call).await });
        }
    }
}

async fn cancel(connection: &Connection, call: &str) {
    let params = ProviderCancelParams {
        call: call.to_string(),
    };
    match serde_json::to_value(params) {
        Ok(value) => connection.notify(name::PROVIDER_CANCEL, value).await,
        Err(error) => tracing::debug!(%error, "a cancel that would not serialise"),
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::*;
    use crate::testing::unanswering;

    fn declared() -> ProviderSpec {
        ProviderSpec {
            id: "house".into(),
            family: Some("anthropic".into()),
            models: vec![ModelInfo {
                id: "house-1".into(),
                display: None,
            }],
            endpoint: EndpointCapabilities {
                images: true,
                count_tokens: false,
                caching: true,
            },
        }
    }

    fn remote(spec: ProviderSpec) -> RemoteProvider {
        RemoteProvider::new("stub", spec, unanswering())
    }

    #[tokio::test]
    async fn a_provider_is_what_the_handshake_declared_and_no_more() {
        let provider = remote(declared());
        assert_eq!(provider.id(), "house");
        assert_eq!(provider.family(), "anthropic", "the shelf it named");
        assert_eq!(
            provider.models().await.expect("the declared models")[0].id,
            "house-1",
            "asking for the models costs no call"
        );
    }

    #[tokio::test]
    async fn a_provider_that_named_no_family_is_filed_under_its_own_id() {
        let provider = remote(ProviderSpec {
            family: None,
            ..declared()
        });
        assert_eq!(provider.family(), provider.id());
    }

    /// ADR-0015 §4, on the one capability a provider is asked about: a model
    /// the declaration does not name gets nothing, whatever the endpoint can do
    /// for the models it does name.
    #[tokio::test]
    async fn a_model_the_declaration_never_named_can_do_nothing() {
        let provider = remote(declared());
        let known = provider.endpoint("house-1");
        assert!(known.images && known.caching && !known.count_tokens);
        assert_eq!(
            provider.endpoint("house-2"),
            EndpointCapabilities::default(),
            "an undeclared model is false all round"
        );
    }

    fn request() -> ModelRequest {
        ModelRequest {
            model: "house-1".into(),
            max_tokens: 100,
            system: Vec::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            reasoning: None,
            provider_options: Default::default(),
        }
    }

    /// The floor under a wedged process, on a clock that does not tick: it is
    /// alive, it says nothing, and the turn is not held open. The kernel
    /// retries a timeout, so the stream ends the way a dropped connection does.
    #[tokio::test(start_paused = true)]
    async fn a_stream_that_hears_nothing_gives_up_at_the_deadline() {
        let provider = remote(declared());
        let mut stream = provider
            .stream(request(), CancellationToken::new())
            .await
            .expect("the open goes out");
        let error = stream
            .next()
            .await
            .expect("a stream that says nothing still ends")
            .expect_err("with the timeout the kernel retries");
        assert_eq!(error, ProviderError::Timeout);
        assert!(error.retryable());
        assert!(stream.next().await.is_none(), "and then it is over");
    }
}
