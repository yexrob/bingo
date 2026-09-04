//! A line on its way back out of the queue and into the composer.
//!
//! `↑` on a box the caret cannot climb any further asks the kernel for the
//! newest line this surface still has waiting (ADR-0008 §2, amended M68). The
//! ask is spawned like every other host call the loop makes, so no key press
//! waits on an actor; what comes back is the line itself, or the word that a
//! turn got to it first.
//!
//! These are functions over the run rather than more of its methods: `Run`'s
//! own `impl` is spread as far as it may be (`scripts/check_discipline.sh` §5).

use std::time::Instant;

use bingo_sdk::{ErrorCode, Input, IntentId, KernelError, Level};

use super::{Reply, Run, identity};

/// What the status line says when the turn took the line first. The race is
/// real and it is the actor's to settle: by the time the ask arrives the entry
/// is either still queued or already spoken, and this is the honest word for
/// the second.
pub(super) const ALREADY_SENT: &str = "already sent";

pub(super) fn ask(run: &mut Run, intent: IntentId) {
    let session = run.session.tree.view().clone();
    let host = run.host.clone();
    run.spawn(async move {
        let taken = host.withdraw(&session, &intent, identity()).await;
        Ok(Reply::Withdrawn(Box::new(taken)))
    });
}

/// The line is the person's again: its words go back in the box and the
/// pictures it carried go back under the tokens that name them, so `⏎` or
/// `tab` sends exactly what was queued.
pub(super) fn took(run: &mut Run, taken: Result<Input, KernelError>) {
    match taken {
        Ok(Input::Text { text, images, .. }) => {
            run.ui.pictures.restore(&text, images);
            run.ui.composer.set(&text);
            run.ui.edited();
        }
        // An action carries no words to edit, and nothing this surface queues
        // is one: there is nothing to put in the box for it.
        Ok(Input::Action { .. }) => {}
        Err(error) => refused(run, error),
    }
}

/// The queue would not give it up. A line a turn has taken is not something a
/// person did wrong, so it is a note and not a warning; anything else says
/// what the kernel said.
fn refused(run: &mut Run, error: KernelError) {
    let (level, message) = match error.code {
        ErrorCode::NotFound => (Level::Info, ALREADY_SENT.to_string()),
        _ => (Level::Warn, error.message),
    };
    run.ui.notify(level, message, Instant::now());
}
