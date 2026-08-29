//! The kernel: session actor and journal, turn state machine, permission
//! gate, tool executor, plugin host. It knows no feature nouns; everything
//! it runs is a plugin registered through `bingo_sdk`.

pub mod accumulator;
pub mod context;
pub mod executor;
pub mod gate;
pub mod host;
pub mod journal;
pub mod models;
pub mod prompt;
pub mod session;
pub mod settings;
pub mod turn;

#[cfg(test)]
pub(crate) mod test_support;

pub use host::{Host, HostConfig, HostError, PluginStatus, Registry};
pub use journal::MemoryStore;
pub use session::{Mailbox, spawn};
pub use turn::{TurnBudget, TurnConfig};
