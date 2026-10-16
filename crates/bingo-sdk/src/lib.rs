//! Stable API that plugins and clients compile against.
//!
//! The kernel (`bingo-core`) is one consumer of these types among many; every
//! plugin crate and every surface depends on this crate alone (ADR-0001).
//! One frame type crosses kernel → client; one transcript fold supplies the
//! client view and the provider context (ADR-0002, ADR-0064).

pub mod agent;
mod ago;
mod asked;
pub use ago::{ago, until};
pub use asked::{PERSON_SURFACES, asked, effective_voice, first_ask, first_line};
pub mod bytes;
pub mod command;
pub mod compactor;
pub mod component;
pub mod contributor;
pub mod error;
pub mod event;
pub mod hook;
pub mod host;
pub mod ids;
pub mod intercept;
pub mod keys;
pub mod model;
pub mod page;
pub mod plugin;
pub mod policy;
pub mod provider;
pub mod service;
pub mod slot;
pub mod state;
pub mod store;
pub mod surface;
pub mod switch;
/// Doubles for a plugin's tests; `features = ["testing"]` in dev-dependencies.
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod tokens;
pub mod tool;
pub mod transcript;
pub mod tree;
pub mod view;

pub use agent::{
    Agent, AgentEvent, AgentStream, AgentTurn, CallKind, CallLocation, ExternalCall, Judge,
};
pub use command::{
    ArgSpec, Command, CommandContext, CommandFold, CommandOutcome, CommandSpec, KeyBinding,
    split_verb,
};
pub use compactor::{CompactContext, CompactError, CompactReason, Compaction, Compactor};
pub use contributor::{
    CONTRIBUTOR_PREFIX, ContextContributor, ContextError, ContextPiece, ContextQuery, Placement,
};
pub use error::{ErrorCode, KernelError};
pub use event::*;
pub use hook::{Hook, HookContext, HookMatcher, HookOutcome, HookPoint, Phase};
pub use host::*;
pub use ids::*;
pub use intercept::*;
pub use model::*;
pub use page::{Page, Pages};
pub use plugin::{
    BUILT_IN, CommandSource, CompactorSource, ConfigClaim, ContextSource, Contribution, HookSource,
    Merge, Plugin, PluginDecision, PluginError, PluginExecution, PluginGrant, PluginLifecycle,
    PluginManifest, PluginOrigin, PluginPlacement, PluginRegistration, PluginReload, PluginRuntime,
    PluginSource, PluginState, PluginStatus, ProviderSource, Registrar, SWITCHED_OFF, ToolSource,
};
pub use policy::{Decision, PermissionPolicy, PolicyInput, Reason, Stance, Verdict};
pub use provider::{
    AuthStatus, InstanceSpec, KeySource, LoginMethod, ModelInfo, Provider, ProviderShape,
    valid_provider_name,
};
/// `Service` and `Services` stay behind `service::`: the kernel has a bundle
/// of its own by that name, and a glob import must not make either ambiguous.
pub use service::{Face, ServiceError, ServiceHandle, ServiceKey, WireService};
pub use state::{Applied, LastTurn, LiveTurn, Retry, SessionState};
pub use store::{Checkpoint, Resumed, SessionStore};
pub use surface::{Exit, Surface, SurfaceKind, SurfaceOptions};
pub use tool::{
    Env, ResultLimit, Subject, Tool, ToolCall, ToolContext, ToolError, ToolHost, ToolOutcome,
    ToolTraits, input_schema,
};
pub use transcript::Transcript;
pub use tree::TreeState;
pub use view::{ActionItem, Tone, TreeNode, View};

/// Re-exported so plugins share one cancellation type without naming tokio-util.
pub use tokio_util::sync::CancellationToken;

pub mod catalogue;

pub mod generation;
pub mod invocation;

pub mod render;

mod approval;
pub use approval::{APPROVALS, Approval, ApprovalReader, Waiting};

pub mod spent;
