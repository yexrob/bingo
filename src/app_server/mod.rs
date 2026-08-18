//! `bingo app-server`: the JSON frontend.
//!
//! One JSON-RPC 2.0 message per line on stdin/stdout, stderr for diagnostics
//! only. B1 lands the contract and the schema bundle that publishes it; the
//! stdio loop that serves it arrives in B6.

pub mod protocol;
pub mod schema;

use crate::error::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum AppServerError {
    /// The transport is not implemented yet. Named rather than silently
    /// accepting a connection it cannot serve.
    #[error(
        "app-server serve mode lands in a later batch; run `bingo app-server generate-schema --out <dir>`"
    )]
    ServeUnavailable,
    #[error("cannot write the schema bundle to {path}: {source}")]
    Output {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Two schemas define the same name with different shapes, so the shared
    /// definitions file cannot hold both. Names come from Rust type names, so
    /// this means two types in the contract are called the same thing.
    #[error("two app-server schemas define {name} differently")]
    SchemaConflict { name: String },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl ErrorCode for AppServerError {
    fn error_code(&self) -> &'static str {
        match self {
            Self::ServeUnavailable => crate::error::SLASH_ERROR_BAD_ARGUMENT,
            Self::SchemaConflict { .. } => "SERVER_ERROR",
            Self::Output { .. } | Self::Json(_) => "STORAGE_ERROR",
        }
    }
}
