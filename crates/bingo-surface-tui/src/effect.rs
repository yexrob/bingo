//! What a key press asks the loop to do. `on_key` is pure over `Ui`; every
//! call that reaches the kernel or the terminal leaves as one of these, so a
//! key table is a test with no runtime in it.

use bingo_sdk::{Activation, Answer, Input, InteractionId, SessionId, SessionSelector};

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Submit(Input),
    Interrupt,
    Answer {
        interaction: InteractionId,
        answer: Answer,
        activation: Activation,
    },
    /// Paint another session of the attached tree; the loop fetches its
    /// mailbox the first time.
    View(SessionId),
    /// Attach to another session; the loop closes the old attachment first.
    Open(SessionSelector),
    /// Fill the session picker from the host.
    ListSessions,
    /// Fill the switcher's stored rows from the host: one read per opening.
    ListStored,
    /// Put a selection on the terminal's own clipboard (OSC 52). The loop
    /// says so when the terminal will not take it.
    Copy(String),
    Exit,
}
