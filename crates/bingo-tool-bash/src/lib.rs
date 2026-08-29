//! The `Bash` tool: one shell command per call, in its own process group.
//!
//! The plugin is assembled from four bricks: the tables that refuse a command
//! before anything is spawned ([`reject`]), the process lifecycle, the bounded
//! output, and the live tail. See docs/plans/M1-provider-tools-gate.md.

pub mod reject;
