//! The two slash commands over the store (ADR-0019 §6, §8): the table of
//! everything that fires later, and the one wake the model set on this
//! session.

mod schedule;
mod wake;

pub use schedule::ScheduleCommand;
pub use wake::WakeCommand;
