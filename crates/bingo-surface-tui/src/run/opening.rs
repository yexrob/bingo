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
