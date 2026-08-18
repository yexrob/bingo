//! The application core's contract.
//!
//! `AppCore` owns application truth and ordering; the TUI and `bingo app-server`
//! are two projections of it. This module is the vocabulary they share: the
//! identifiers the server mints ([`ids`]), the mutations it accepts
//! ([`command`]), the events it publishes ([`event`]), and the snapshots it hands
//! out ([`snapshot`]).
//!
//! B1 lands the types. The actor that mints, sequences, and serves them is B2's;
//! until then nothing here has a caller and each module carries a temporary
//! `allow(dead_code)`.
//!
//! Design: `notes/design/gui-app-server.md` (with its amendments) and
//! `notes/design/gui-app-server-plan.md`.

pub mod command;
pub mod event;
pub mod ids;
pub mod snapshot;
