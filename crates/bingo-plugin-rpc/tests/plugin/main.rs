//! What the bridge does against a real plugin process.
//!
//! The process is this crate's `stub_plugin` example: `cargo test` builds a
//! crate's examples, so the binary is always beside the test binary
//! (`target/<profile>/examples/stub_plugin` next to `target/<profile>/deps/`),
//! with no build script and no second manifest. Everything here discovers it
//! the way a person's `plugins/` directory would be discovered.
//!
//! One module per thing that crosses: the harness they share, then tools and
//! commands, context and compaction, the model, and what a process that
//! refuses or dies looks like from this side.

// An integration test is not `cfg(test)`; the test-only lint relief is spelled out.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod harness;

mod context;
mod hooks;
mod lifecycle;
mod provider;
mod service;
mod tools;
