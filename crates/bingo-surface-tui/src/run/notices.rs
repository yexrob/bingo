//! What this surface has to say the moment it opens.
//!
//! These are the things a run learned before it had a screen to say them on:
//! the probes ran before the terminal was taken, and what they found has to
//! reach a person somehow. They are raised here, once, from the one place
//! that opens a run — never from a draw, which would raise them again on
//! every frame.
//!
//! These are functions over the run rather than more of its methods: `Run`'s
//! own `impl` is spread as far as it may be (`scripts/check_discipline.sh` §5).

use std::time::Instant;

use bingo_sdk::Level;

use crate::ui::Ui;

/// Everything the run opens with. Today that is what tmux has to be told
/// when the pictures could not reach the terminal behind it (M49 brick 3);
/// a terminal that simply draws none says nothing, because there is nothing
/// a person could do about it.
pub(super) fn raise(ui: &mut Ui, now: Instant) {
    if let Some(text) = crate::graphics::notice() {
        ui.notify(Level::Info, text, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics;

    fn opened(f: impl FnOnce(&mut Ui)) -> Ui {
        let mut ui = Ui::new(Vec::new(), Instant::now());
        f(&mut ui);
        ui
    }

    #[test]
    fn what_the_probe_found_is_said_once_when_the_run_opens() {
        let ui = graphics::saying(graphics::PASSTHROUGH_UNHEARD, || {
            opened(|ui| raise(ui, Instant::now()))
        });
        let said = ui.notice().expect("a notice");
        assert_eq!(said.text, graphics::PASSTHROUGH_UNHEARD);
        assert_eq!(said.level, Level::Info);
    }

    #[test]
    fn a_run_with_nothing_to_report_opens_in_silence() {
        let ui = graphics::with(graphics::drawing(), || {
            opened(|ui| raise(ui, Instant::now()))
        });
        assert!(ui.notice().is_none());
    }
}
