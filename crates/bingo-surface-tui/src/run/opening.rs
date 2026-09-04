//! The opening, wired: whether it plays, and when it started.
//!
//! A frame of the piece is a pure function of the second it is for and it costs
//! microseconds, so it is drawn *in* the draw ([`crate::opening`]) — there is
//! nothing to render off the loop's thread and nothing to hold in step. All the
//! run keeps is the instant the piece began.
//!
//! These are functions over the run rather than more of its methods: `Run`'s
//! own `impl` is spread as far as it may be (`scripts/check_discipline.sh` §5).

use super::Run;
use crate::clock::Now;
use crate::welcome;

/// Whether this run opens with the piece, asked once when the surface has its
/// session and its screen. The rule itself is [`welcome::opens`]; this is only
/// the reading of the world it takes.
pub(super) fn plays(run: &Run, screen: (u16, u16), asked: bool, now: Now) -> bool {
    let state = run.session.tree.root();
    welcome::opens(welcome::Opens {
        ours: welcome::wanted(state),
        fresh: state.items.is_empty(),
        asked,
        screen,
        colour: crate::theme::full_colour(),
        glyphs: crate::theme::glyphs() != &crate::theme::ASCII,
        motion: now.motion,
    })
}

/// Start it, where this run is one that opens with it.
pub(super) fn begin(run: &mut Run, screen: (u16, u16), asked: bool, now: Now) {
    if plays(run, screen, asked, now) {
        run.ui.intro = Some(crate::opening::Playing::from(now.instant));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doubles::Recorder;
    use crate::opening::Playing;
    use crate::painted::truecolor;
    use crate::run::{Wake, tests::idle, tests::idle_in};
    use crate::test_support::{assistant, folded, frame, later, scene};
    use bingo_sdk::{Event, ItemStatus};

    /// A terminal with room for the piece in it.
    const SCREEN: (u16, u16) = (80, 24);

    /// The reading of the world the rule is given. The rule's own table is
    /// [`welcome::opens`]'s; these are the four facts this run reads off the
    /// session, the argv and the screen.
    #[test]
    fn a_fresh_session_on_a_terminal_with_room_opens_with_the_piece() {
        let (_, now) = scene();
        crate::theme::with(truecolor(), || {
            let run = idle(now.instant);
            assert!(plays(&run, SCREEN, false, now));
            assert!(
                !plays(&run, SCREEN, true, now),
                "a run given work on the command line goes and does it"
            );
            assert!(
                !plays(&run, (79, 24), false, now),
                "and a narrow terminal has no room for the box to play in"
            );
            assert!(!plays(&run, (80, 15), false, now), "nor a short one");
            assert!(
                !plays(&run, SCREEN, false, crate::test_support::still(now)),
                "and a still run draws no frames at all"
            );
        });
    }

    #[test]
    fn a_session_that_already_happened_is_not_entered() {
        let (_, now) = scene();
        let resumed = folded(vec![frame(
            1,
            Event::ItemCompleted {
                item: assistant("itm_1", "we spoke before", ItemStatus::Completed),
            },
        )]);
        crate::theme::with(truecolor(), || {
            let run = idle_in(
                resumed,
                std::sync::Arc::new(crate::doubles::TestSession::default()),
                now.instant,
            );
            assert!(!plays(&run, SCREEN, false, now));
        });
    }

    #[test]
    fn a_terminal_with_eight_colours_or_only_ascii_never_sees_it() {
        let (_, now) = scene();
        for look in [crate::painted::ascii(), crate::painted::no_colour()] {
            crate::theme::with(look, || {
                let mut run = idle(now.instant);
                begin(&mut run, SCREEN, false, now);
                assert!(run.ui.intro.is_none());
            });
        }
    }

    /// The piece is drawn where every other row of the transcript is, and it
    /// reaches the terminal's own cells in the draw that started it.
    #[test]
    fn the_frame_of_the_piece_is_what_the_box_draws() {
        let (_, now) = scene();
        let mut run = idle(now.instant);
        run.ui.intro = Some(Playing::from(now.instant));
        let mut recorder = Recorder::default();
        run.paint(&mut recorder, Wake::Frame, now).expect("a frame");
        assert!(
            !recorder.last().contains("Welcome to bingo!"),
            "the first second of the piece has said nothing yet:\n{}",
            recorder.last()
        );

        run.ui.intro = Some(Playing::from(
            now.instant
                .checked_sub(std::time::Duration::from_millis(2_000))
                .expect("a clock with two seconds behind it"),
        ));
        run.painted = crate::run::older_than_a_frame();
        run.paint(&mut recorder, Wake::Frame, now).expect("a frame");
        assert!(
            recorder.last().contains("Welcome to bingo!"),
            "and by the last beat the box is whole:\n{}",
            recorder.last()
        );
    }

    /// When the piece has played out it is taken away, and what the box draws
    /// is what it has always drawn.
    #[test]
    fn the_box_comes_back_when_the_piece_has_run_out() {
        let (mut ui, now) = scene();
        ui.intro = Some(Playing::from(now.instant));
        ui.expire(later(now, 2_300));
        assert!(ui.intro.is_some(), "it is still playing");
        ui.expire(later(now, 2_400));
        assert!(ui.intro.is_none(), "and then it is over");
    }
}
