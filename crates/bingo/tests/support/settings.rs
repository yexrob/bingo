//! A settings layer read back after a command wrote it.
//!
//! The layers are TOML (ADR-0058 §1), and a black-box test asks the file what
//! it says rather than asking the binary that wrote it — so the reader here is
//! `toml_edit`'s own, not the kernel's.
//!
//! Reached as `support::settings` where the binary already has the JSON-RPC
//! harness, and as `#[path = ".../support/settings.rs"] mod settings;` where
//! it does not.

use std::path::Path;

/// What one settings file says. It panics where there is no file: a test that
/// reads a layer back is a test that expected a command to write one.
pub fn read(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    toml_edit::de::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}
