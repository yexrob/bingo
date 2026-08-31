//! A message too big for one frame arrives in parts (ADR-0016 §6).
//!
//! `sum` is how many, `seq` is which, and `message_id` is what ties them
//! together. Parts nobody finishes are dropped after five seconds, because a
//! half a message held forever is a leak and a half a message delivered is a
//! lie.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::frame::{Frame, header};

/// How long a message may stay half-arrived.
pub const LIFETIME: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct Partial {
    parts: Vec<Option<Vec<u8>>>,
    /// The first part to arrive, which the whole message is rebuilt around.
    first: Frame,
    started: Instant,
}

impl Partial {
    fn whole(&self) -> Option<Frame> {
        let mut payload = Vec::new();
        for part in &self.parts {
            payload.extend_from_slice(part.as_deref()?);
        }
        Some(Frame {
            payload,
            ..self.first.clone()
        })
    }
}

#[derive(Debug, Default)]
pub struct Chunks {
    partials: HashMap<String, Partial>,
}

impl Chunks {
    /// The frame whole, when this one completes it. A frame that was never
    /// split is whole the moment it arrives.
    pub fn absorb(&mut self, frame: Frame, now: Instant) -> Option<Frame> {
        let sum = frame.number(header::SUM).unwrap_or(1);
        if sum <= 1 {
            return Some(frame);
        }
        let seq = frame.number(header::SEQ)?;
        let id = frame.header(header::MESSAGE_ID)?.to_string();
        let partial = self.partials.entry(id.clone()).or_insert_with(|| Partial {
            parts: vec![None; sum],
            first: frame.clone(),
            started: now,
        });
        // A `sum` that disagrees with the one that opened this message is a
        // peer contradicting itself; the part is dropped rather than resized
        // into a message of the wrong length.
        *partial.parts.get_mut(seq)? = Some(frame.payload);
        let whole = partial.whole()?;
        self.partials.remove(&id);
        Some(whole)
    }

    /// Forget what nobody finished. Called on every tick of the read loop.
    pub fn expire(&mut self, now: Instant) {
        self.partials
            .retain(|_, partial| now.saturating_duration_since(partial.started) < LIFETIME);
    }

    #[cfg(test)]
    fn waiting(&self) -> usize {
        self.partials.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(id: &str, sum: usize, seq: usize, payload: &str) -> Frame {
        let mut frame = Frame {
            payload: payload.as_bytes().to_vec(),
            payload_type: "im.message.receive_v1".into(),
            ..Frame::default()
        };
        frame.set_header(header::TYPE, "event");
        frame.set_header(header::MESSAGE_ID, id);
        frame.set_header(header::SUM, sum.to_string());
        frame.set_header(header::SEQ, seq.to_string());
        frame
    }

    #[test]
    fn a_message_of_one_part_is_whole_already() {
        let mut chunks = Chunks::default();
        let whole = chunks.absorb(part("om_1", 1, 0, "{}"), Instant::now());
        assert_eq!(whole.expect("whole").payload, b"{}");
        assert_eq!(chunks.waiting(), 0);
    }

    #[test]
    fn parts_are_joined_in_seq_order_however_they_arrive() {
        let mut chunks = Chunks::default();
        let now = Instant::now();
        assert!(chunks.absorb(part("om_1", 2, 1, "world\"}"), now).is_none());
        assert_eq!(chunks.waiting(), 1);
        let whole = chunks
            .absorb(part("om_1", 2, 0, "{\"a\":\"hello "), now)
            .expect("the message is whole");
        assert_eq!(
            String::from_utf8_lossy(&whole.payload),
            "{\"a\":\"hello world\"}"
        );
        assert_eq!(chunks.waiting(), 0, "a whole message is not still waiting");
        assert_eq!(
            whole.payload_type, "im.message.receive_v1",
            "the rest of the frame is the first part's"
        );
    }

    #[test]
    fn two_messages_in_flight_do_not_mix() {
        let mut chunks = Chunks::default();
        let now = Instant::now();
        chunks.absorb(part("om_1", 2, 0, "a"), now);
        chunks.absorb(part("om_2", 2, 0, "x"), now);
        assert!(chunks.absorb(part("om_1", 2, 1, "b"), now).is_some());
        assert_eq!(chunks.waiting(), 1);
    }

    #[test]
    fn a_part_nobody_finished_is_dropped_after_five_seconds() {
        let mut chunks = Chunks::default();
        let now = Instant::now();
        chunks.absorb(part("om_1", 2, 0, "a"), now);
        chunks.expire(now + LIFETIME - Duration::from_millis(1));
        assert_eq!(chunks.waiting(), 1);
        chunks.expire(now + LIFETIME);
        assert_eq!(chunks.waiting(), 0);
    }

    #[test]
    fn a_part_past_the_end_of_its_message_is_dropped_rather_than_resizing_it() {
        let mut chunks = Chunks::default();
        let now = Instant::now();
        chunks.absorb(part("om_1", 2, 0, "a"), now);
        assert!(chunks.absorb(part("om_1", 2, 5, "?"), now).is_none());
        assert!(chunks.absorb(part("om_1", 2, 1, "b"), now).is_some());
    }
}
