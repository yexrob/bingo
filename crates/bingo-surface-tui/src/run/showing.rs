//! The pictures a frame drew, between that frame and the next.
//!
//! A frame draws placeholder cells and says which picture each of them stands
//! for. Everything after that is here: the bytes the terminal is given for a
//! picture it has not got, the fittings the run owes for the ones whose pixels
//! are not in hand yet, the destinations an answer's words named that nobody
//! has been sent after, the file a click is handed, and the memory the terminal
//! gets back on the way out.
//!
//! Nothing here draws, and nothing a frame waits for is done here. The two
//! expensive things a picture costs — reading it in and fitting it to its
//! cells — are both mailed out to tasks of their own and folded back when they
//! land, so a screenshot never costs a keystroke (M51, M61).
//!
//! These are functions over the run rather than more of its methods: `Run`'s
//! own `impl` is spread as far as it may be (`scripts/check_discipline.sh` §5),
//! and where its pictures go is a chapter of its own either way.

use std::time::Instant;

use bingo_sdk::{ErrorCode, KernelError, Level, SessionState};
use serde_json::Value;

use super::{Reply, Run, stdio};
use crate::graphics::picture::Source;
use crate::graphics::{Cell, Graphics, Picture, Pixels, Stored, Transport, linked};
use crate::terminal::Screen;
use crate::ui::Ui;
use crate::viewer;

/// How many days a fetched picture is kept, as the run was told
/// (`pictures.cacheDays`, read off the settings layers before any host) — and
/// the cache's own fortnight where it was told nothing.
pub(super) fn cache_days(args: &Value) -> u64 {
    args.get("pictureCacheDays")
        .and_then(Value::as_u64)
        .unwrap_or(bingo_pictures::cache::DAYS)
}

/// The bytes that make the terminal hold the pictures this frame placed, cut
/// to the cells each of them covers.
///
/// A picture the terminal has not got is resolved back to where it came from —
/// the journal, the composer's own held pictures, or a destination the words
/// named — and its cells' pixels asked of the memo
/// ([`crate::graphics::Decoded`]); one it already has costs nothing at all. A
/// *not yet* is a picture this frame does not send and does not disturb.
fn placing(
    ui: &Ui,
    state: &SessionState,
    cell: Cell,
    stored: &mut Stored,
    placed: &[Picture],
    transport: Transport,
) -> Vec<u8> {
    let pixels = |picture: &Picture| {
        match picture.source.image_in(state, &ui.pictures, &ui.linked) {
            Some(image) => ui.decoded.pixels(picture.id(), image, picture.pixels(cell)),
            // Not where it was drawn from any more: a rewind took the item, or
            // the draft that held it is gone. Nothing will ever draw it.
            None => Pixels::Never,
        }
    };
    stored.catch_up(placed, pixels, transport)
}

/// The pictures the frame drew placeholders for, after the frame: the
/// cells are on the screen and these are what the terminal draws into
/// them, so they go out of band as the title and the clipboard do.
///
/// A terminal that draws no pictures is asked for none, and neither is a
/// frame whose pictures the terminal is already holding: the whole cost
/// of a redraw is one walk of the blocks.
pub(super) fn hand(run: &mut Run, screen: &mut dyn Screen) -> Result<(), KernelError> {
    let Graphics::Kitty { cell, transport } = crate::graphics::chosen() else {
        return Ok(());
    };
    let placed = run.ui.painted.borrow().placed();
    let state = run.session.tree.viewed();
    let bytes = placing(&run.ui, state, cell, &mut run.stored, &placed, transport);
    if bytes.is_empty() {
        return Ok(());
    }
    screen.place(&bytes).map_err(stdio)
}

/// The pictures this frame placed whose cells' pixels are not in hand
/// (M61): fitting one is a decode and a resize, hundreds of milliseconds
/// for a screenshot, so it goes to a blocking thread and comes back as a
/// reply like every other call this loop makes. One fitting per rectangle,
/// however many frames ask for it.
pub(super) fn fit(run: &mut Run) {
    for fit in run.ui.decoded.owed() {
        run.spawn(async move {
            tokio::task::spawn_blocking(move || Reply::Fitted(Box::new(fit.fitted())))
                .await
                .map_err(gave_up)
        });
    }
}

/// The pictures this frame's words named that nobody has been sent after
/// yet (M51), read in off the loop's thread — a path on this machine's
/// disk, a URL this machine fetches (ADR-0041 §3) and keeps (M61). Each
/// destination is asked for once a session, whatever the answer was, so a
/// transcript of them costs one read each and a redraw costs none.
pub(super) fn read_linked(run: &mut Run) {
    // A terminal that draws no picture is sent after none. The chip is
    // the whole of what it will ever show, so a file read or an address
    // fetched for it would be bytes taken and thrown away — and model
    // text is not a reason to reach the network on its own.
    if crate::graphics::chosen() == Graphics::Off {
        return;
    }
    let wanted = run.ui.painted.borrow().blocks.wanted();
    let cwd = std::path::PathBuf::from(&run.session.tree.viewed().summary.cwd);
    let reads = run.ui.linked.take_all(wanted, &cwd, crate::paths::home());
    for (dest, source) in reads {
        let cache = run.cache.clone();
        run.spawn(async move {
            Ok(Reply::Linked(Box::new(
                linked::read(dest, source, cache).await,
            )))
        });
    }
}

/// Give the terminal its memory back on the way out: a picture it is
/// holding for this surface outlives the run otherwise, and nothing will
/// ever place it again.
pub(super) fn forget(run: &mut Run, screen: &mut dyn Screen) -> Result<(), KernelError> {
    let transport = crate::graphics::chosen().transport();
    let bytes = run.stored.forget_all(transport);
    if bytes.is_empty() {
        return Ok(());
    }
    screen.place(&bytes).map_err(stdio)
}

/// A click on a drawn picture hands it to whatever this system opens pictures
/// with (M56) — always a file on this machine, never an address — and the
/// notice says what was opened, or, when nothing would, what it was handed.
pub(super) fn open(run: &mut Run, source: &Source) {
    let opened = handed_over(run, source);
    let now = Instant::now();
    match opened {
        Ok(word) => run.ui.notify(Level::Info, format!("opened {word}"), now),
        Err(why) => run.ui.notify(Level::Warn, why, now),
    }
}

/// The picture behind the click, read out of wherever it came from and
/// handed over as a file ([`viewer`]).
fn handed_over(run: &Run, source: &Source) -> Result<String, String> {
    let state = run.session.tree.viewed();
    let image = source.image_in(state, &run.ui.pictures, &run.ui.linked);
    viewer::open(
        &run.opener,
        source,
        image,
        viewer::Where {
            cwd: std::path::Path::new(&state.summary.cwd),
            home: crate::paths::home(),
            data_dir: &run.data_dir,
        },
    )
}

/// A fitting whose thread died under it. Nothing a person did caused it and
/// nothing they can do fixes it, so it goes the way every other failed call
/// goes — a notice, and the picture stays a rectangle of empty cells.
fn gave_up(error: tokio::task::JoinError) -> KernelError {
    KernelError::new(
        ErrorCode::Internal,
        format!("a picture could not be fitted to its cells: {error}"),
    )
}
