//! What the terminal is holding, and the bytes that make it hold what the
//! frame just drew.
//!
//! The frame says which pictures it placed and how big; this is the one place
//! that turns the difference between that and what went out before into
//! bytes. The invariant is a sentence: **the terminal holds the last
//! [`KEPT`] pictures a frame placed**. A picture drawn for the first time is
//! transmitted, one whose rectangle changed is transmitted again — the bytes
//! are cut to the cells they will cover (M48 brick 2), so a bigger rectangle
//! is a picture the terminal was never given — and one that has fallen off
//! the end is deleted, so a long conversation cannot fill the terminal's
//! memory. A picture a frame did not place stays held: a list opened over
//! the transcript, a scroll past a screenshot, hide its cells for a moment,
//! and a picture deleted then sent again on the way back is a picture that
//! flickers. With no cells to draw into, a held picture costs the terminal
//! nothing but the memory the cap bounds.

use std::sync::Arc;

use bingo_pictures::Png;

use super::kitty;
use super::picture::Picture;
use super::tmux::Transport;

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
        transport: Transport,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        let mut held = self.held.clone();
        for picture in wanted(placed) {
            // Already there at this very rectangle — from an earlier frame,
            // or from a block earlier in this one — costs nothing.
            if !held.contains(picture) && !self.held.contains(picture) {
                match transmitted(picture, &pixels, transport) {
                    Some(bytes) => out.extend_from_slice(&bytes),
                    // Nothing to send it with, so the terminal does not hold
                    // it and nobody may believe it does.
                    None => {
                        held.retain(|kept| kept.id() != picture.id());
                        continue;
                    }
                }
            }
            // Placed again is newest again, whether or not it was sent.
            held.retain(|kept| kept.id() != picture.id());
            held.push(picture.clone());
        }
        held.drain(..held.len().saturating_sub(KEPT));
        out.extend(self.forgetting(&held, transport));
        self.held = held;
        out
    }

    /// The bytes that make the terminal let every held picture go — the way
    /// out, where nothing will ever place them again.
    pub fn forget_all(&mut self, transport: Transport) -> Vec<u8> {
        let out = self.forgetting(&[], transport);
        self.held.clear();
        out
    }

    /// The bytes for everything the terminal is holding that this frame did
    /// not place.
    fn forgetting(&self, held: &[Picture], transport: Transport) -> Vec<u8> {
        self.held
            .iter()
            .map(Picture::id)
            .filter(|id| !held.iter().any(|kept| kept.id() == *id))
            .flat_map(|id| kitty::delete(id, transport))
            .collect()
    }
}

/// The pixels of one picture's rectangle, as the terminal is given them.
fn transmitted(
    picture: &Picture,
    pixels: &impl Fn(&Picture) -> Option<Arc<Png>>,
    transport: Transport,
) -> Option<Vec<u8>> {
    let png = pixels(picture)?;
    Some(kitty::transmit(
        picture.id(),
        &png.bytes,
        picture.cols,
        picture.rows,
        transport,
    ))
}

/// The last [`KEPT`] of what the frame placed. They are handed over in the
/// order they were drawn, so the ones that survive are the newest — which is
/// where a person is reading, and where the composer's own strip is.
fn wanted(placed: &[Picture]) -> &[Picture] {
    &placed[placed.len().saturating_sub(KEPT)..]
}

#[cfg(test)]
mod tests {
    use super::super::picture::Source;
    use super::*;
    use bingo_sdk::ItemId;

    fn picture(item: &str, cols: u16, rows: u16) -> Picture {
        Picture {
            source: Source::Journal {
                item: ItemId::from_raw(item),
                part: 0,
            },
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
        let first = text(stored.catch_up(&placed, pixels, Transport::Bare));
        assert!(first.contains("a=T,f=100,q=2,U=1"), "{first:?}");
        assert_eq!(
            stored.catch_up(&placed, pixels, Transport::Bare),
            Vec::<u8>::new(),
            "the second frame costs nothing"
        );
    }

    /// A fold opening under a picture asks for a bigger rectangle, and the
    /// bytes the terminal was given were cut to the small one: what it never
    /// had it is sent, at the size it will now show.
    #[test]
    fn a_picture_that_grew_is_sent_again_at_the_size_it_grew_to() {
        let mut stored = Stored::default();
        stored.catch_up(&[picture("itm_1", 4, 2)], pixels, Transport::Bare);
        let moved = text(stored.catch_up(&[picture("itm_1", 40, 12)], pixels, Transport::Bare));
        assert!(moved.starts_with("\x1b_Ga=T,f=100"), "{moved:?}");
        assert!(moved.contains("c=40,r=12"), "{moved:?}");
        assert_eq!(stored.held, vec![picture("itm_1", 40, 12)]);
    }

    /// A layer over the transcript, or a scroll past it, places no picture
    /// for a frame: the terminal keeps holding it, so its return costs no
    /// bytes and shows no flicker.
    #[test]
    fn a_picture_a_frame_did_not_place_is_kept_for_its_return() {
        let mut stored = Stored::default();
        let hidden = picture("itm_1", 4, 2);
        stored.catch_up(std::slice::from_ref(&hidden), pixels, Transport::Bare);
        assert!(stored.catch_up(&[], pixels, Transport::Bare).is_empty());
        assert_eq!(stored.held, vec![hidden.clone()]);
        assert!(
            stored
                .catch_up(std::slice::from_ref(&hidden), pixels, Transport::Bare)
                .is_empty(),
            "back on the screen, it is already there"
        );
    }

    #[test]
    fn what_is_held_is_deleted_when_it_falls_off_the_end() {
        let mut stored = Stored::default();
        let old = picture("itm_old", 4, 2);
        stored.catch_up(std::slice::from_ref(&old), pixels, Transport::Bare);
        let newer: Vec<Picture> = (0..KEPT)
            .map(|i| picture(&format!("itm_{i}"), 4, 2))
            .collect();
        let rolled = text(stored.catch_up(&newer, pixels, Transport::Bare));
        assert_eq!(rolled.matches("a=d,d=I").count(), 1);
        assert!(
            rolled.ends_with(&format!("i={}\x1b\\", old.id())),
            "{rolled:?}"
        );
        assert_eq!(stored.held.len(), KEPT);
    }

    /// Past the cap the oldest go, so a conversation full of screenshots
    /// leaves the terminal holding a bounded number of them.
    #[test]
    fn no_more_than_the_cap_is_ever_held() {
        let mut stored = Stored::default();
        let all: Vec<Picture> = (0..KEPT + 3)
            .map(|i| picture(&format!("itm_{i}"), 4, 2))
            .collect();
        stored.catch_up(&all[..KEPT], pixels, Transport::Bare);
        let rolled = text(stored.catch_up(&all, pixels, Transport::Bare));
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

    #[test]
    fn the_way_out_lets_everything_go() {
        let mut stored = Stored::default();
        let held = picture("itm_1", 4, 2);
        stored.catch_up(std::slice::from_ref(&held), pixels, Transport::Bare);
        let gone = text(stored.forget_all(Transport::Bare));
        assert_eq!(gone, format!("\x1b_Ga=d,d=I,q=2,i={}\x1b\\", held.id()));
        assert!(stored.held.is_empty());
    }

    /// A picture no decoder read is not held and not claimed to be: the next
    /// frame asks again rather than leaving a placeholder over nothing.
    #[test]
    fn a_picture_without_pixels_is_never_claimed_to_be_held() {
        let mut stored = Stored::default();
        let bytes = stored.catch_up(&[picture("itm_1", 4, 2)], |_| None, Transport::Bare);
        assert!(bytes.is_empty());
        assert!(stored.held.is_empty());
    }

    /// M49 brick 1: through tmux the reconciler's bytes are the same bytes
    /// in tmux's envelope — the picture on the way in and the forgetting on
    /// the way out.
    #[test]
    fn through_tmux_every_sequence_goes_out_wrapped() {
        let mut stored = Stored::default();
        let held = picture("itm_1", 4, 2);
        let sent = text(stored.catch_up(std::slice::from_ref(&held), pixels, Transport::Tmux));
        assert!(
            sent.starts_with("\x1bPtmux;\x1b\x1b_Ga=T,f=100"),
            "{sent:?}"
        );
        assert!(sent.ends_with("\x1b\x1b\\\x1b\\"), "{sent:?}");
        let newer: Vec<Picture> = (0..KEPT)
            .map(|i| picture(&format!("new_{i}"), 4, 2))
            .collect();
        let gone = text(stored.catch_up(&newer, pixels, Transport::Tmux));
        assert!(
            gone.ends_with(&format!(
                "\x1bPtmux;\x1b\x1b_Ga=d,d=I,q=2,i={}\x1b\x1b\\\x1b\\",
                held.id()
            )),
            "{gone:?}"
        );
    }

    /// The same picture in two blocks of one frame — a transcript and the
    /// pager over it — is one picture to the terminal.
    #[test]
    fn one_picture_placed_twice_is_sent_once() {
        let mut stored = Stored::default();
        let twice = vec![picture("itm_1", 4, 2), picture("itm_1", 4, 2)];
        let bytes = text(stored.catch_up(&twice, pixels, Transport::Bare));
        assert_eq!(bytes.matches("a=T").count(), 1);
        assert_eq!(stored.held.len(), 1);

        let apart = vec![
            picture("itm_2", 4, 2),
            picture("itm_3", 4, 2),
            picture("itm_2", 4, 2),
        ];
        let bytes = text(stored.catch_up(&apart, pixels, Transport::Bare));
        assert_eq!(bytes.matches("a=T").count(), 2, "{bytes:?}");
        assert_eq!(stored.held.len(), 3);
    }
}
