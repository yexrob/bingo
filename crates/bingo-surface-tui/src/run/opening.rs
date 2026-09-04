//! The opening shot, wired: whether it plays, and where its frames come from.
//!
//! A frame of the piece is a ray-marched picture — tens of milliseconds in a
//! debug build — so no draw may ever wait for one. Each is rendered on a
//! blocking thread and comes back as a reply like every other call this loop
//! makes ([`super::showing::fit`] does the same for a picture's pixels), and
//! the draw takes whichever frame it has. The piece runs on the wall clock, so
//! a frame that is not ready is a frame that is not shown.
//!
//! These are functions over the run rather than more of its methods: `Run`'s
//! own `impl` is spread as far as it may be (`scripts/check_discipline.sh` §5).

use bingo_sdk::{ErrorCode, KernelError};
use ratatui::text::Line;

use super::{Reply, Run};
use crate::clock::Now;
use crate::terminal::Screen;
use crate::welcome;

/// One frame of the piece, back from the thread that drew it.
pub(super) struct Rendered {
    /// The width it was drawn for. A screen that has since been resized has no
    /// use for it, and says so rather than drawing a frame of the wrong shape.
    pub width: u16,
    pub rows: Vec<Line<'static>>,
}

/// Whether this run opens with the piece, asked once when the surface has its
/// session and its screen. The rule itself is [`welcome::opens`]; this is only
/// the reading of the world it takes.
pub(super) fn plays(run: &Run, screen: &dyn Screen, asked: bool, now: Now) -> bool {
    let state = run.session.tree.root();
    welcome::opens(welcome::Opens {
        ours: welcome::wanted(state),
        fresh: state.items.is_empty(),
        asked,
        screen: screen.size(),
        colour: crate::theme::full_colour(),
        glyphs: crate::theme::glyphs() != &crate::theme::ASCII,
        motion: now.motion,
    })
}

/// Start it, where this run is one that opens with it.
pub(super) fn begin(run: &mut Run, screen: &dyn Screen, asked: bool, now: Now) {
    if plays(run, screen, asked, now) {
        run.ui.intro = Some(crate::intro::Playing::from(now.instant));
    }
}

/// The next frame of the piece, asked for off the loop's thread.
///
/// One at a time: the piece is a clock, so a second request would only race the
/// first to be thrown away. A frame is asked for after the draw, as the
/// pictures are, so what it costs is never in front of anything a person did.
pub(super) fn ask(run: &mut Run, now: Now) {
    let width = run.ui.painted.borrow().regions.transcript.width;
    let boxed = resting(run, width);
    let Some(intro) = run.ui.intro.as_mut() else {
        return;
    };
    // Before the first frame has been drawn there is no transcript to measure,
    // and a frame of no width is not a frame.
    if width == 0 || !intro.wants() {
        return;
    }
    let t = intro.seconds(now);
    intro.asked();
    run.spawn(async move {
        tokio::task::spawn_blocking(move || {
            Reply::Opening(Box::new(Rendered {
                width,
                rows: crate::intro::frame(t, width, &boxed),
            }))
        })
        .await
        .map_err(gave_up)
    });
}

/// The box the piece lands on, as the session in view would be welcomed today.
/// It is drawn here rather than on the thread because it is the reducer's to
/// answer and the reducer does not leave this loop.
fn resting(run: &Run, width: u16) -> Vec<Line<'static>> {
    welcome::lines(
        run.session.tree.root(),
        usize::from(width),
        run.ui.update.as_deref(),
    )
}

/// One has come back. A frame for a piece that has already been skipped or has
/// played out is dropped where it lands.
pub(super) fn landed(run: &mut Run, frame: Rendered) {
    if let Some(intro) = run.ui.intro.as_mut() {
        intro.landed(frame.width, frame.rows);
    }
}

/// A frame whose thread died under it. Nothing a person did caused it and
/// nothing they can do fixes it; the piece simply stops where it is.
fn gave_up(error: tokio::task::JoinError) -> KernelError {
    KernelError::new(
        ErrorCode::Internal,
        format!("a frame of the opening could not be drawn: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doubles::Recorder;
    use crate::intro::Playing;
    use crate::painted::truecolor;
    use crate::run::{Wake, tests::idle, tests::idle_in};
    use crate::test_support::{assistant, folded, frame, later, scene};
    use bingo_sdk::{Event, ItemStatus};
    use tokio::sync::mpsc;

    /// A screen of a chosen size, which is the one thing about a terminal the
    /// opening asks after that a [`Recorder`] does not already answer.
    struct Sized(u16, u16);

    impl crate::terminal::Screen for Sized {
        fn draw(
            &mut self,
            _: &crate::tree::Tree,
            _: &crate::ui::Ui,
            _: Now,
        ) -> std::io::Result<()> {
            Ok(())
        }
        fn title(&mut self, _: &str) -> std::io::Result<()> {
            Ok(())
        }
        fn bell(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn notify(&mut self, _: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn copy(&mut self, _: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn place(&mut self, _: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn size(&self) -> (u16, u16) {
            (self.0, self.1)
        }
    }

    /// The reading of the world the rule is given. The rule's own table is
    /// [`welcome::opens`]'s; these are the four facts this run reads off the
    /// session, the argv and the screen.
    #[test]
    fn a_fresh_session_on_a_terminal_with_room_opens_with_the_piece() {
        let (_, now) = scene();
        crate::theme::with(truecolor(), || {
            let run = idle(now.instant);
            assert!(plays(&run, &Recorder::default(), false, now));
            assert!(
                !plays(&run, &Recorder::default(), true, now),
                "a run given work on the command line goes and does it"
            );
            assert!(
                !plays(&run, &Sized(79, 24), false, now),
                "and a narrow terminal has no room for a world"
            );
            assert!(!plays(&run, &Sized(80, 15), false, now), "nor a short one");
            assert!(
                !plays(
                    &run,
                    &Recorder::default(),
                    false,
                    crate::test_support::still(now)
                ),
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
            assert!(!plays(&run, &Recorder::default(), false, now));
        });
    }

    #[test]
    fn a_terminal_with_eight_colours_or_only_ascii_never_sees_it() {
        let (_, now) = scene();
        for look in [crate::painted::ascii(), crate::painted::no_colour()] {
            crate::theme::with(look, || {
                let mut run = idle(now.instant);
                begin(&mut run, &Recorder::default(), false, now);
                assert!(run.ui.intro.is_none());
            });
        }
    }

    /// The draw never waits: the first frame is painted before any picture has
    /// been marched, and the request goes out behind it.
    #[tokio::test]
    async fn the_box_is_painted_before_a_frame_of_the_piece_is_ready() {
        let (_, now) = scene();
        let mut run = idle(now.instant);
        run.ui.intro = Some(Playing::from(now.instant));
        let (replies, mut waiting) = mpsc::channel(4);
        run.replies = replies;
        let mut recorder = Recorder::default();

        run.paint(&mut recorder, Wake::Frame, now).expect("a frame");
        assert_eq!(recorder.frames.len(), 1, "it painted without waiting");
        let asked = waiting.recv().await.expect("a frame was asked for");
        assert!(matches!(asked, Reply::Opening(_)), "and it was the piece's");
    }

    #[tokio::test]
    async fn only_one_frame_of_the_piece_is_ever_in_flight() {
        let (_, now) = scene();
        let mut run = idle(now.instant);
        run.ui.intro = Some(Playing::from(now.instant));
        let (replies, mut waiting) = mpsc::channel(4);
        run.replies = replies;
        let mut recorder = Recorder::default();
        for at in [0, 40, 80] {
            let now = later(now, at);
            run.painted = crate::run::older_than_a_frame();
            run.paint(&mut recorder, Wake::Frame, now).expect("a frame");
        }
        assert_eq!(recorder.frames.len(), 3, "three frames were painted");
        waiting.recv().await.expect("one was asked for");
        assert!(
            waiting.try_recv().is_err(),
            "and only one, however many draws went past"
        );
    }

    /// A frame that has landed is what the welcome box draws, and it reaches
    /// the terminal's own cells.
    #[tokio::test]
    async fn the_frame_that_landed_is_what_the_box_draws() {
        let (_, now) = scene();
        let mut run = idle(now.instant);
        run.ui.intro = Some(Playing::from(now.instant));
        let mut recorder = Recorder::default();
        run.paint(&mut recorder, Wake::Frame, now).expect("a frame");
        let width = run.ui.painted.borrow().regions.transcript.width;
        assert!(width > 0, "the first draw measured the transcript");

        landed(
            &mut run,
            Rendered {
                width,
                rows: vec![Line::from("▀".repeat(usize::from(width)))],
            },
        );
        run.painted = crate::run::older_than_a_frame();
        run.paint(&mut recorder, Wake::Frame, now).expect("a frame");
        assert!(
            recorder.last().contains(&"▀".repeat(usize::from(width))),
            "the frame is on the screen:\n{}",
            recorder.last()
        );
        assert!(
            !recorder.last().contains("Welcome to bingo!"),
            "and the resting box is not:\n{}",
            recorder.last()
        );
    }

    /// A frame rendered for another width is not a frame: the box draws
    /// nothing rather than a picture of the wrong shape.
    #[tokio::test]
    async fn a_frame_from_before_a_resize_is_dropped() {
        let (_, now) = scene();
        let mut run = idle(now.instant);
        let mut playing = Playing::from(now.instant);
        playing.landed(31, vec![Line::from("stale")]);
        run.ui.intro = Some(playing);
        let mut recorder = Recorder::default();
        run.paint(&mut recorder, Wake::Frame, now).expect("a frame");
        assert!(!recorder.last().contains("stale"), "{}", recorder.last());
    }

    /// When the piece has played out it is taken away, and what the box draws
    /// is what it has always drawn.
    #[test]
    fn the_box_comes_back_when_the_piece_has_run_out() {
        let (mut ui, now) = scene();
        ui.intro = Some(Playing::from(now.instant));
        ui.expire(later(now, 3_900));
        assert!(ui.intro.is_some(), "it is still playing");
        ui.expire(later(now, 4_000));
        assert!(ui.intro.is_none(), "and then it is over");
    }
}
