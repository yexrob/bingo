//! What a key press asks the loop to do. `on_key` is pure over `Ui`; every
//! call that reaches the kernel or the terminal leaves as one of these, so a
//! key table is a test with no runtime in it.

use bingo_sdk::{Activation, Answer, Input, InteractionId, SessionSelector};

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Submit(Input),
    Interrupt,
    Answer {
        interaction: InteractionId,
        answer: Answer,
        activation: Activation,
    },
    /// Attach to another session; the loop closes the old attachment first.
    Open(SessionSelector),
    /// Fill the session picker from the host.
    ListSessions,
    Exit,
}
