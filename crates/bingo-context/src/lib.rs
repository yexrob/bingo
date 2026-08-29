//! Context: what the model is told about the project, and what happens when
//! the conversation outgrows the window.
//!
//! The kernel owns the ruler — the thresholds, the acceptance rule and the
//! breaker — and this plugin owns the strategy: what a summary says, which
//! files reach the prompt, and what a working turn leaves behind (ADR-0006).

mod compact;
mod estimate;
mod files;
mod hook;
mod instructions;
mod memory;
mod prompt;
mod root;
mod split;
mod stream;
mod tail;
mod transcript;

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod git;
#[cfg(test)]
mod query;
#[cfg(test)]
mod scripted;

pub use compact::SummaryCompactor;
pub use hook::MemoryHook;
pub use instructions::InstructionsContributor;
pub use memory::MemoryContributor;
