//! What the terminal is holding, and the bytes that make it hold what the
//! frame just drew.
//!
//! The transcript says which pictures a frame placed and how big; this is the
//! one place that turns the difference between that and what went out before
//! into bytes. The invariant is a sentence: **the terminal holds exactly the
//! last [`KEPT`] pictures the transcript holds**. A picture drawn for the
//! first time is transmitted, one whose rectangle changed is placed again
//! without sending its bytes twice, and one that has fallen off the end is
//! deleted so a long conversation cannot fill the terminal's memory.

use std::sync::Arc;

use bingo_pictures::Png;

use super::kitty;
use super::picture::Picture;

/// How many pictures the terminal is asked to keep. Enough that scrolling
/// back over a few screens finds them all still there, few enough that a
/// conversation full of screenshots does not grow without end.
pub const KEPT: usize = 32;

/// The pictures the terminal has, in the order they were sent.
#[derive(Debug, Default)]
pub struct Stored {
    held: Vec<Picture>,
}

impl Stored {
    /// The bytes that make the terminal hold `placed` and nothing else.
    ///
    /// `pixels` is asked only for a picture the terminal has not got: a
    /// redraw of one it already has costs no decode and no bytes.
    pub fn catch_up(
        &mut self,
        placed: &[Picture],
        pixels: impl Fn(&Picture) -> Option<Arc<Png>>,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        let mut held: Vec<Picture> = Vec::new();
        for picture in wanted(placed) {
            if held.iter().any(|kept| kept.id() == picture.id()) {
                continue;
            }
            match self.sending(picture, &pixels) {
                Some(bytes) => out.extend_from_slice(&bytes),
                // Nothing to send it with, so the terminal does not hold it
                // and nobody may believe it does.
                None => continue,
            }
            held.push(picture.clone());
        }
        out.extend(self.forgetting(&held));
        self.held = held;
        out
    }

    /// What one picture costs: everything, when the terminal has never seen
    /// it; a new rectangle, when only that changed; nothing at all otherwise.
    fn sending(
        &self,
        picture: &Picture,
        pixels: &impl Fn(&Picture) -> Option<Arc<Png>>,
    ) -> Option<Vec<u8>> {
        let id = picture.id();
        match self.held.iter().find(|kept| kept.id() == id) {
            Some(kept) if kept == picture => Some(Vec::new()),
            Some(_) => Some(kitty::place(id, picture.cols, picture.rows)),
            None => {
                let png = pixels(picture)?;
                Some(kitty::transmit(id, &png.bytes, picture.cols, picture.rows))
            }
        }
    }

    /// The bytes for everything the terminal is holding that this frame did
    /// not place.
    fn forgetting(&self, held: &[Picture]) -> Vec<u8> {
        self.held
            .iter()
            .map(Picture::id)
            .filter(|id| !held.iter().any(|kept| kept.id() == *id))
            .flat_map(kitty::delete)
            .collect()
    }
}

/// The last [`KEPT`] of what the frame placed. The transcript hands them over
/// in its own order, so the ones that survive are the newest — which is where
/// a person is reading.
fn wanted(placed: &[Picture]) -> &[Picture] {
    &placed[placed.len().saturating_sub(KEPT)..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::ItemId;

    fn picture(item: &str, cols: u16, rows: u16) -> Picture {
        Picture {
            item: ItemId::from_raw(item),
            part: 0,
            cols,
            rows,
        }
    }

    fn pixels(_: &Picture) -> Option<Arc<Png>> {
        Some(Arc::new(Png {
            bytes: b"png!".to_vec(),
            width: 4,
            height: 2,
        }))
    }

    fn text(bytes: Vec<u8>) -> String {
        String::from_utf8(bytes).expect("ascii")
    }

    #[test]
    fn a_picture_is_transmitted_once_however_many_frames_draw_it() {
        let mut stored = Stored::default();
        let placed = vec![picture("itm_1", 4, 2)];
        let first = text(stored.catch_up(&placed, pixels));
        assert!(first.contains("a=T,f=100,q=2,U=1"), "{first:?}");
        assert_eq!(
            stored.catch_up(&placed, pixels),
            Vec::<u8>::new(),
            "the second frame costs nothing"
        );
    }

    /// A fold opening under a picture changes its rectangle and not its
    /// bytes, so the terminal is told where it goes rather than sent it again.
    #[test]
    fn a_picture_that_changed_size_is_placed_again_and_not_sent_again() {
        let mut stored = Stored::default();
        stored.catch_up(&[picture("itm_1", 4, 2)], pixels);
        let moved = text(stored.catch_up(&[picture("itm_1", 40, 12)], pixels));
        assert!(moved.starts_with("\x1b_Ga=p,"), "{moved:?}");
        assert!(moved.contains("c=40,r=12"), "{moved:?}");
        assert!(!moved.contains("a=T"), "the bytes did not go twice");
    }

    #[test]
    fn a_picture_the_transcript_no_longer_holds_is_deleted() {
        let mut stored = Stored::default();
        let gone = picture("itm_1", 4, 2);
        stored.catch_up(std::slice::from_ref(&gone), pixels);
        let after = text(stored.catch_up(&[], pixels));
        assert_eq!(after, format!("\x1b_Ga=d,d=I,q=2,i={}\x1b\\", gone.id()));
        assert!(stored.held.is_empty());
    }

    /// Past the cap the oldest go, so a conversation full of screenshots
    /// leaves the terminal holding a bounded number of them.
    #[test]
    fn no_more_than_the_cap_is_ever_held() {
        let mut stored = Stored::default();
        let all: Vec<Picture> = (0..KEPT + 3)
            .map(|i| picture(&format!("itm_{i}"), 4, 2))
            .collect();
        stored.catch_up(&all[..KEPT], pixels);
        let rolled = text(stored.catch_up(&all, pixels));
        assert_eq!(stored.held.len(), KEPT);
        assert_eq!(
            stored.held.first().map(Picture::id),
            all.get(3).map(Picture::id),
            "the window is the newest of them"
        );
        assert_eq!(
            rolled.matches("a=d,d=I").count(),
            3,
            "and the three that fell off were let go"
        );
    }

    /// A picture no decoder read is not held and not claimed to be: the next
    /// frame asks again rather than leaving a placeholder over nothing.
    #[test]
    fn a_picture_without_pixels_is_never_claimed_to_be_held() {
        let mut stored = Stored::default();
        let bytes = stored.catch_up(&[picture("itm_1", 4, 2)], |_| None);
        assert!(bytes.is_empty());
        assert!(stored.held.is_empty());
    }

    /// The same picture in two blocks of one frame — a transcript and the
    /// pager over it — is one picture to the terminal.
    #[test]
    fn one_picture_placed_twice_is_sent_once() {
        let mut stored = Stored::default();
        let twice = vec![picture("itm_1", 4, 2), picture("itm_1", 4, 2)];
        let bytes = text(stored.catch_up(&twice, pixels));
        assert_eq!(bytes.matches("a=T").count(), 1);
        assert_eq!(stored.held.len(), 1);
    }
}
