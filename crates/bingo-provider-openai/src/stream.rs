//! The streaming body: HTTP chunks → SSE frames → `ModelEvent`s.
//!
//! Two guards sit on it. A chunk that does not arrive within [`IDLE_TIMEOUT`]
//! ends the stream as a `Timeout`, so a server that connects and then goes
//! quiet cannot hang a headless run; a cancelled token ends it silently,
//! which is how an interrupt reaches the wire. Neither retries — the turn
//! loop owns the ladder.
//!
//! The guards are the same two `bingo-provider-anthropic::stream` carries: a
//! plugin may not import another plugin, so they are duplicated until they
//! earn a place in the sdk.

use std::collections::VecDeque;
use std::pin::Pin;
use std::time::Duration;

use bingo_sdk::{CancellationToken, ModelEvent, ModelStream, ProviderError};
use futures::{Stream, StreamExt};

use crate::events::Decoder;
use crate::sse::{SseFrame, SseParser};

/// How long the body may stay silent between chunks (old
/// `providers/openai.rs:41`).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

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

pub fn model_stream(chunks: Chunks, cancel: CancellationToken) -> ModelStream {
    let body = Body {
        chunks,
        parser: SseParser::new(),
        decoder: Decoder::new(),
        queue: VecDeque::new(),
        failure: None,
        done: false,
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
    cancel: CancellationToken,
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
        let cancel = self.cancel.clone();
        let chunk = tokio::select! {
            biased;
            _ = cancel.cancelled() => None,
            chunk = tokio::time::timeout(IDLE_TIMEOUT, self.chunks.next()) => Some(chunk),
        };
        // A cancelled turn ends where it stands; the loop already knows why.
        let Some(chunk) = chunk else {
            self.done = true;
            return;
        };
        match chunk {
            Err(_elapsed) => self.fail(ProviderError::Timeout),
            Ok(None) => self.end(),
            Ok(Some(Err(transport))) => self.fail(transport),
            Ok(Some(Ok(bytes))) => self.feed(&bytes),
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
        model_stream(chunks, CancellationToken::new())
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
    async fn a_mid_stream_failure_arrives_after_everything_that_preceded_it() {
        let items = drain(fixture("failed.sse", 13)).await;
        let (last, before) = items.split_last().expect("some events");
        assert!(
            matches!(last, Err(ProviderError::Server { status: 500, .. })),
            "{last:?}"
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
            Ok(b"event: response.in_progress\ndata: {}\n\n".to_vec()),
            Err(ProviderError::Transport {
                message: "connection reset".into(),
            }),
        ]));
        assert_eq!(
            drain(chunks).await,
            vec![Err(ProviderError::Transport {
                message: "connection reset".into()
            })]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_body_that_goes_quiet_times_out() {
        let chunks: Chunks = Box::pin(stream::pending());
        let mut events = model_stream(chunks, CancellationToken::new());
        assert_eq!(events.next().await, Some(Err(ProviderError::Timeout)));
        assert_eq!(events.next().await, None, "a timeout ends the stream");
    }

    #[tokio::test]
    async fn a_cancelled_turn_stops_the_stream_before_it_finishes() {
        let cancel = CancellationToken::new();
        let mut events = model_stream(fixture("text.sse", 64), cancel.clone());
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
        let events: Vec<_> = model_stream(fixture("text.sse", 64), cancel)
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
