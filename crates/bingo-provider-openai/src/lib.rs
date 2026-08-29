//! The OpenAI Responses API as a `Provider` plugin.
//!
//! Everything below `lib.rs` is pure — request encoding, SSE framing, the
//! event state machine, error classification, the effort table, the catalogue
//! reader — so the wire format is pinned by fixtures and snapshots rather than
//! by a live endpoint.
//!
//! Stateless by design: `store` is always `false`, so the journal stays the
//! source of truth and every turn re-sends the whole conversation, carrying
//! the model's encrypted reasoning state with it.

pub mod effort;
pub mod error;
pub mod events;
pub mod input;
pub mod models;
pub mod request;
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
