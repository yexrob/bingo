//! What a search backend is: a query in, the results out. One trait, so the
//! keyless default and a keyed service are the same thing to the tool.

use std::fmt;

use async_trait::async_trait;
use bingo_sdk::ToolError;

/// One result, as every backend reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// `Debug` so a backend can be named in a diagnostic — and so implementing one
/// is a decision about what of it may be printed. A key is not.
#[async_trait]
pub trait SearchBackend: fmt::Debug + Send + Sync {
    /// The results for a query, best first. Filtering and counting are the
    /// tool's, not the backend's.
    async fn search(&self, query: &str) -> Result<Vec<Hit>, ToolError>;
}
