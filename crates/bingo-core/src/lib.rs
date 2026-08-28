//! The kernel: session actor, journal, turn state machine, permission gate,
//! executor, plugin host. It depends on `bingo-sdk` and nothing feature-shaped.

pub mod accumulator;
pub mod context;
pub mod executor;
