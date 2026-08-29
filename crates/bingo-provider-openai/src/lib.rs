//! The OpenAI Responses API as a `Provider` plugin.
//!
//! Everything below `lib.rs` is pure — the endpoint variants, the reasoning
//! effort table, SSE framing, error classification, the catalogue reader — so
//! the wire format is pinned by fixtures and snapshots rather than by a live
//! endpoint.

pub mod effort;
pub mod error;
pub mod models;
pub mod sse;
pub mod variant;
