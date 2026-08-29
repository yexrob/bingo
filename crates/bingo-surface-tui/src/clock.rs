//! The two clocks a frame is drawn against, kept apart on purpose: the
//! surface's own timers are monotonic (`Instant`), while every deadline the
//! kernel states — an interaction's guard, a turn's start — is wall time.

use std::time::Instant;

use jiff::Timestamp;

#[derive(Clone, Copy, Debug)]
pub struct Now {
    pub instant: Instant,
    pub wall: Timestamp,
}

impl Now {
    pub fn real() -> Self {
        Self {
            instant: Instant::now(),
            wall: Timestamp::now(),
        }
    }
}
