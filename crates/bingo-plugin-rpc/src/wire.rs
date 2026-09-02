//! The methods and the notifications a plugin process speaks (ADR-0015 §2,
//! ADR-0030 §6): their names, and the shape of what goes in and comes back.
//!
//! Every params and result type is an sdk type or a struct of sdk types. The
//! bridge adds envelopes, never shapes: `ToolSpec`, `CommandSpec`,
//! `ToolOutput`, `CommandOutcome`, `Completion`, `ContextPiece`, `Compaction`,
//! `ModelRequest`, `ModelEvent` and `ProviderError` cross verbatim, so a plugin
//! author writes against the kernel's own vocabulary. What a process may not
//! hold — the host handle a `ContextQuery` carries, the provider a
//! `CompactContext` carries — is left behind by a projection with a name of its
//! own (ADR-0030 §5).
//!
//! One lane is not a call and a reply: a model streams. `provider/stream` opens
//! it, `provider/delta` notifications carry it, and the response to the open
//! closes it. Every delta names the stream it belongs to, so two running at
//! once never interleave.
//!
//! One method travels both ways: `service/call`. The host asks a process for
//! a service that process serves; a process asks the host for any service
//! whose owner opened a wire face — and the host, being the router, is how
//! two processes pair with no pipe between them (ADR-0031 §4).
//!
//! One pair of names splits by whether anyone waits. `hook/decide` carries the
//! four points whose answer the kernel reads; `hook/observe` carries the four
//! it does not, as a notification, so watching a turn costs the turn nothing
//! (ADR-0032 §2, §3). Both name the hook the handshake declared, and the
//! matcher declared with it keeps an unmatched event off the pipe entirely.
//!
//! `METHODS` and `NOTIFICATIONS` are the one table: the schema walks it, and
//! the host and the example plugin both dispatch on the names in [`name`].
//! What the host's own service takes and answers is not in that table and not
//! here: it rides inside one `service/call`'s params, so it lives with the
//! doors that read it ([`crate::doors`]).

use std::collections::BTreeMap;
use std::path::PathBuf;

use bingo_sdk::{
    CommandOutcome, CommandSpec, CompactContext, CompactReason, Compaction, Completion,
    ContextPiece, ContextQuery, ContextUsage, EndpointCapabilities, Env, Frame, HookContext,
    HookMatcher, HookOutcome, Input, Item, ModelCapabilities, ModelEvent, ModelInfo, ModelRequest,
    Phase, Placement, ProviderError, SessionId, SessionSummary, ToolCall, ToolOutput, ToolSpec,
    TurnId,
};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The major the host speaks. A process that answers with another one is
/// refused rather than guessed at (ADR-0015 §Consequences). Five since
/// ADR-0032 opened hooks, whose declaration is read at the handshake: a plugin
/// written for one major says so and is refused, rather than being asked what
/// it cannot answer.
pub const PROTOCOL: u32 = 5;

/// Every name that travels on the wire, in one place.
pub mod name {
    /// Kernel → plugin, once, before anything else.
    pub const INITIALIZE: &str = "initialize";
    /// Kernel → plugin: run one tool call.
    pub const TOOL_CALL: &str = "tool/call";
    /// Kernel → plugin: run one `/name`.
    pub const COMMAND_RUN: &str = "command/run";
    /// Kernel → plugin: what could follow this `/name`'s partial argument.
    pub const COMMAND_COMPLETE: &str = "command/complete";
    /// Kernel → plugin: what this contributor adds to the round in the query.
    pub const CONTEXT_CONTRIBUTE: &str = "context/contribute";
    /// Kernel → plugin: summarise this transcript.
    pub const COMPACTOR_COMPACT: &str = "compactor/compact";
    /// Kernel → plugin: stream one model response. The answer is the stream's
    /// close, not its content; the content arrives as [`PROVIDER_DELTA`].
    pub const PROVIDER_STREAM: &str = "provider/stream";
    /// Either way: one call on one service, by key (ADR-0031 §4). Kernel →
    /// plugin serves a service that plugin declared; plugin → kernel is the
    /// one thing a process may ask the host for, and the host routes it to
    /// whoever holds the key.
    pub const SERVICE_CALL: &str = "service/call";
    /// Kernel → plugin: one of the four decision points, waiting on an
    /// outcome (ADR-0032 §2). A hook that does not answer never decides.
    pub const HOOK_DECIDE: &str = "hook/decide";

    /// Kernel → plugin: one of the four observation points happened
    /// (ADR-0032 §3). Nothing is awaited, so watching costs the turn nothing.
    pub const HOOK_OBSERVE: &str = "hook/observe";
    /// Plugin → kernel: replace a running call's live output line.
    pub const TOOL_PROGRESS: &str = "tool/progress";
    /// Kernel → plugin: the turn was interrupted; the call may stop itself.
    pub const TOOL_CANCEL: &str = "tool/cancel";
    /// Plugin → kernel: one event of a running stream.
    pub const PROVIDER_DELTA: &str = "provider/delta";
    /// Kernel → plugin: this stream is no longer wanted; stop it.
    pub const PROVIDER_CANCEL: &str = "provider/cancel";
}

/// Where the host lives, as a process that is not in it can read.
///
/// A projection of the sdk's `Env`, which is not `Serialize`: a plugin that
/// keeps state of its own needs somewhere to put it, and `dataDir` is where.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostEnv {
    pub home: PathBuf,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl From<&Env> for HostEnv {
    fn from(env: &Env) -> Self {
        Self {
            home: env.home.clone(),
            config_dir: env.config_dir.clone(),
            data_dir: env.data_dir.clone(),
        }
    }
}

/// The handshake, sent once on a fresh process.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol: u32,
    /// The directory the manifest was read from, already resolved.
    pub plugin_root: PathBuf,
    /// This plugin's settings slice (`plugins.<name>`); `null` when unset.
    #[serde(default)]
    pub config: Value,
    pub env: HostEnv,
}

/// What the process says it is and what it contributes. Everything here is a
/// claim: the tools it names are registered untrusted (ADR-0015 §4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// The major the process speaks. The host refuses any but its own, which
    /// it can only do if the process says which one it is.
    pub protocol: u32,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    #[serde(default)]
    pub commands: Vec<CommandSpec>,
    #[serde(default)]
    pub contributors: Vec<ContributorSpec>,
    #[serde(default)]
    pub compactors: Vec<CompactorSpec>,
    #[serde(default)]
    pub providers: Vec<ProviderSpec>,
    #[serde(default)]
    pub hooks: Vec<HookSpec>,
    /// The services this process serves, by the key another plugin calls them
    /// by. A key is global — it is what a consumer writes in `requires` — so
    /// the declaration is a map and not a list: one answer per key.
    #[serde(default)]
    pub services: BTreeMap<String, ServiceSpec>,
}

/// What one service speaks: a name for every method it answers, and the JSON
/// Schema of what that method takes.
///
/// The schema is for whoever writes the caller — nothing here validates
/// against it, as nothing validates a manifest's `config` schema — and the
/// names are the host's: a method the declaration never named is refused
/// before it crosses, with the set that is named (ADR-0031 §5). A service
/// that names no method speaks none.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSpec {
    #[serde(default)]
    pub methods: BTreeMap<String, Value>,
}

/// A contributor a process says it has. Placement is handshake data: which of
/// the three moments this contributor speaks at is asked once, here, and never
/// per call. A placement this host does not know refuses the handshake, in
/// words, rather than being guessed at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContributorSpec {
    /// The name this process knows the contributor by; `context/contribute`
    /// carries it back. The kernel sees it prefixed by the plugin's own name.
    pub id: String,
    pub placement: Placement,
}

/// A compaction strategy a process says it has. One field, and the same shape
/// as every other declaration: a plugin's answer to "what do you contribute"
/// reads the same way whatever the kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompactorSpec {
    pub id: String,
}

/// A model provider a process says it serves, and the whole of what the host
/// believes about it. The id is the one a person types (`--provider <id>`,
/// `/model <id>/<model>`), `family` is the catalogue shelf its models are filed
/// under (ADR-0017; the id itself by default, as the sdk trait does), and
/// `models` is what it serves — the answer to `Provider::models`, given once
/// here so that asking costs no call.
///
/// `endpoint` is what this endpoint does with a request for one of *those*
/// models. A model the declaration does not name gets nothing: no images, no
/// token counting, no caching (ADR-0015 §4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSpec {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default)]
    pub models: Vec<ModelInfo>,
    #[serde(default)]
    pub endpoint: EndpointCapabilities,
}

/// One call, named as the plugin named it — the `plugin__<name>__` prefix is
/// the model's and the permission grammar's, never the process's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallParams {
    pub call_id: String,
    pub name: String,
    pub input: Value,
    pub cwd: PathBuf,
    pub session: SessionId,
    pub turn: TurnId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResult {
    pub output: ToolOutput,
}

/// One `/name args` line, with the argument text exactly as it was typed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandRunParams {
    pub name: String,
    pub args: String,
    pub cwd: PathBuf,
    pub session: SessionId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandRunResult {
    pub outcome: CommandOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandCompleteParams {
    pub name: String,
    pub partial: String,
    pub cwd: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandCompleteResult {
    #[serde(default)]
    pub completions: Vec<Completion>,
}

/// One round, as a process that is not in the host can read it.
///
/// The serializable projection of the sdk's `ContextQuery` (ADR-0030 §5): the
/// session, where it is and what it has said so far. The host handle the
/// in-process query carries stays on this side — an external contributor
/// reads the round, it does not reach the host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContributeQuery {
    pub session: SessionSummary,
    pub turn: TurnId,
    pub round: u32,
    #[serde(default)]
    pub items: Vec<Item>,
    pub usage: ContextUsage,
    pub capabilities: ModelCapabilities,
    pub cwd: PathBuf,
}

impl From<ContextQuery<'_>> for ContributeQuery {
    fn from(query: ContextQuery<'_>) -> Self {
        Self {
            session: query.session.clone(),
            turn: query.turn.clone(),
            round: query.round,
            items: query.items.to_vec(),
            usage: *query.usage,
            capabilities: query.capabilities.clone(),
            cwd: query.cwd.to_path_buf(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextContributeParams {
    /// Which of the contributors the handshake declared is being asked.
    pub id: String,
    pub query: ContributeQuery,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextContributeResult {
    /// A contributor with nothing to add this round says so with an empty
    /// list, which is never wrong.
    #[serde(default)]
    pub pieces: Vec<ContextPiece>,
}

/// What a compaction acts on, as a process that is not in the host can read
/// it: the sdk's `CompactContext` without what cannot cross — the host's
/// provider and the turn's cancellation token. A remote strategy summarises by
/// its own means, or cuts by none.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompactorContext {
    #[serde(default)]
    pub items: Vec<Item>,
    pub usage: ContextUsage,
    pub capabilities: ModelCapabilities,
    pub model: String,
    /// Consecutive compactions the kernel discarded; at `BREAKER_TRIP` the
    /// breaker is tripped and a strategy takes its rung that needs no model.
    pub failures: u32,
    /// Tokens of the newest items a cut should leave intact.
    pub keep_budget: u64,
}

impl From<&CompactContext<'_>> for CompactorContext {
    fn from(cx: &CompactContext<'_>) -> Self {
        Self {
            items: cx.items.to_vec(),
            usage: cx.usage,
            capabilities: cx.capabilities.clone(),
            model: cx.model.to_string(),
            failures: cx.failures,
            keep_budget: cx.keep_budget,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompactorCompactParams {
    /// Which of the compactors the handshake declared is being asked.
    pub id: String,
    pub context: CompactorContext,
    pub reason: CompactReason,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompactorCompactResult {
    pub compaction: Compaction,
}

/// One model response, asked for. The request crosses as the sdk writes it —
/// system blocks, messages, tools and all — because a provider that cannot read
/// the whole request cannot answer it.
///
/// `call` names this stream. Every [`ProviderDeltaParams`] carries it back and
/// [`ProviderCancelParams`] stops it by it, so two streams running on one pipe
/// never interleave.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStreamParams {
    /// Which of the providers the handshake declared is being asked.
    pub id: String,
    pub call: String,
    pub request: ModelRequest,
}

/// The close. A stream that ran to its `finish` event answers with nothing; one
/// that broke answers with the error the trait speaks, so the kernel's retry
/// ladder reads the same kind it reads from an in-process provider.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStreamResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProviderError>,
}

/// Plugin → kernel, while a stream runs: one event, for one stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDeltaParams {
    pub call: String,
    pub event: ModelEvent,
}

/// Kernel → plugin: nobody is reading this stream any more — the turn was
/// interrupted, or whoever asked let go. A process that ignores it is slow, not
/// broken: the host stops reading either way, and the stream's idle deadline is
/// what keeps a silent one from holding a turn open.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCancelParams {
    pub call: String,
}

/// One call on one service, the same shape whichever way it goes: the key
/// that names the service, the method, and whatever that method takes.
///
/// Neither side reads `params`: it is the service's own contract, written in
/// the schema the declaration carries, and the bridge is a pipe for it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceCallParams {
    pub key: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// What the service answered. A service with nothing to say answers `null`,
/// which is an answer; a service that could not answer fails the call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceCallResult {
    #[serde(default)]
    pub result: Value,
}

/// A hook a process says it has, and the matcher it is claimed with.
///
/// The matcher is the sdk's own, declared once: the kernel's cheap skip runs
/// on this side, so an event this matcher rules out never crosses the pipe at
/// all (ADR-0032 §1). A declaration that names no point wants every point, as
/// an in-process hook's empty matcher does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HookSpec {
    /// The name this process knows the hook by; `hook/decide` and
    /// `hook/observe` carry it back. The kernel sees it prefixed by the
    /// plugin's own name.
    pub id: String,
    #[serde(flatten)]
    pub matcher: HookMatcher,
}

/// Where a hook is standing, as a process that is not in the host can read it.
///
/// The serializable projection of the sdk's `HookContext` (ADR-0030 §5): which
/// session, which turn if any, where it works and what model answers it. The
/// host handle and the provider stay on this side — an external hook reads
/// where it is, it does not reach the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HookSite {
    pub session: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnId>,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl From<&HookContext> for HookSite {
    fn from(cx: &HookContext) -> Self {
        Self {
            session: cx.session.clone(),
            turn: cx.turn.clone(),
            cwd: cx.cwd.clone(),
            model: cx.model.clone(),
        }
    }
}

/// What one decision point hands over. The point is the tag and the payload is
/// what that point owns — the two the trait mutates carry the value to rewrite,
/// the two that only judge carry what they judge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "point", content = "payload", rename_all = "camelCase")]
pub enum HookDecision {
    Submit { input: Input },
    BeforeTool { call: ToolCall },
    AfterTool { call: ToolCall, output: ToolOutput },
    Stop,
}

/// One decision, asked for and waited on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HookDecideParams {
    /// Which of the hooks the handshake declared is deciding.
    pub id: String,
    pub site: HookSite,
    #[serde(flatten)]
    pub decision: HookDecision,
}

/// The value a hook rewrote, in the one shape the point that owns it takes.
/// A point that owns no mutable argument has no variant here, so a rewrite
/// anywhere else is refused by the shape rather than by a check.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HookValue {
    /// `submit`'s input, as the hook would have it.
    Input { input: Input },
    /// `beforeTool`'s call, as the hook would have it.
    Call { call: ToolCall },
}

/// What the hook decided, and what it rewrote if the point owned anything.
///
/// The outcome crosses as the sdk writes it: there is no `Allow` in the type,
/// so the wire cannot say one (ADR-0032 §4). A hook that only watches this
/// point answers `continue` and nothing else.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HookDecideResult {
    pub outcome: HookOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<HookValue>,
}

/// What one observation point hands over. Nothing here is answered, so
/// nothing here has a result type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "point", content = "payload", rename_all = "camelCase")]
pub enum HookObservation {
    Turn {
        phase: Phase,
        turn: TurnId,
        #[serde(default)]
        items: Vec<Item>,
    },
    Compact {
        phase: Phase,
    },
    Session {
        phase: Phase,
    },
    Event {
        frame: Box<Frame>,
    },
}

/// Kernel → plugin: one observation point happened. Nothing is awaited.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HookObserveParams {
    /// Which of the hooks the handshake declared is watching.
    pub id: String,
    pub site: HookSite,
    #[serde(flatten)]
    pub observation: HookObservation,
}

/// Plugin → kernel, while a call runs: the whole of the live output line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolProgressParams {
    pub call_id: String,
    pub tail: String,
}

/// Kernel → plugin: this call's turn was interrupted, and nobody is waiting
/// for the answer any more. The host has already let the call go, so a process
/// that ignores this is not holding anything up — it is only doing work whose
/// result will be thrown away.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolCancelParams {
    pub call_id: String,
}

/// Names a type in the schema: adds it to `$defs` and answers with its `$ref`.
pub type Ref = fn(&mut SchemaGenerator) -> Schema;

pub fn schema_of<T: JsonSchema>(generator: &mut SchemaGenerator) -> Schema {
    generator.subschema_for::<T>()
}

/// A method: its name, its params, its result.
pub type Method = (&'static str, Ref, Ref);

/// A notification: its name and its params.
pub type Notification = (&'static str, Ref);

pub static METHODS: &[Method] = &[
    (
        name::INITIALIZE,
        schema_of::<InitializeParams>,
        schema_of::<InitializeResult>,
    ),
    (
        name::TOOL_CALL,
        schema_of::<ToolCallParams>,
        schema_of::<ToolCallResult>,
    ),
    (
        name::COMMAND_RUN,
        schema_of::<CommandRunParams>,
        schema_of::<CommandRunResult>,
    ),
    (
        name::COMMAND_COMPLETE,
        schema_of::<CommandCompleteParams>,
        schema_of::<CommandCompleteResult>,
    ),
    (
        name::CONTEXT_CONTRIBUTE,
        schema_of::<ContextContributeParams>,
        schema_of::<ContextContributeResult>,
    ),
    (
        name::COMPACTOR_COMPACT,
        schema_of::<CompactorCompactParams>,
        schema_of::<CompactorCompactResult>,
    ),
    (
        name::PROVIDER_STREAM,
        schema_of::<ProviderStreamParams>,
        schema_of::<ProviderStreamResult>,
    ),
    (
        name::SERVICE_CALL,
        schema_of::<ServiceCallParams>,
        schema_of::<ServiceCallResult>,
    ),
    (
        name::HOOK_DECIDE,
        schema_of::<HookDecideParams>,
        schema_of::<HookDecideResult>,
    ),
];

pub static NOTIFICATIONS: &[Notification] = &[
    (name::TOOL_PROGRESS, schema_of::<ToolProgressParams>),
    (name::TOOL_CANCEL, schema_of::<ToolCancelParams>),
    (name::PROVIDER_DELTA, schema_of::<ProviderDeltaParams>),
    (name::PROVIDER_CANCEL, schema_of::<ProviderCancelParams>),
    (name::HOOK_OBSERVE, schema_of::<HookObserveParams>),
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn no_name_is_used_twice() {
        let mut names: Vec<&str> = METHODS
            .iter()
            .map(|method| method.0)
            .chain(NOTIFICATIONS.iter().map(|notification| notification.0))
            .collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total);
    }

    #[test]
    fn the_handshake_travels_in_camel_case() {
        let params = InitializeParams {
            protocol: PROTOCOL,
            plugin_root: PathBuf::from("/plugins/wordcount"),
            config: json!({ "limit": 10 }),
            env: HostEnv::from(&Env::rooted("/home/u")),
        };
        let wire = serde_json::to_value(&params).expect("the handshake serialises");
        assert_eq!(wire["pluginRoot"], json!("/plugins/wordcount"));
        assert_eq!(wire["env"]["dataDir"], json!("/home/u/.bingo/data"));
        assert_eq!(
            serde_json::from_value::<InitializeParams>(wire).expect("and parses"),
            params
        );
    }

    /// A process that says nothing about what it contributes has contributed
    /// nothing, which is never wrong (ADR-0009 §1).
    #[test]
    fn a_handshake_may_name_no_tools_and_no_commands() {
        let result: InitializeResult = serde_json::from_value(json!({
            "protocol": 1, "name": "quiet", "version": "0.1.0"
        }))
        .expect("a handshake");
        assert!(result.tools.is_empty() && result.commands.is_empty());
    }

    #[test]
    fn a_call_carries_the_ids_the_kernel_minted() {
        let params = ToolCallParams {
            call_id: "call_1".into(),
            name: "count".into(),
            input: json!({ "path": "notes.txt" }),
            cwd: PathBuf::from("/work"),
            session: SessionId::from_raw("ses_1"),
            turn: TurnId::from_raw("trn_1"),
        };
        let wire = serde_json::to_value(&params).expect("a call serialises");
        assert_eq!(wire["callId"], json!("call_1"));
        assert_eq!(wire["session"], json!("ses_1"));
        assert_eq!(wire["turn"], json!("trn_1"));
    }

    #[test]
    fn an_outcome_crosses_as_the_sdk_writes_it() {
        let result = CommandRunResult {
            outcome: CommandOutcome::Applied {
                message: Some("counted".into()),
            },
        };
        let wire = serde_json::to_value(&result).expect("an outcome serialises");
        assert_eq!(wire["outcome"]["kind"], json!("applied"));
        assert_eq!(wire["outcome"]["message"], json!("counted"));
    }

    // ------------------------------------------- context and compaction

    fn summary() -> SessionSummary {
        serde_json::from_value(json!({
            "id": "ses_1",
            "cwd": "/work",
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z",
        }))
        .expect("a session summary")
    }

    fn capabilities() -> ModelCapabilities {
        ModelCapabilities {
            context_window: 200_000,
            max_output: 8_000,
            images: false,
            reasoning: false,
            count_tokens: false,
            caching: false,
        }
    }

    fn usage() -> ContextUsage {
        ContextUsage {
            used: 100,
            window: 1_000,
            trigger: 800,
        }
    }

    /// The one thing a projection is for: an external contributor reads the
    /// round, and reaches nothing (ADR-0030 §5).
    #[test]
    fn the_round_crosses_without_the_host_the_query_carries() {
        let (session, turn, host) = (
            summary(),
            TurnId::from_raw("trn_1"),
            bingo_sdk::testing::NoHost::handle(),
        );
        let query = ContributeQuery::from(ContextQuery {
            session: &session,
            host: &host,
            turn: &turn,
            round: 2,
            items: &[],
            usage: &usage(),
            capabilities: &capabilities(),
            cwd: std::path::Path::new("/work"),
        });
        let wire = serde_json::to_value(&query).expect("a round serialises");
        assert!(wire.get("host").is_none(), "{wire}");
        assert_eq!(wire["session"]["id"], json!("ses_1"));
        assert_eq!(wire["round"], json!(2));
        assert_eq!(wire["usage"]["window"], json!(1_000));
        assert_eq!(wire["capabilities"]["contextWindow"], json!(200_000));
        assert_eq!(
            serde_json::from_value::<ContributeQuery>(wire).expect("and parses"),
            query
        );
    }

    #[test]
    fn a_declared_contributor_carries_the_placement_it_speaks_at() {
        let spec: ContributorSpec = serde_json::from_value(json!({
            "id": "notes",
            "placement": { "kind": "system", "order": 10 }
        }))
        .expect("a contributor declaration");
        assert_eq!(spec.placement, Placement::System { order: 10 });
        let round: ContributorSpec = serde_json::from_value(json!({
            "id": "inbox",
            "placement": { "kind": "roundStart" }
        }))
        .expect("a contributor declaration");
        assert_eq!(round.placement, Placement::RoundStart);
    }

    #[test]
    fn a_placement_this_host_does_not_know_is_refused_in_words() {
        let why = serde_json::from_value::<ContributorSpec>(json!({
            "id": "notes",
            "placement": { "kind": "whenever" }
        }))
        .expect_err("there are three placements and no others")
        .to_string();
        assert!(why.contains("whenever"), "{why}");
    }

    #[test]
    fn a_piece_crosses_as_the_sdk_writes_it() {
        let result = ContextContributeResult {
            pieces: vec![ContextPiece::User {
                parts: vec![bingo_sdk::ContentPart::text("three notes")],
                label: "notes".into(),
            }],
        };
        let wire = serde_json::to_value(&result).expect("a piece serialises");
        assert_eq!(wire["pieces"][0]["kind"], json!("user"));
        assert_eq!(wire["pieces"][0]["parts"][0]["text"], json!("three notes"));
        assert_eq!(
            serde_json::from_value::<ContextContributeResult>(wire).expect("and parses"),
            result
        );
    }

    /// A contributor with nothing to add answers with nothing, which is never
    /// wrong (ADR-0009 §1).
    #[test]
    fn a_contributor_may_answer_with_no_pieces_at_all() {
        let result: ContextContributeResult =
            serde_json::from_value(json!({})).expect("an empty answer");
        assert!(result.pieces.is_empty());
    }

    #[test]
    fn a_compaction_is_asked_for_with_the_reason_the_trait_speaks() {
        let params = CompactorCompactParams {
            id: "cut".into(),
            context: CompactorContext {
                items: Vec::new(),
                usage: usage(),
                capabilities: capabilities(),
                model: "m".into(),
                failures: 1,
                keep_budget: 250,
            },
            reason: CompactReason::Overflow {
                message: "too long".into(),
            },
        };
        let wire = serde_json::to_value(&params).expect("a compaction request serialises");
        assert_eq!(wire["context"]["keepBudget"], json!(250));
        assert_eq!(wire["reason"]["kind"], json!("overflow"));
        assert_eq!(wire["reason"]["message"], json!("too long"));
        assert!(
            wire["context"].get("provider").is_none(),
            "the provider stays on this side: {wire}"
        );
        assert_eq!(
            serde_json::from_value::<CompactorCompactParams>(wire).expect("and parses"),
            params
        );
    }

    // -------------------------------------------------------- the stream lane

    fn request() -> ModelRequest {
        ModelRequest {
            model: "stub-1".into(),
            max_tokens: 1_000,
            system: Vec::new(),
            messages: vec![bingo_sdk::Message::text(
                bingo_sdk::Role::User,
                "say something",
            )],
            tools: Vec::new(),
            reasoning: None,
            session: None,
            provider_options: Default::default(),
        }
    }

    /// The open: which provider, which stream, and the request the trait
    /// speaks, whole.
    #[test]
    fn a_stream_opens_with_the_request_the_sdk_writes() {
        let params = ProviderStreamParams {
            id: "house".into(),
            call: "call-1".into(),
            request: request(),
        };
        let wire = serde_json::to_value(&params).expect("an open serialises");
        assert_eq!(wire["id"], json!("house"));
        assert_eq!(wire["call"], json!("call-1"));
        assert_eq!(wire["request"]["maxTokens"], json!(1_000));
        assert_eq!(wire["request"]["messages"][0]["parts"][0]["type"], "text");
        assert_eq!(
            serde_json::from_value::<ProviderStreamParams>(wire).expect("and parses"),
            params
        );
    }

    /// The one thing the delta lane is for: an event of the sdk's own enum,
    /// named for the stream it belongs to. Two streams' deltas are told apart
    /// by `call` and by nothing else.
    #[test]
    fn a_delta_carries_one_sdk_event_and_the_stream_it_belongs_to() {
        let delta = ProviderDeltaParams {
            call: "call-1".into(),
            event: ModelEvent::TextDelta {
                id: "b1".into(),
                delta: "half".into(),
            },
        };
        let wire = serde_json::to_value(&delta).expect("a delta serialises");
        assert_eq!(wire["call"], json!("call-1"));
        assert_eq!(wire["event"]["type"], json!("textDelta"));
        assert_eq!(wire["event"]["delta"], json!("half"));
        assert_eq!(
            serde_json::from_value::<ProviderDeltaParams>(wire).expect("and parses"),
            delta
        );
        let other: ProviderDeltaParams = serde_json::from_value(json!({
            "call": "call-2",
            "event": { "type": "textDelta", "id": "b1", "delta": "half" }
        }))
        .expect("another stream's delta");
        assert_ne!(other.call, delta.call, "the same event, another stream");
        assert_eq!(other.event, delta.event);
    }

    /// A finish crosses like any other event: the kernel folds it, the bridge
    /// does not read it.
    #[test]
    fn a_finish_crosses_as_the_sdk_writes_it() {
        let delta: ProviderDeltaParams = serde_json::from_value(json!({
            "call": "call-1",
            "event": {
                "type": "finish",
                "usage": { "inputTokens": 10, "outputTokens": 3 },
                "finishReason": { "unified": "stop" }
            }
        }))
        .expect("a finish");
        let ModelEvent::Finish {
            usage,
            finish_reason,
        } = delta.event
        else {
            panic!("a finish is a finish");
        };
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(finish_reason.unified, bingo_sdk::UnifiedFinish::Stop);
    }

    /// The close: nothing, or the error the trait speaks — kind and all, so
    /// the kernel's retry ladder reads what it always reads.
    #[test]
    fn a_stream_closes_with_nothing_or_with_the_error_the_trait_speaks() {
        let clean: ProviderStreamResult =
            serde_json::from_value(json!({})).expect("a stream that ran to its finish");
        assert!(clean.error.is_none());
        let broken: ProviderStreamResult = serde_json::from_value(json!({
            "error": { "kind": "rateLimited", "retryAfterMs": 1_500 }
        }))
        .expect("a stream that broke");
        let error = broken.error.expect("the error the trait speaks");
        assert!(error.retryable() && error.retry_after_ms() == Some(1_500));
        assert_eq!(
            serde_json::to_value(ProviderStreamResult { error: None }).expect("it serialises"),
            json!({}),
            "a clean close says nothing at all"
        );
    }

    #[test]
    fn a_cancel_names_the_stream_it_stops_and_no_other() {
        let params = ProviderCancelParams {
            call: "call-2".into(),
        };
        let wire = serde_json::to_value(&params).expect("a cancel serialises");
        assert_eq!(wire, json!({ "call": "call-2" }));
        assert_eq!(
            serde_json::from_value::<ProviderCancelParams>(wire).expect("and parses"),
            params
        );
    }

    /// The declaration a provider is built from: what it serves, where its
    /// models are filed, and what its endpoint does with them.
    #[test]
    fn a_declared_provider_carries_its_models_and_what_its_endpoint_does() {
        let spec: ProviderSpec = serde_json::from_value(json!({
            "id": "house",
            "family": "anthropic",
            "models": [{ "id": "house-1", "display": "House One" }],
            "endpoint": { "images": true, "caching": true }
        }))
        .expect("a provider declaration");
        assert_eq!(spec.family.as_deref(), Some("anthropic"));
        assert_eq!(spec.models[0].id, "house-1");
        assert!(spec.endpoint.images && spec.endpoint.caching);
        assert!(
            !spec.endpoint.count_tokens,
            "what the declaration leaves out is false, never guessed"
        );
    }

    /// A process may declare a provider and say nothing else about it: no
    /// family (the id is the shelf), no models, an endpoint that does nothing.
    #[test]
    fn a_provider_that_says_only_its_name_is_a_declaration_too() {
        let spec: ProviderSpec =
            serde_json::from_value(json!({ "id": "quiet" })).expect("a declaration");
        assert!(spec.family.is_none() && spec.models.is_empty());
        assert_eq!(spec.endpoint, EndpointCapabilities::default());
        assert_eq!(
            serde_json::to_value(&spec).expect("it serialises"),
            json!({ "id": "quiet", "models": [], "endpoint": {
                "images": false, "countTokens": false, "caching": false
            }}),
            "an absent family is absent on the wire, never the id repeated"
        );
    }

    // ------------------------------------------------------ services, both ways

    /// The same line serves both directions: the host asking a process for a
    /// service it declared, and a process asking the host for one the host
    /// routes. There is no second shape, because there is no second lane.
    #[test]
    fn a_service_call_is_one_shape_whichever_way_it_travels() {
        let params = ServiceCallParams {
            key: "kv".into(),
            method: "set".into(),
            params: json!({ "key": "greeting", "value": "hello" }),
        };
        let wire = serde_json::to_value(&params).expect("a call serialises");
        assert_eq!(
            wire,
            json!({
                "key": "kv",
                "method": "set",
                "params": { "key": "greeting", "value": "hello" }
            })
        );
        assert_eq!(
            serde_json::from_value::<ServiceCallParams>(wire).expect("and parses"),
            params,
            "the host writes what a process writes, and reads what a process reads"
        );
    }

    /// A method that takes nothing says nothing, and a service that answers
    /// nothing says `null` — an answer, and not a failure.
    #[test]
    fn a_call_may_take_nothing_and_an_answer_may_be_nothing() {
        let params: ServiceCallParams =
            serde_json::from_value(json!({ "key": "kv", "method": "clear" }))
                .expect("a call with no params");
        assert_eq!(params.params, Value::Null);
        let answered: ServiceCallResult = serde_json::from_value(json!({})).expect("an answer");
        assert_eq!(answered.result, Value::Null);
        assert_eq!(
            serde_json::to_value(ServiceCallResult {
                result: json!("hello")
            })
            .expect("it serialises"),
            json!({ "result": "hello" })
        );
    }

    /// What a service declaration is: the methods by name, and the schema of
    /// each. The names are the host's — it refuses what is not there — and the
    /// schemas are the caller's to read.
    #[test]
    fn a_declared_service_names_the_methods_it_speaks_and_their_schemas() {
        let result: InitializeResult = serde_json::from_value(json!({
            "protocol": PROTOCOL,
            "name": "store",
            "version": "0.1.0",
            "services": {
                "kv": { "methods": {
                    "set": { "type": "object", "required": ["key", "value"] },
                    "get": { "type": "object", "required": ["key"] }
                }}
            }
        }))
        .expect("a handshake that declares a service");
        let kv = result.services.get("kv").expect("the declared service");
        assert_eq!(
            kv.methods.keys().cloned().collect::<Vec<_>>(),
            ["get", "set"],
            "a method the declaration never named is one the host refuses"
        );
        assert_eq!(kv.methods["get"]["required"], json!(["key"]));
    }

    /// A process that declares no services declares none: the field is absent
    /// on the wire, never an empty object nobody wrote.
    #[test]
    fn a_process_that_serves_no_service_says_nothing_about_services() {
        let result: InitializeResult =
            serde_json::from_value(json!({ "protocol": PROTOCOL, "name": "q", "version": "0" }))
                .expect("a handshake");
        assert!(result.services.is_empty());
        let spec: ServiceSpec = serde_json::from_value(json!({})).expect("a bare declaration");
        assert!(
            spec.methods.is_empty(),
            "and a service that names no method speaks none"
        );
    }

    // ------------------------------------------------------------ hooks

    fn site() -> HookSite {
        HookSite {
            session: SessionId::from_raw("ses_1"),
            turn: Some(TurnId::from_raw("trn_1")),
            cwd: PathBuf::from("/work"),
            model: Some("stub-1".into()),
        }
    }

    fn call() -> ToolCall {
        ToolCall {
            call_id: "c1".into(),
            name: "Bash".into(),
            input: json!({ "command": "ls" }),
        }
    }

    /// The declaration mirrors `HookMatcher`: the points and the tool pattern
    /// are the sdk's own fields, flat beside the id.
    #[test]
    fn a_declared_hook_carries_the_matcher_the_kernel_skips_on() {
        let spec: HookSpec = serde_json::from_value(json!({
            "id": "guard",
            "points": ["beforeTool", "submit"],
            "tool": "Bash"
        }))
        .expect("a hook declaration");
        assert_eq!(
            spec.matcher,
            HookMatcher {
                points: vec![
                    bingo_sdk::HookPoint::BeforeTool,
                    bingo_sdk::HookPoint::Submit
                ],
                tool: Some("Bash".into()),
            }
        );
        assert_eq!(
            serde_json::to_value(&spec).expect("it serialises"),
            json!({ "id": "guard", "points": ["beforeTool", "submit"], "tool": "Bash" })
        );
    }

    /// A hook that names no point wants every point, and says nothing about a
    /// tool — the empty matcher, exactly as an in-process hook writes it.
    #[test]
    fn a_hook_that_names_no_point_wants_them_all() {
        let spec: HookSpec = serde_json::from_value(json!({ "id": "watch" })).expect("a hook");
        assert_eq!(spec.matcher, HookMatcher::default());
        assert_eq!(
            serde_json::to_value(&spec).expect("it serialises"),
            json!({ "id": "watch", "points": [] })
        );
    }

    /// A point this host does not know is refused in words, as a placement is.
    #[test]
    fn a_point_this_host_does_not_know_is_refused_in_words() {
        let why = serde_json::from_value::<HookSpec>(json!({
            "id": "guard",
            "points": ["whenever"]
        }))
        .expect_err("there are eight points and no others")
        .to_string();
        assert!(why.contains("whenever"), "{why}");
    }

    /// The one thing the projection is for: an external hook reads where it is
    /// standing, and reaches nothing (ADR-0030 §5).
    #[test]
    fn a_site_crosses_without_the_host_the_context_carries() {
        let wire = serde_json::to_value(site()).expect("a site serialises");
        assert!(
            wire.get("host").is_none() && wire.get("provider").is_none(),
            "{wire}"
        );
        assert_eq!(
            wire,
            json!({
                "session": "ses_1", "turn": "trn_1",
                "cwd": "/work", "model": "stub-1"
            })
        );
    }

    /// One decision, tagged by the point that asked for it, with the payload
    /// that point owns.
    #[test]
    fn a_decision_is_asked_for_by_the_point_that_owns_the_payload() {
        let params = HookDecideParams {
            id: "guard".into(),
            site: site(),
            decision: HookDecision::BeforeTool { call: call() },
        };
        let wire = serde_json::to_value(&params).expect("a decision serialises");
        assert_eq!(wire["id"], json!("guard"));
        assert_eq!(wire["point"], json!("beforeTool"));
        assert_eq!(wire["payload"]["call"]["name"], json!("Bash"));
        assert_eq!(
            serde_json::from_value::<HookDecideParams>(wire).expect("and parses"),
            params
        );
    }

    /// `stop` judges nothing but the moment: the point is the whole message.
    #[test]
    fn a_stop_carries_the_point_and_no_payload_at_all() {
        let params = HookDecideParams {
            id: "again".into(),
            site: site(),
            decision: HookDecision::Stop,
        };
        let wire = serde_json::to_value(&params).expect("a stop serialises");
        assert_eq!(wire["point"], json!("stop"));
        assert!(wire.get("payload").is_none(), "{wire}");
        assert_eq!(
            serde_json::from_value::<HookDecideParams>(wire).expect("and parses"),
            params
        );
    }

    /// `afterTool` judges what happened and rewrites nothing, so the outcome
    /// comes back alone.
    #[test]
    fn an_after_tool_carries_the_call_and_what_it_answered() {
        let params = HookDecideParams {
            id: "audit".into(),
            site: site(),
            decision: HookDecision::AfterTool {
                call: call(),
                output: ToolOutput::text("two files"),
            },
        };
        let wire = serde_json::to_value(&params).expect("it serialises");
        assert_eq!(
            wire["payload"]["output"]["parts"][0]["text"],
            json!("two files")
        );
        assert_eq!(
            serde_json::from_value::<HookDecideParams>(wire).expect("and parses"),
            params
        );
    }

    /// The outcome crosses as the sdk writes it, and the rewritten value comes
    /// back in the shape of the point that owns one.
    #[test]
    fn an_outcome_crosses_as_the_sdk_writes_it_and_the_rewrite_beside_it() {
        let result = HookDecideResult {
            outcome: HookOutcome::Continue,
            value: Some(HookValue::Call {
                call: ToolCall {
                    input: json!({ "command": "ls -la" }),
                    ..call()
                },
            }),
        };
        let wire = serde_json::to_value(&result).expect("a decision serialises");
        assert_eq!(wire["outcome"], json!({ "kind": "continue" }));
        assert_eq!(wire["value"]["kind"], json!("call"));
        assert_eq!(wire["value"]["call"]["input"]["command"], json!("ls -la"));
        assert_eq!(
            serde_json::from_value::<HookDecideResult>(wire).expect("and parses"),
            result
        );
    }

    /// A hook that only judges answers with the outcome and nothing else.
    #[test]
    fn a_hook_that_rewrites_nothing_sends_no_value() {
        let denied: HookDecideResult = serde_json::from_value(json!({
            "outcome": { "kind": "deny", "reason": "not that one" }
        }))
        .expect("a refusal");
        assert!(denied.value.is_none());
        assert_eq!(
            serde_json::to_value(&denied).expect("it serialises"),
            json!({ "outcome": { "kind": "deny", "reason": "not that one" } })
        );
    }

    /// The law the whole design rests on: nothing a process writes is an
    /// `Allow`, in any spelling, at any point (ADR-0032 §4).
    #[test]
    fn no_answer_a_process_can_write_widens_anything() {
        for word in ["allow", "Allow", "approve", "permit", "ALLOW"] {
            let why = serde_json::from_value::<HookDecideResult>(json!({
                "outcome": { "kind": word }
            }))
            .expect_err("there is no allow to say")
            .to_string();
            assert!(why.contains(word), "{why}");
        }
    }

    /// An observation names its point the same way a decision does, and there
    /// is no result type at all: nothing waits on it.
    #[test]
    fn an_observation_carries_the_phase_and_the_turn_it_watched() {
        let params = HookObserveParams {
            id: "watch".into(),
            site: site(),
            observation: HookObservation::Turn {
                phase: Phase::End,
                turn: TurnId::from_raw("trn_1"),
                items: Vec::new(),
            },
        };
        let wire = serde_json::to_value(&params).expect("an observation serialises");
        assert_eq!(wire["point"], json!("turn"));
        assert_eq!(wire["payload"]["phase"], json!("end"));
        assert_eq!(wire["payload"]["turn"], json!("trn_1"));
        assert_eq!(
            serde_json::from_value::<HookObserveParams>(wire).expect("and parses"),
            params
        );
    }

    /// A frame crosses as the journal writes it: the observer folds the one
    /// event stream, and the bridge does not read it.
    #[test]
    fn an_observed_frame_crosses_as_the_journal_writes_it() {
        let params: HookObserveParams = serde_json::from_value(json!({
            "id": "watch",
            "site": { "session": "ses_1", "cwd": "/work" },
            "point": "event",
            "payload": { "frame": {
                "seq": 1,
                "ts": "2026-01-01T00:00:00Z",
                "session": "ses_1",
                "event": {
                    "type": "turnStarted", "turn": "trn_1",
                    "inputs": [], "origin": "submit"
                }
            }}
        }))
        .expect("an observed frame");
        let HookObservation::Event { frame } = params.observation else {
            panic!("an event is an event");
        };
        assert_eq!(frame.seq, bingo_sdk::Seq(1));
        assert!(params.site.turn.is_none(), "outside a turn there is none");
    }

    #[test]
    fn a_compaction_comes_back_as_the_sdk_writes_it() {
        let result: CompactorCompactResult = serde_json::from_value(json!({
            "compaction": {
                "summary": "what happened",
                "boundary": "itm_7",
                "before": 900,
                "after": 300,
            }
        }))
        .expect("a compaction");
        assert_eq!(result.compaction.boundary.as_str(), "itm_7");
        assert!(result.compaction.kept.is_empty());
        assert_eq!(
            result.compaction.usage,
            bingo_sdk::Usage::default(),
            "a strategy that spends nothing says nothing"
        );
    }
}
