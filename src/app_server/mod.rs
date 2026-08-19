//! `bingo app-server`: the JSON frontend.
//!
//! One JSON-RPC 2.0 message per line on stdin/stdout, stderr for diagnostics
//! only. B1 lands the contract and the schema bundle that publishes it; the
//! stdio loop that serves it arrives in B6.

pub mod protocol;
pub mod schema;
pub mod session;
pub mod stdio;

use crate::error::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum AppServerError {
    /// A client line ran past the negotiated ceiling. The stream cannot be
    /// framed past it, so the connection ends rather than guessing where the
    /// next frame begins.
    #[error("a client frame exceeded the {limit}-byte ceiling")]
    FrameTooLarge { limit: usize },
    /// Bounded backpressure and the write timeout both ran out. The transport is
    /// already unusable; the notice sent before this is best-effort only.
    #[error("the client is not reading fast enough to stay attached")]
    ClientTooSlow,
    /// stdin stopped being UTF-8, or stdout stopped accepting frames.
    #[error("the app-server stream cannot be framed: {detail}")]
    Framing { detail: String },
    /// The client and this build did not agree on how to talk. The refusal was
    /// written before this; the connection cannot continue, because the two
    /// non-recoverable initialization failures are exactly the ones a client
    /// cannot usefully retry on the same connection.
    #[error("initialization failed: {}", .kind.message())]
    Initialization {
        kind: crate::app_server::protocol::error::ProtocolErrorKind,
    },
    /// Where bingo keeps its state, or where the process is standing, could not
    /// be resolved. Nothing was served.
    #[error("cannot resolve where bingo keeps its state: {detail}")]
    Bootstrap { detail: String },
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
            Self::FrameTooLarge { .. } => crate::error::FRAME_TOO_LARGE,
            Self::ClientTooSlow => crate::error::CLIENT_TOO_SLOW,
            Self::Framing { .. } => crate::error::TRANSPORT_FAILED,
            Self::Initialization { kind } => kind.bingo_code(),
            Self::Bootstrap { .. } => "CONFIG_INVALID",
            Self::SchemaConflict { .. } => "SERVER_ERROR",
            Self::Output { .. } | Self::Json(_) => "STORAGE_ERROR",
        }
    }
}
