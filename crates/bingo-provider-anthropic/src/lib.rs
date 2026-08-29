//! The Anthropic Messages API as a `Provider` plugin.

pub mod error;
pub mod events;
pub mod request;
pub mod sse;

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
