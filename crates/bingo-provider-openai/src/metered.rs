//! The request body as movement (ADR-0056 §2).
//!
//! A guard that watches a request cannot tell a slow upload from a silent
//! server unless something stamps the moment the last byte moved. [`Metered`]
//! is that something: one `Bytes` handed to the transport in frames of at most
//! [`FRAME`], each frame stamping a [`Clock`] the response then goes on
//! stamping per chunk. [`quiet`] is the guard that reads it.
//!
//! The hint is exact, so the request keeps its `Content-Length` and is never
//! chunked, and the builder above is never handed the metered body — it keeps
//! its own bytes for `RequestBuilder::try_clone` (ADR-0056 §3).
//!
//! The same module sits in `bingo-provider-anthropic`: a plugin may not import
//! another plugin, so the bricks are duplicated as the guards already are.

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::Duration;

use bingo_sdk::ProviderError;
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
// Tokio's clock, not the standard one: a test pauses time, and only this one
// moves with it.
use tokio::time::Instant;

/// The most the transport is handed at once (ADR-0056 §2). Small enough that a
/// connection which stalls mid-body stops the clock within one frame's worth
/// of writing; large enough that an 8 MB conversation is a few hundred frames.
pub const FRAME: usize = 64 * 1024;

/// When a byte last moved on one connection, in either direction. The request
/// and the response that follows it share one, because one silence is worth
/// exactly as much as the other.
#[derive(Clone, Debug)]
pub struct Clock(Arc<Mutex<Instant>>);

impl Clock {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }

    pub fn stamp(&self) {
        *self.at() = Instant::now();
    }

    pub fn last(&self) -> Instant {
        *self.at()
    }

    /// Behind the lock is one `Instant` and nothing else, so a panic elsewhere
    /// cannot have left it half-written: a poisoned lock is read as it stands
    /// rather than taking the turn down with it.
    fn at(&self) -> MutexGuard<'_, Instant> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolves once the clock has stood still for `idle`, and never before: every
/// stamp pushes the deadline out again.
pub async fn quiet(clock: &Clock, idle: Duration) {
    loop {
        let deadline = clock.last() + idle;
        if Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep_until(deadline).await;
    }
}

/// One request body, handed out in frames that stamp for themselves.
#[derive(Debug)]
pub struct Metered {
    rest: Bytes,
    clock: Clock,
}

impl Metered {
    pub fn new(bytes: Bytes, clock: Clock) -> Self {
        Self { rest: bytes, clock }
    }
}

impl Body for Metered {
    type Data = Bytes;
    /// Reading from memory cannot fail; the transport's own failures are the
    /// only ones a request has.
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let body = self.get_mut();
        if body.rest.is_empty() {
            return Poll::Ready(None);
        }
        let frame = body.rest.split_to(FRAME.min(body.rest.len()));
        body.clock.stamp();
        Poll::Ready(Some(Ok(Frame::data(frame))))
    }

    fn is_end_stream(&self) -> bool {
        self.rest.is_empty()
    }

    /// Exact, which is what keeps `Content-Length` on the request: hyper reads
    /// this hint and chunks whatever cannot answer it.
    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.rest.len() as u64)
    }
}

/// The request as it goes out: the same bytes, in frames that stamp `clock`.
/// A request with no body at all — every `GET` here — is left alone.
pub fn meter(
    builder: reqwest::RequestBuilder,
    clock: &Clock,
) -> Result<reqwest::Request, ProviderError> {
    let mut request = builder.build().map_err(|e| ProviderError::Request {
        message: format!("the request could not be built: {e}"),
    })?;
    let bytes = request
        .body()
        .map(|body| body.as_bytes().map(Bytes::copy_from_slice));
    match bytes {
        None => Ok(request),
        // Nothing here builds a streaming body, and one that slipped through
        // would be a request no guard could watch.
        Some(None) => Err(ProviderError::Config {
            message: "a streaming request body cannot be metered".into(),
        }),
        Some(Some(bytes)) => {
            *request.body_mut() = Some(reqwest::Body::wrap(Metered::new(bytes, clock.clone())));
            Ok(request)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::Waker;

    /// Every frame the body yields, in order. `poll_frame` reads memory and
    /// never parks, so one pass with a no-op waker drains it.
    fn frames(body: &mut Metered) -> Vec<Bytes> {
        let mut cx = Context::from_waker(Waker::noop());
        let mut frames = Vec::new();
        loop {
            match Pin::new(&mut *body).poll_frame(&mut cx) {
                Poll::Ready(Some(Ok(frame))) => {
                    frames.push(frame.into_data().expect("a data frame"))
                }
                Poll::Ready(None) => return frames,
                Poll::Pending => panic!("a body over memory never parks"),
            }
        }
    }

    fn body(len: usize) -> (Metered, Clock) {
        let clock = Clock::new();
        let bytes: Bytes = (0..len).map(|i| i as u8).collect::<Vec<u8>>().into();
        (Metered::new(bytes, clock.clone()), clock)
    }

    #[test]
    fn the_frames_are_the_body_cut_at_sixty_four_kibibytes() {
        let len = FRAME * 2 + 7;
        let (mut metered, _) = body(len);
        let frames = frames(&mut metered);
        assert_eq!(frames.len(), 3);
        assert!(frames.iter().all(|frame| frame.len() <= FRAME));
        assert_eq!(frames.concat(), body(len).0.rest, "the bytes are the bytes");
    }

    #[test]
    fn the_size_hint_is_exact_so_the_request_is_never_chunked() {
        let (metered, _) = body(FRAME * 2 + 7);
        assert_eq!(metered.size_hint().exact(), Some(FRAME as u64 * 2 + 7));
        assert!(!metered.is_end_stream());
    }

    #[test]
    fn an_empty_body_ends_at_once() {
        let (mut metered, clock) = body(0);
        let stamped = clock.last();
        assert!(metered.is_end_stream());
        assert_eq!(metered.size_hint().exact(), Some(0));
        assert!(frames(&mut metered).is_empty());
        assert_eq!(clock.last(), stamped, "nothing moved, nothing was stamped");
    }

    #[tokio::test(start_paused = true)]
    async fn every_frame_stamps_the_clock_once() {
        let (mut metered, clock) = body(FRAME * 3);
        let start = clock.last();
        let mut stamps = Vec::new();
        let mut cx = Context::from_waker(Waker::noop());
        while let Poll::Ready(Some(Ok(_))) = Pin::new(&mut metered).poll_frame(&mut cx) {
            stamps.push(clock.last());
            tokio::time::advance(Duration::from_secs(1)).await;
        }
        assert_eq!(stamps.len(), 3);
        assert_eq!(
            stamps,
            (0..3)
                .map(|i| start + Duration::from_secs(i))
                .collect::<Vec<_>>(),
            "one stamp per frame, at the moment the frame went out"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_clock_that_keeps_moving_is_never_quiet() {
        let clock = Clock::new();
        let moving = clock.clone();
        let ticking = tokio::spawn(async move {
            for _ in 0..60 {
                tokio::time::sleep(Duration::from_secs(10)).await;
                moving.stamp();
            }
        });
        assert!(
            tokio::time::timeout(
                Duration::from_secs(600),
                quiet(&clock, Duration::from_secs(60))
            )
            .await
            .is_err(),
            "ten minutes of movement is not a silence"
        );
        ticking.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn a_clock_that_stops_is_quiet_one_idle_later() {
        let clock = Clock::new();
        let start = Instant::now();
        quiet(&clock, Duration::from_secs(60)).await;
        assert_eq!(start.elapsed(), Duration::from_secs(60));
    }

    #[tokio::test(start_paused = true)]
    async fn a_stamp_pushes_the_silence_out_by_a_whole_idle() {
        let clock = Clock::new();
        let start = Instant::now();
        let stamped = clock.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(59)).await;
            stamped.stamp();
        });
        quiet(&clock, Duration::from_secs(60)).await;
        assert_eq!(start.elapsed(), Duration::from_secs(119));
    }
}
