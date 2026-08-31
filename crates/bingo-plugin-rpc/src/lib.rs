//! The cross-process plugin bridge (ADR-0015): a `plugin.json` under
//! `plugins/<name>/` spawns a process, and that process ships bingo-native
//! tools and commands in whatever language it likes.
//!
//! The wire is the sdk's own types as JSON over JSON-RPC 2.0, one message per
//! line, on the child's stdin and stdout. `schema/plugin.json` is that
//! contract written down, generated from [`wire`] and [`manifest`] — a plugin
//! author who cannot read Rust reads that file and nothing else.

pub mod manifest;
pub mod schema;
pub mod wire;

pub use manifest::{Entry, Manifest};
pub use wire::PROTOCOL;
