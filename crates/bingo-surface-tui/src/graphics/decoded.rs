//! The pictures this surface has turned into pixels, kept so it does it once.
//!
//! A frame has to know how many pixels a picture is before it can say how many
//! cells it takes, and finding that out means decoding it (`bingo-pictures`).
//! That is far too much work to do at every draw, so the answer is kept here —
//! including the answer "no decoder read it", which would otherwise be paid for
//! over and over by the one picture that cannot be drawn.
//!
//! It is a memo of work already done, not a second copy of anything: the
//! picture itself stays in the item the reducer folded, and this holds no more
//! of them than the terminal itself is holding ([`super::stored`]).

use std::cell::RefCell;
use std::sync::Arc;

use bingo_pictures::Png;
use bingo_sdk::Image;

use super::stored::KEPT;

/// Pictures as pixels, by the id the terminal knows them under, oldest first.
///
/// The cell is inside rather than around it because this is a memo: a draw
/// holds it by shared reference, the way the frame holds everything else it
/// derives ([`crate::ui::Painted`]).
#[derive(Debug, Default)]
pub struct Decoded {
    kept: RefCell<Vec<(u32, Option<Arc<Png>>)>>,
}

impl Decoded {
    /// This picture as pixels, decoded at most once. `None` where no decoder
    /// read it — which is the degrade of design §5, not an error to report.
    pub fn png(&self, id: u32, image: &Image) -> Option<Arc<Png>> {
        if let Some(known) = self.known(id) {
            return known;
        }
        let png = bingo_pictures::to_png(image)
            .inspect_err(|e| tracing::debug!(%id, error = %e, "a picture no decoder read"))
            .ok()
            .map(Arc::new);
        self.keep(id, png.clone());
        png
    }

    /// What is already known about this id, and nothing about whether the
    /// answer was a picture: `Some(None)` is a picture that would not decode
    /// last time and will not decode now.
    fn known(&self, id: u32) -> Option<Option<Arc<Png>>> {
        let kept = self.kept.borrow();
        let (_, png) = kept.iter().find(|(kept, _)| *kept == id)?;
        Some(png.clone())
    }

    fn keep(&self, id: u32, png: Option<Arc<Png>>) {
        let mut kept = self.kept.borrow_mut();
        kept.push((id, png));
        let over = kept.len().saturating_sub(KEPT);
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
        assert_eq!(decoded.kept.borrow().len(), 1, "asked once, kept once");
    }

    #[test]
    fn no_more_pictures_are_held_than_the_terminal_holds() {
        let decoded = Decoded::default();
        for id in 0..KEPT as u32 + 4 {
            decoded.png(id + 1, &png(2, 2));
        }
        assert_eq!(decoded.kept.borrow().len(), KEPT);
        assert!(decoded.known(1).is_none(), "the oldest were let go");
        assert!(decoded.known(KEPT as u32 + 4).is_some());
    }
}
