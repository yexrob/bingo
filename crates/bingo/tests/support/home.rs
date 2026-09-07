//! Where a run believes its home is.
//!
//! `std::env::home_dir` reads the platform's own spelling and only that:
//! `HOME` on unix, `USERPROFILE` on Windows. A test that isolates a run by
//! naming one of them isolates nothing on the other platform — on Windows it
//! would run in the home of whoever started the suite, among their settings,
//! their credentials and their locks. So both are set, from the one path, in
//! the one place.
//!
//! Reached as `support::home` where the binary already has the JSON-RPC
//! harness, and as `#[path = ".../support/home.rs"] mod home;` where it does
//! not.

use std::path::{Path, PathBuf};

/// What a run must be told for `home` to be its home. Hand it to
/// `Command::envs`; a builder that has no `envs` loops over it.
pub fn home_env(home: impl AsRef<Path>) -> [(&'static str, PathBuf); 2] {
    let home = home.as_ref().to_path_buf();
    [("HOME", home.clone()), ("USERPROFILE", home)]
}
