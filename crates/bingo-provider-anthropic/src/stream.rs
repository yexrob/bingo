//! The streaming body: HTTP chunks → SSE frames → `ModelEvent`s.
//!
//! Two guards sit on it. Silence ends the stream as a `Timeout` — silence
//! being that no byte has moved on the wire, in either direction, for
//! [`IDLE_TIMEOUT`]; the clock is the one the request's own body stamped on
//! its way out (ADR-0056 §2) — so a server that connects and then goes quiet
//! cannot hang a headless run. A cancelled token ends the stream silently,
//! which is how an interrupt reaches the wire. Neither retries — the turn
//! loop owns the ladder.
//!
//! [`CONNECT_TIMEOUT`] bounds the phase before either of them: reaching the
//! server at all. Past it, size never ends a request; only silence does.
//!
//! The guards are the same two `bingo-provider-openai::stream` carries: a
//! plugin may not import another plugin, so they are duplicated until they
//! earn a place in the sdk.

use std::collections::VecDeque;
use std::pin::Pin;
use std::time::Duration;

use bingo_sdk::{CancellationToken, ModelEvent, ModelStream, ProviderError};
use futures::{Stream, StreamExt};

use crate::events::Decoder;
use crate::metered::{Clock, quiet};
use crate::sse::{SseFrame, SseParser};

/// How long reaching the server may take, which is the one phase with nothing
/// to meter yet (ADR-0056 §1).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// How long the wire may stay silent. The upload, the wait for the status
/// line and the body that follows share this one bound — Codex's number for
/// the same job (`codex-rs/model-provider-info`).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// The response body as bytes, with transport failures already named. Erasing
/// the chunk type here is what lets a fixture drive the decoding half without
/// an HTTP server.
pub type Chunks = Pin<Box<dyn Stream<Item = Result<Vec<u8>, ProviderError>> + Send>>;

pub fn chunks(response: reqwest::Response) -> Chunks {
    Box::pin(response.bytes_stream().map(|chunk| {
        chunk
            .map(|bytes| bytes.to_vec())
            .map_err(|e| ProviderError::Transport {
                message: e.to_string(),
            })
    }))
}

pub fn model_stream(chunks: Chunks, clock: Clock, cancel: CancellationToken) -> ModelStream {
    let body = Body {
        chunks,
        parser: SseParser::new(),
        decoder: Decoder::new(),
        queue: VecDeque::new(),
        failure: None,
        done: false,
        clock,
        cancel,
    };
    Box::pin(futures::stream::unfold(body, |mut body| async move {
        body.next().await.map(|item| (item, body))
    }))
}

/// One response being read. `queue` holds events already decoded but not yet
/// handed out, so a chunk carrying several frames is delivered event by event
/// and a failure still arrives *after* everything that preceded it.
struct Body {
    chunks: Chunks,
    parser: SseParser,
    decoder: Decoder,
    queue: VecDeque<ModelEvent>,
    failure: Option<ProviderError>,
    done: bool,
    /// The request stamped it too: one connection, one silence.
    clock: Clock,
    cancel: CancellationToken,
}

/// What ended one wait on the body.
enum Pulled {
    Cancelled,
    Chunk(Option<Result<Vec<u8>, ProviderError>>),
    Silent,
}

impl Body {
    async fn next(&mut self) -> Option<Result<ModelEvent, ProviderError>> {
        loop {
            if let Some(event) = self.queue.pop_front() {
                return Some(Ok(event));
            }
            if let Some(failure) = self.failure.take() {
                self.done = true;
                return Some(Err(failure));
            }
            if self.done || self.cancel.is_cancelled() {
                return None;
            }
            self.pump().await;
        }
    }

    /// One chunk, decoded into the queue — or the thing that ends the stream.
    async fn pump(&mut self) {
        match self.pull().await {
            // A cancelled turn ends where it stands; the loop knows why.
            Pulled::Cancelled => self.done = true,
            Pulled::Silent => self.fail(ProviderError::Timeout),
            Pulled::Chunk(None) => self.end(),
            Pulled::Chunk(Some(Err(transport))) => self.fail(transport),
            Pulled::Chunk(Some(Ok(bytes))) => self.feed(&bytes),
        }
    }

    /// Whichever comes first: the interrupt, the next chunk, the silence. A
    /// chunk that is ready beats a guard that has just expired.
    async fn pull(&mut self) -> Pulled {
        let cancel = self.cancel.clone();
        let clock = self.clock.clone();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Pulled::Cancelled,
            chunk = self.chunks.next() => {
                clock.stamp();
                Pulled::Chunk(chunk)
            }
            () = quiet(&clock, IDLE_TIMEOUT) => Pulled::Silent,
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        match self.parser.feed(bytes) {
            Ok(frames) => {
                for frame in frames {
                    if !self.absorb(&frame) {
                        return;
                    }
                }
            }
            Err(message) => self.fail(ProviderError::Stream { message }),
        }
    }

    /// The body ended: read whatever the last frame left unterminated.
    fn end(&mut self) {
        self.done = true;
        if let Some(frame) = self.parser.finish() {
            self.absorb(&frame);
        }
    }

    /// False when this frame ended the stream.
    fn absorb(&mut self, frame: &SseFrame) -> bool {
        match self.decoder.decode(&frame.event, &frame.data) {
            Ok(events) => {
                self.queue.extend(events);
                true
            }
            Err(error) => {
                self.fail(error);
                false
            }
        }
    }

    fn fail(&mut self, error: ProviderError) {
        self.failure = Some(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::{FinishReason, UnifiedFinish};
    use futures::stream;

    /// A fixture body, cut into small chunks so every frame straddles one.
    fn fixture(name: &str, chunk: usize) -> Chunks {
        let body = std::fs::read(crate::tests::fixture(name)).expect("read the fixture");
        let parts: Vec<Result<Vec<u8>, ProviderError>> =
            body.chunks(chunk).map(|c| Ok(c.to_vec())).collect();
        Box::pin(stream::iter(parts))
    }

    async fn drain(chunks: Chunks) -> Vec<Result<ModelEvent, ProviderError>> {
        model_stream(chunks, Clock::new(), CancellationToken::new())
            .collect()
            .await
    }

    #[tokio::test]
    async fn a_chunked_body_decodes_to_the_same_events_as_one_piece() {
        let whole = drain(fixture("text.sse", 4096)).await;
        let split = drain(fixture("text.sse", 7)).await;
        assert_eq!(whole, split, "framing must not depend on chunk size");
        assert!(matches!(
            whole.last(),
            Some(Ok(ModelEvent::Finish {
                finish_reason: FinishReason {
                    unified: UnifiedFinish::Stop,
                    ..
                },
                ..
            }))
        ));
    }

    #[tokio::test]
    async fn a_mid_stream_error_arrives_after_everything_that_preceded_it() {
        let items = drain(fixture("error_mid_stream.sse", 13)).await;
        let (last, before) = items.split_last().expect("some events");
        assert_eq!(
            last,
            &Err(ProviderError::Server {
                status: 529,
                message: "overloaded_error: Overloaded".into(),
            })
        );
        assert!(before.iter().all(Result::is_ok));
        assert!(matches!(
            before.last(),
            Some(Ok(ModelEvent::TextDelta { .. }))
        ));
    }

    #[tokio::test]
    async fn a_transport_failure_ends_the_stream() {
        let chunks: Chunks = Box::pin(stream::iter(vec![
            Ok(b"event: ping\ndata: {}\n\n".to_vec()),
            Err(ProviderError::Transport {
                message: "connection reset".into(),
            }),
        ]));
        let items = drain(chunks).await;
        assert_eq!(
            items,
            vec![Err(ProviderError::Transport {
                message: "connection reset".into()
            })]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_body_that_goes_quiet_times_out() {
        let chunks: Chunks = Box::pin(stream::pending());
        let mut events = model_stream(chunks, Clock::new(), CancellationToken::new());
        assert_eq!(events.next().await, Some(Err(ProviderError::Timeout)));
        assert_eq!(events.next().await, None, "a timeout ends the stream");
    }

    #[tokio::test]
    async fn a_cancelled_turn_stops_the_stream_before_it_finishes() {
        let cancel = CancellationToken::new();
        let mut events = model_stream(fixture("text.sse", 64), Clock::new(), cancel.clone());
        assert!(events.next().await.is_some());
        cancel.cancel();
        let rest: Vec<_> = events.collect().await;
        assert!(
            !rest
                .iter()
                .any(|e| matches!(e, Ok(ModelEvent::Finish { .. }))),
            "a cancelled stream never finishes"
        );
    }

    #[tokio::test]
    async fn a_token_cancelled_before_the_first_poll_yields_nothing() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let events: Vec<_> = model_stream(fixture("text.sse", 64), Clock::new(), cancel)
            .collect()
            .await;
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn a_body_that_never_frames_is_a_stream_error() {
        let flood: Vec<Result<Vec<u8>, ProviderError>> =
            (0..9).map(|_| Ok(vec![b'x'; 1024 * 1024])).collect();
        let items = drain(Box::pin(stream::iter(flood))).await;
        assert!(matches!(
            items.last(),
            Some(Err(ProviderError::Stream { .. }))
        ));
    }
}
