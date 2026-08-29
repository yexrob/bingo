//! The OpenAI Responses API as a `Provider` plugin.
//!
//! Everything below `lib.rs` is pure — the endpoint variants, the reasoning
//! effort table, SSE framing, the event state machine, error classification,
//! the catalogue reader — so the wire format is pinned by fixtures and
//! snapshots rather than by a live endpoint.

pub mod effort;
pub mod error;
pub mod events;
pub mod models;
pub mod sse;
pub mod stream;
pub mod variant;

#[cfg(test)]
pub(crate) mod tests {
    use std::path::PathBuf;

    /// A recorded wire body under `fixtures/`. Tests read it from the manifest
    /// directory, because a test binary's working directory is not the crate's.
    pub(crate) fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name)
    }
}
