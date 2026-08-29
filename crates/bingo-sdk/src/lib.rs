//! Stable API that plugins and clients compile against.
//!
//! The kernel (`bingo-core`) is one consumer of these types among many; every
//! plugin crate and every surface depends on this crate alone (ADR-0001).
//! One frame type crosses kernel → client; two pure reducers derive the
//! client view and the provider context from the same journal (ADR-0002).

pub mod command;
pub mod compactor;
pub mod contributor;
pub mod error;
pub mod event;
pub mod hook;
pub mod host;
pub mod ids;
pub mod model;
pub mod plugin;
pub mod policy;
pub mod provider;
pub mod state;
pub mod store;
pub mod surface;
pub mod tokens;
pub mod tool;

pub use command::{
    ArgSpec, Command, CommandContext, CommandOutcome, CommandSpec, Completion, View,
};
pub use compactor::{CompactContext, CompactReason, Compaction, Compactor};
pub use contributor::{ContextContributor, ContextError, ContextPiece, ContextQuery, Placement};
pub use error::{ErrorCode, KernelError};
pub use event::*;
pub use hook::{Hook, HookContext, HookMatcher, HookOutcome, HookPoint, Phase};
pub use host::*;
pub use ids::*;
pub use model::*;
pub use plugin::{
    CommandSource, ConfigClaim, Contribution, Merge, Plugin, PluginError, PluginManifest,
    Registrar, ToolSource,
};
pub use policy::{Decision, PermissionPolicy, PolicyInput, Reason, Verdict};
pub use provider::{AuthStatus, ModelInfo, Provider};
pub use state::{Applied, LiveTurn, Retry, SessionState};
pub use store::SessionStore;
pub use surface::{Exit, Surface, SurfaceKind, SurfaceOptions};
pub use tool::{
    Delivery, Env, Interrupt, ResultLimit, Subject, Tool, ToolCall, ToolContext, ToolError,
    ToolHost, ToolTraits, input_schema,
};

/// Re-exported so plugins share one cancellation type without naming tokio-util.
pub use tokio_util::sync::CancellationToken;
