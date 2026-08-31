//! Experience (ADR-0014): procedural playbooks a project accumulates —
//! *when this happens, do this, check it worked* — as hand-editable files
//! under one directory per project, ranked back into the prompt by a
//! zero-dependency BM25.
//!
//! Facts about a project are the memory extractor's; this store keeps only
//! procedure, and the two never share a corpus or a prompt block.

pub mod bm25;
mod diff;
pub mod entry;
mod frontmatter;
mod id;
mod project;
mod rank;
mod render;
pub mod store;
pub mod tools;

#[cfg(test)]
mod tests;
