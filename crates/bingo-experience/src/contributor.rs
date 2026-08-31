//! The two ways the library reaches the prompt (ADR-0014 §6): an index of
//! what there is, resident in the system prompt, and a recall of what fits
//! what was just said, appended after the person's turn.

mod index;
mod recall;

pub use index::IndexContributor;
pub use recall::RecallContributor;
