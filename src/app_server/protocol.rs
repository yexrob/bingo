//! The app-server wire contract: JSON-RPC 2.0 over NDJSON on stdio.
//!
//! Four parts: the [`envelope`] every frame wears, the [`requests`] a client may
//! send, the [`notifications`] the server publishes, and the [`error`] shape
//! every failure takes. The resources they carry are the core's own
//! (`crate::app::snapshot`), so there is one conversation summary in this
//! process, not two.
//!
//! Within major 1, unknown object fields are additive and ignored. A new method,
//! notification, required field, or union variant arrives by minor version or an
//! opted-in capability; breaking semantics require a new major.
//!
//! Spec: `notes/design/gui-app-server.md`.
// The contract lands before its transport: the stdio loop (B6) is the first
// consumer. Remove this allow when it arrives.
#![allow(dead_code)]

pub mod envelope;
pub mod error;
pub mod notifications;
pub mod requests;

/// One JSON round trip for every request, result, notification, and error the
/// contract declares. Split out of this file for the same reason `query_tests.rs`
/// is: fixtures grow, contracts should not.
#[cfg(test)]
mod fixtures_tests;
