//! A room's four verbs, with an agent on the other side of them (ADR-0021,
//! ADR-0053). `OpenRoom` opens one for a purpose; `Seat` and `Unseat` move the
//! roster of one that stands; `CloseRoom` ends it. Each is a module of its own
//! — one verb, one file — and what they share is here: the block a person
//! reads afterwards, and the shape of a refusal.
//!
//! Which room a call names and whether it is the caller's to change is the
//! door's (`crate::door`), and what a verb does to a roster is `seat`'s. A tool
//! here is the schema, the card and the receipt, and nothing else.

mod close;
mod join;
mod leave;
mod open;

pub use close::CloseRoomTool;
pub use join::SeatTool;
pub use leave::UnseatTool;
pub use open::OpenRoomTool;

use bingo_sdk::{ErrorCode, KernelError, Tone, ToolError, TreeNode, View};

use crate::ear::{self, Seat};

/// A refusal in the terms the model can act on: an input it can correct, or a
/// host that failed under it.
fn refused(error: KernelError) -> ToolError {
    match error.code {
        ErrorCode::InvalidInput => ToolError::InvalidInput(error.message),
        _ => ToolError::Failed(error.message),
    }
}

/// The room a call left, as a person reads it (ADR-0013, the block lane): the
/// room, badged where the call did something to the room itself, and the seats
/// it now has under it.
fn block(title: &str, badge: Option<&str>, seats: &[Seat]) -> View {
    View::Tree {
        nodes: vec![TreeNode {
            label: title.to_string(),
            badge: badge.map(str::to_string),
            tone: Tone::Neutral,
            children: ear::nodes(seats),
        }],
    }
}

/// The same, for the three verbs that leave a room standing.
fn seated(title: &str, seats: &[Seat]) -> View {
    block(title, None, seats)
}

/// What a call that names nobody to seat or unseat is refused with: an empty
/// roster move reads exactly like one that worked, so it is not run.
fn nobody(verb: &str) -> KernelError {
    KernelError::new(
        ErrorCode::InvalidInput,
        format!("name at least one member to {verb}"),
    )
}
