//! The fan-out side of the actor: one bounded channel per subscriber, and the
//! `Lagged` marker a subscriber gets instead of the frames it was too slow for.

use std::sync::{Arc, Mutex};

use bingo_sdk::*;
use futures::StreamExt;
use jiff::Timestamp;
use tokio::sync::mpsc;

/// Frames a subscriber may fall behind by before it is told to resync.
pub const SUBSCRIBER_CAPACITY: usize = 256;

/// The gap a subscriber fell behind by: first and last missed seq. Shared
/// with its stream, which turns it into the `Lagged` marker once the frames
/// it did get are drained — and then ends, so the client has to resync.
type Lag = Arc<Mutex<Option<(Seq, Seq)>>>;

struct Subscriber {
    tx: mpsc::Sender<Frame>,
    lag: Lag,
}

#[derive(Default)]
pub(super) struct Subscribers {
    live: Vec<Subscriber>,
}

impl Subscribers {
    /// Offer the frame to everyone. A subscriber whose client is gone is
    /// forgotten; one that cannot keep up is marked and hears only the marker.
    pub(super) fn fanout(&mut self, frame: &Frame) {
        self.live.retain_mut(|s| {
            let mut lag = s.lag.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, to)) = lag.as_mut() {
                // Already behind: the stream ends at the marker, so nothing
                // after the gap is worth queueing.
                *to = frame.seq;
                return !s.tx.is_closed();
            }
            match s.tx.try_send(frame.clone()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    *lag = Some((frame.seq, frame.seq));
                    true
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            }
        });
    }

    /// A new subscription: `replay` first, then frames as they are published.
    pub(super) fn add(&mut self, session: SessionId, replay: Vec<Frame>) -> FrameStream {
        let (tx, rx) = mpsc::channel(SUBSCRIBER_CAPACITY);
        let lag: Lag = Arc::new(Mutex::new(None));
        self.live.push(Subscriber {
            tx,
            lag: Arc::clone(&lag),
        });
        Box::pin(futures::stream::iter(replay).chain(live(session, rx, lag)))
    }

    pub(super) fn clear(&mut self) {
        self.live.clear();
    }
}

/// The live tail of one subscription; it ends at the gap marker, if it reaches one.
fn live(
    session: SessionId,
    rx: mpsc::Receiver<Frame>,
    lag: Lag,
) -> impl futures::Stream<Item = Frame> + Send {
    futures::stream::unfold(Some((rx, lag)), move |slot| {
        let session = session.clone();
        async move {
            let (mut rx, lag) = slot?;
            match rx.try_recv() {
                Ok(frame) => return Some((frame, Some((rx, lag)))),
                Err(mpsc::error::TryRecvError::Disconnected) => return None,
                Err(mpsc::error::TryRecvError::Empty) => {}
            }
            let gap = lag.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some((from, to)) = gap {
                let marker = Frame {
                    seq: to,
                    ts: Timestamp::now(),
                    session,
                    cause: None,
                    event: Event::Lagged { from, to },
                };
                // Dropping the receiver ends the subscription on the actor's
                // side too.
                return Some((marker, None));
            }
            rx.recv().await.map(|frame| (frame, Some((rx, lag))))
        }
    })
}
