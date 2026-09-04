//! The pictures this surface has turned into pixels, kept so it does it once.
//!
//! A frame has to know how many pixels a picture is before it can say how many
//! cells it takes, and finding that out means decoding it (`bingo-pictures`).
//! That is far too much work to do at every draw, so the answer is kept here —
//! including the answer "no decoder read it", which would otherwise be paid for
//! over and over by the one picture that cannot be drawn.
//!
//! What goes *out* is a second question with a second answer: the terminal
//! only ever shows the pixels the cells hold, so a picture is shrunk to its
//! rectangle before it is sent ([`Decoded::thumbnail`]) and the shrink is
//! remembered under that rectangle. The whole is what a frame measures; the
//! thumbnail is what the wire carries.
//!
//! It is a memo of work already done, not a second copy of anything: the
//! picture itself stays where it came from, and this holds no more pictures
//! than the terminal itself is holding ([`super::stored`]).

use std::cell::RefCell;
use std::sync::Arc;

use bingo_pictures::Png;
use bingo_sdk::Image;

use super::stored::KEPT;

/// What one answer is about: a picture, whole or at the size of a rectangle
/// of cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Asked {
    id: u32,
    /// The pixels it was fitted into, or `None` for the picture as it came.
    within: Option<(u32, u32)>,
}

/// How many answers are kept. Twice [`KEPT`] because a picture on the screen
/// is asked about twice — whole, to measure it, and at the size it was sent.
const ANSWERS: usize = 2 * KEPT;

/// Pictures as pixels, by what was asked about them, oldest first.
///
/// The cell is inside rather than around it because this is a memo: a draw
/// holds it by shared reference, the way the frame holds everything else it
/// derives ([`crate::ui::Painted`]).
#[derive(Debug, Default)]
pub struct Decoded {
    kept: RefCell<Vec<(Asked, Option<Arc<Png>>)>>,
}

impl Decoded {
    /// This picture as pixels, decoded at most once. `None` where no decoder
    /// read it — which is the degrade of design §5, not an error to report.
    pub fn png(&self, id: u32, image: &Image) -> Option<Arc<Png>> {
        let asked = Asked { id, within: None };
        if let Some(known) = self.known(asked) {
            return known;
        }
        let png = bingo_pictures::to_png(image)
            .inspect_err(|e| tracing::debug!(%id, error = %e, "a picture no decoder read"))
            .ok()
            .map(Arc::new);
        self.keep(asked, png.clone());
        png
    }

    /// This picture at the pixels a rectangle of cells holds: what goes over
    /// the wire, so a screenshot in a twelve-row block costs the block's
    /// kilobytes and not the screenshot's. A picture already inside the box
    /// is the picture itself, shrunk by nothing.
    pub fn thumbnail(&self, id: u32, image: &Image, within: (u32, u32)) -> Option<Arc<Png>> {
        let asked = Asked {
            id,
            within: Some(within),
        };
        if let Some(known) = self.known(asked) {
            return known;
        }
        let small = Arc::new(bingo_pictures::fitted(image, within).ok()?);
        self.keep(asked, Some(small.clone()));
        Some(small)
    }

    /// What is already known about this question, and nothing about whether
    /// the answer was a picture: `Some(None)` is a picture that would not
    /// decode last time and will not decode now.
    fn known(&self, asked: Asked) -> Option<Option<Arc<Png>>> {
        let kept = self.kept.borrow();
        let (_, png) = kept.iter().find(|(kept, _)| *kept == asked)?;
        Some(png.clone())
    }

    fn keep(&self, asked: Asked, png: Option<Arc<Png>>) {
        let mut kept = self.kept.borrow_mut();
        kept.push((asked, png));
        let over = kept.len().saturating_sub(ANSWERS);
        kept.drain(..over);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_pictures::testing::{png, unreadable};

    #[test]
    fn a_picture_is_decoded_once_and_answered_from_the_memo_after() {
        let decoded = Decoded::default();
        let first = decoded.png(1, &png(3, 4)).expect("pixels");
        let again = decoded.png(1, &png(9, 9)).expect("pixels");
        assert_eq!(
            (again.width, again.height),
            (3, 4),
            "the id is what it was asked about"
        );
        assert!(Arc::ptr_eq(&first, &again), "and nothing was decoded twice");
    }

    #[test]
    fn a_picture_no_decoder_reads_is_remembered_as_such() {
        let decoded = Decoded::default();
        let broken = unreadable();
        assert!(decoded.png(2, &broken).is_none());
        assert!(decoded.png(2, &broken).is_none());
        assert!(decoded.thumbnail(2, &broken, (10, 10)).is_none());
        assert_eq!(decoded.kept.borrow().len(), 1, "asked once, kept once");
    }

    /// The wire's question: the picture at the size of its cells, shrunk once
    /// and answered from the memo after — and never blown up.
    #[test]
    fn a_picture_is_shrunk_to_its_cells_once_and_answered_from_the_memo_after() {
        let decoded = Decoded::default();
        let shot = png(400, 300);
        let first = decoded.thumbnail(1, &shot, (120, 60)).expect("pixels");
        assert_eq!((first.width, first.height), (80, 60));
        let again = decoded.thumbnail(1, &shot, (120, 60)).expect("pixels");
        assert!(Arc::ptr_eq(&first, &again), "nothing was shrunk twice");
        let bigger = decoded.thumbnail(1, &shot, (400, 240)).expect("pixels");
        assert_eq!((bigger.width, bigger.height), (320, 240));
        let whole = decoded.png(1, &shot).expect("pixels");
        assert_eq!(
            (whole.width, whole.height),
            (400, 300),
            "and the picture itself is still what a frame measures"
        );
        assert_eq!(decoded.kept.borrow().len(), 3, "one whole, two boxes");
    }

    #[test]
    fn a_picture_inside_its_cells_is_not_blown_up_to_fill_them() {
        let decoded = Decoded::default();
        let small = decoded.thumbnail(1, &png(4, 4), (120, 60)).expect("pixels");
        assert_eq!((small.width, small.height), (4, 4));
    }

    #[test]
    fn no_more_pictures_are_held_than_the_terminal_holds() {
        let decoded = Decoded::default();
        for id in 0..KEPT as u32 + 4 {
            decoded.png(id + 1, &png(2, 2));
            decoded.thumbnail(id + 1, &png(2, 2), (10, 10));
        }
        assert_eq!(decoded.kept.borrow().len(), ANSWERS);
        assert!(
            decoded
                .known(Asked {
                    id: 1,
                    within: None
                })
                .is_none(),
            "the oldest were let go"
        );
        assert!(
            decoded
                .known(Asked {
                    id: KEPT as u32 + 4,
                    within: None
                })
                .is_some()
        );
    }
}
