//! Stable API that plugins and clients compile against.
//!
//! The kernel (`bingo-core`) is one consumer of these types among many; every
//! plugin crate and every surface depends on this crate alone (ADR-0001).
//! One frame type crosses kernel → client; two pure reducers derive the
//! client view and the provider context from the same journal (ADR-0002).

pub mod error;
pub mod event;
pub mod ids;
pub mod model;
pub mod state;

pub use error::{ErrorCode, KernelError};
pub use event::*;
pub use ids::*;
pub use model::*;
pub use state::{Applied, SessionState};
