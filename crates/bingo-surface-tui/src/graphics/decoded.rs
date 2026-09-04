//! The pictures this surface has measured, and the pixels it has fitted to
//! their cells.
//!
//! Two questions, and they cost very different things.
//!
//! **How many pixels is it** a frame has to answer before it can say how many
//! cells a picture takes, and how many cells it takes is where every row under
//! it goes. So the answer must be in hand *now*, or a transcript would reflow
//! as its pictures landed. It is: a picture's size is in its own header
//! ([`bingo_pictures::size`]), so measuring is not decoding, and the answer is
//! kept here so it is read once.
//!
//! **What are the pixels of this rectangle of cells** is what goes over the
//! wire, and it is a decode and a resize — hundreds of milliseconds for a
//! screenshot, which is a third of a second in which no key is read. So it is
//! never done on the caller's thread (M61). A caller is answered with what is
//! in hand — the pixels, *not yet*, or "no decoder will ever read this" — and a
//! *not yet* is left on [`Decoded::owed`] for the run to do off the loop
//! ([`Fit::fitted`]) and hand back ([`Decoded::answered`]).
//!
//! It is a memo of work already done, not a second copy of anything: the
//! picture itself stays where it came from, and this holds no more pictures
//! than the terminal itself is holding ([`super::stored`]).

use std::cell::RefCell;
use std::sync::Arc;

use bingo_pictures::Png;
use bingo_sdk::Image;

use super::stored::KEPT;

/// What is known, now, about the pixels of one rectangle of cells.
#[derive(Clone, Debug)]
pub enum Pixels {
    /// Fitted: these are the bytes the terminal is given.
    Ready(Arc<Png>),
    /// On its way. The run has the work, and the frame after its reply finds
    /// the pixels here.
    NotYet,
    /// No decoder read the picture, and none ever will.
    Never,
}

/// One picture's number and how many pixels it is — `None` where no decoder
/// recognised the bytes at all.
type Measured = (u32, Option<(u32, u32)>);

/// Which rectangle of which picture an answer is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Asked {
    id: u32,
    /// The pixels the cells hold.
    within: (u32, u32),
}

/// One picture to fit to its cells, off the loop's thread.
///
/// It carries the picture rather than borrowing it, because it outlives the
/// frame that asked: one copy of the payload per rectangle, against a decode
/// that costs a hundred times as much.
#[derive(Debug)]
pub struct Fit {
    asked: Asked,
    image: Image,
}

/// What one fitting came back with.
#[derive(Debug)]
pub struct Fitted {
    asked: Asked,
    /// The pixels, or `None` where no decoder read the picture.
    png: Option<Arc<Png>>,
}

impl Fit {
    /// The pixels. **This is the expensive call** — a decode and a resize — and
    /// it belongs on a blocking thread, never on one that draws.
    pub fn fitted(self) -> Fitted {
        let png = bingo_pictures::fitted(&self.image, self.asked.within)
            .inspect_err(
                |e| tracing::debug!(id = self.asked.id, error = %e, "a picture no decoder read"),
            )
            .ok()
            .map(Arc::new);
        Fitted {
            asked: self.asked,
            png,
        }
    }
}

/// How many rectangles are kept: two for every picture the terminal holds,
/// because a fold opening under one asks for a second.
const RECTANGLES: usize = 2 * KEPT;

/// Where one rectangle has got to.
#[derive(Debug)]
enum State {
    /// Asked for, off this thread, and not answered yet.
    Coming,
    Pixels(Arc<Png>),
    /// No decoder read the picture it belongs to.
    Never,
}

/// Pictures as sizes, and rectangles of them as pixels.
///
/// The cells are inside rather than around it because this is a memo: a draw
/// holds it by shared reference, the way the frame holds everything else it
/// derives ([`crate::ui::Painted`]).
#[derive(Debug, Default)]
pub struct Decoded {
    /// How many pixels each picture is, by its number, oldest first. `None`
    /// where no decoder recognises the bytes — kept as such, so the one
    /// picture that cannot be drawn is not measured again on every frame.
    measured: RefCell<Vec<Measured>>,
    /// Where each rectangle asked about has got to, oldest first.
    rectangles: RefCell<Vec<(Asked, State)>>,
    /// The fittings a frame has asked for that nobody has been handed yet.
    owed: RefCell<Vec<Fit>>,
}

impl Decoded {
    /// How many pixels this picture is, measured at most once. `None` where no
    /// decoder read it — which is the degrade of design §5, not an error to
    /// report.
    pub fn size(&self, id: u32, image: &Image) -> Option<(u32, u32)> {
        if let Some(known) = self.known(id) {
            return known;
        }
        let size = bingo_pictures::size(image);
        let mut measured = self.measured.borrow_mut();
        measured.push((id, size));
        let over = measured.len().saturating_sub(KEPT);
        measured.drain(..over);
        size
    }

    /// The pixels of the rectangle of cells a picture was drawn into: what goes
    /// over the wire, so a screenshot in a twelve-row block costs the block's
    /// kilobytes and not the screenshot's.
    ///
    /// Asking is taking. The first ask for a rectangle puts the work on the
    /// run's list and every ask after it is answered *not yet*, so thirty
    /// frames a second cost one fit.
    pub fn pixels(&self, id: u32, image: &Image, within: (u32, u32)) -> Pixels {
        let asked = Asked { id, within };
        match self.state(asked) {
            Some(pixels) => pixels,
            None => self.ask(asked, image),
        }
    }

    /// The fittings a frame asked for, for the run to do off its own thread.
    /// Handing them over is taking them: what was asked for once is done once.
    pub fn owed(&self) -> Vec<Fit> {
        std::mem::take(&mut self.owed.borrow_mut())
    }

    /// What one fitting came back with. An answer to a rectangle nobody is
    /// waiting for — the picture scrolled off and the cap let it go, the item
    /// was rewound away — is dropped where it lands: nothing may put pixels
    /// back that no frame asked for.
    pub fn answered(&self, fitted: Fitted) {
        let mut rectangles = self.rectangles.borrow_mut();
        let Some((_, state)) = rectangles
            .iter_mut()
            .find(|(asked, _)| *asked == fitted.asked)
        else {
            return;
        };
        *state = match fitted.png {
            Some(png) => State::Pixels(png),
            None => State::Never,
        };
    }

    /// What is already known about this picture's size, and nothing about
    /// whether it has one: `Some(None)` is a picture no decoder recognised.
    fn known(&self, id: u32) -> Option<Option<(u32, u32)>> {
        let measured = self.measured.borrow();
        let (_, size) = measured.iter().find(|(kept, _)| *kept == id)?;
        Some(*size)
    }

    fn state(&self, asked: Asked) -> Option<Pixels> {
        let rectangles = self.rectangles.borrow();
        let (_, state) = rectangles.iter().find(|(kept, _)| *kept == asked)?;
        Some(match state {
            State::Coming => Pixels::NotYet,
            State::Pixels(png) => Pixels::Ready(png.clone()),
            State::Never => Pixels::Never,
        })
    }

    /// Put this rectangle on the run's list, and mark it on its way.
    fn ask(&self, asked: Asked, image: &Image) -> Pixels {
        let mut rectangles = self.rectangles.borrow_mut();
        rectangles.push((asked, State::Coming));
        let over = rectangles.len().saturating_sub(RECTANGLES);
        rectangles.drain(..over);
        self.owed.borrow_mut().push(Fit {
            asked,
            image: image.clone(),
        });
        Pixels::NotYet
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_pictures::testing::{png, unreadable};

    /// The run's part, done here: every fitting the memo is owed.
    fn settle(decoded: &Decoded) {
        for fit in decoded.owed() {
            decoded.answered(fit.fitted());
        }
    }

    fn ready(pixels: &Pixels) -> Option<(u32, u32)> {
        match pixels {
            Pixels::Ready(png) => Some((png.width, png.height)),
            _ => None,
        }
    }

    #[test]
    fn a_picture_is_measured_once_and_answered_from_the_memo_after() {
        let decoded = Decoded::default();
        assert_eq!(decoded.size(1, &png(3, 4)), Some((3, 4)));
        assert_eq!(
            decoded.size(1, &png(9, 9)),
            Some((3, 4)),
            "the id is what it was asked about"
        );
        assert_eq!(decoded.measured.borrow().len(), 1);
    }

    #[test]
    fn a_picture_no_decoder_reads_is_remembered_as_such() {
        let decoded = Decoded::default();
        let broken = unreadable();
        assert_eq!(decoded.size(2, &broken), None);
        assert_eq!(decoded.size(2, &broken), None);
        assert_eq!(decoded.measured.borrow().len(), 1, "asked once, kept once");
    }

    /// The whole of brick 2: a frame is answered at once, and the pixels are
    /// the run's work. Nothing here decodes on the caller's thread — the only
    /// call that reaches a decoder is [`Fit::fitted`].
    #[test]
    fn a_rectangle_is_not_yet_until_the_run_has_fitted_it() {
        let decoded = Decoded::default();
        let shot = png(400, 300);
        assert!(matches!(
            decoded.pixels(1, &shot, (120, 60)),
            Pixels::NotYet
        ));
        assert!(
            matches!(decoded.pixels(1, &shot, (120, 60)), Pixels::NotYet),
            "and asking again is not a second fit"
        );
        assert_eq!(decoded.owed().len(), 1, "one in flight for one rectangle");
        assert!(decoded.owed().is_empty(), "handed over is taken");
    }

    /// The wire's question, once the run has answered it: the picture at the
    /// size of its cells, fitted once, answered from the memo after — and
    /// never blown up.
    #[test]
    fn a_picture_is_fitted_to_its_cells_once_and_answered_from_the_memo_after() {
        let decoded = Decoded::default();
        let shot = png(400, 300);
        decoded.pixels(1, &shot, (120, 60));
        settle(&decoded);
        let first = decoded.pixels(1, &shot, (120, 60));
        assert_eq!(ready(&first), Some((80, 60)));
        assert!(decoded.owed().is_empty(), "nothing was asked for twice");

        decoded.pixels(1, &shot, (400, 240));
        settle(&decoded);
        assert_eq!(
            ready(&decoded.pixels(1, &shot, (400, 240))),
            Some((320, 240)),
            "a second rectangle is a second question"
        );
        assert_eq!(
            decoded.size(1, &shot),
            Some((400, 300)),
            "and the picture itself is still what a frame measures"
        );
        assert_eq!(decoded.rectangles.borrow().len(), 2);
    }

    #[test]
    fn a_picture_inside_its_cells_is_not_blown_up_to_fill_them() {
        let decoded = Decoded::default();
        let small = png(4, 4);
        decoded.pixels(1, &small, (120, 60));
        settle(&decoded);
        assert_eq!(ready(&decoded.pixels(1, &small, (120, 60))), Some((4, 4)));
    }

    /// A picture nothing can draw is answered `Never` for ever, so the sender
    /// asks once and nobody waits on it.
    #[test]
    fn a_rectangle_of_a_picture_no_decoder_reads_is_never() {
        let decoded = Decoded::default();
        let broken = unreadable();
        decoded.pixels(2, &broken, (10, 10));
        settle(&decoded);
        assert!(matches!(
            decoded.pixels(2, &broken, (10, 10)),
            Pixels::Never
        ));
        assert!(decoded.owed().is_empty(), "and it is not tried again");
    }

    /// An answer for a rectangle the cap let go, or one nobody ever asked for,
    /// is dropped where it lands (M61's risk).
    #[test]
    fn an_answer_nobody_is_waiting_for_is_dropped() {
        let decoded = Decoded::default();
        let shot = png(40, 30);
        decoded.pixels(1, &shot, (20, 20));
        let mut owed = decoded.owed();
        let fit = owed.pop().expect("one fitting");
        assert!(owed.is_empty());
        decoded.rectangles.borrow_mut().clear();
        decoded.answered(fit.fitted());
        assert!(
            decoded.rectangles.borrow().is_empty(),
            "the answer put nothing back"
        );
    }

    #[test]
    fn no_more_is_held_than_the_terminal_holds() {
        let decoded = Decoded::default();
        for id in 1..=KEPT as u32 + 4 {
            decoded.size(id, &png(2, 2));
            decoded.pixels(id, &png(2, 2), (10, 10));
            decoded.pixels(id, &png(2, 2), (20, 20));
        }
        assert_eq!(decoded.measured.borrow().len(), KEPT);
        assert_eq!(decoded.rectangles.borrow().len(), RECTANGLES);
        assert!(decoded.known(1).is_none(), "the oldest were let go");
        assert!(decoded.known(KEPT as u32 + 4).is_some());
    }
}
