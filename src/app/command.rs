//! What the application core accepts: one submission path, one typed action
//! union, and the mutations that are neither.
//!
//! The composer's slash/shell parser and a GUI's typed call produce the same
//! [`Action`]; there are never separate TUI and GUI handlers. The registry that
//! dispatches them and the metadata `action/list` publishes come from the same
//! place (B5) — this module lands the union and its identity, not its handlers.
// The contract lands before its caller: the actor that routes these (B2/B3) is
// the first consumer. Remove this allow when it arrives.
#![allow(dead_code)]

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::app::ids::{
    AssetId, ConversationId, InteractionId, ItemId, OperationId, QueueId, TurnId,
};
use crate::app::snapshot::{
    ActivationKind, InteractionDecision, ItemCursor, PermissionMode, PermissionRuleDecision,
    ResourceRevision, RewindMode, SessionLocator, ThemeChoice, ThinkingLevel,
};

/// How the composer reads the text it was handed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ComposerMode {
    Normal,
    /// The `!` prefix: the line is a shell command.
    Shell,
}

/// What a client submitted to a conversation.
///
/// `Composer` is the human path — leading slash, shell mode, and direct-address
/// syntax all mean what they mean in the CLI, so a GUI cannot silently bypass
/// them. `SendProse` is the explicit programmatic delivery that parses none of
/// those forms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Submission {
    #[serde(rename_all = "camelCase")]
    Composer {
        mode: ComposerMode,
        text: String,
        attachments: Vec<AssetId>,
    },
    #[serde(rename_all = "camelCase")]
    SendProse {
        text: String,
        attachments: Vec<AssetId>,
    },
}

/// What the core did with a submission. The caller never chooses between these:
/// queue-versus-steer, delivery-versus-turn, and command scope are the core's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SubmitDisposition {
    #[serde(rename_all = "camelCase")]
    TurnStarted { turn_id: TurnId },
    /// Accepted behind a busy turn. Queueing never promises that steering will
    /// happen — if a tool barrier absorbs the entry, `queue/itemAbsorbed` says so.
    #[serde(rename_all = "camelCase")]
    Queued {
        queue_id: QueueId,
        position: u32,
        steer_eligible: bool,
    },
    /// Handed to a conversation that does not run a turn for it.
    #[serde(rename_all = "camelCase")]
    Delivered { message_id: ItemId },
    /// Parsed as an action and applied synchronously.
    #[serde(rename_all = "camelCase")]
    Applied { result: ActionResult },
    #[serde(rename_all = "camelCase")]
    OperationStarted { operation_id: OperationId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ActionResultStatus {
    Applied,
    /// Valid, and nothing changed (selecting the model already selected).
    NoChange,
}

/// The outcome of a synchronously applied action. Views are not returned here —
/// help, status, context, config, skills, tasks, and images are structured reads
/// (`config/read`, `catalog/read`, `resource/read`), and each frontend renders
/// its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActionResult {
    pub status: ActionResultStatus,
    /// The new revision of whatever the action mutated, for a client holding a
    /// precondition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<ResourceRevision>,
    /// One short English line worth showing. A projection, not a contract:
    /// clients branch on the status, never on this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Where a rewind cuts back to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RewindTarget {
    /// Back to the state before this item.
    #[serde(rename_all = "camelCase")]
    Item { item_id: ItemId },
    /// The most recent checkpoint, which is what `/rewind` with no argument means.
    Latest,
}

/// Which family an action belongs to, for grouping in a command menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ActionFamily {
    Session,
    Conversation,
    Model,
    Provider,
    Permission,
    Mcp,
    Skill,
    Team,
    Room,
    Command,
    Settings,
}

/// The stable identity of an action, shared by `action/list` metadata and the
/// registry that dispatches it. Unlike every other identifier here it is a name
/// rather than an epoch-scoped handle: a client may hard-code it.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub struct ActionId(pub String);

impl ActionId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ActionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ActionId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

/// Every uncommon CLI operation, as one union both frontends produce.
///
/// An action carries no origin of its own: `action/execute` names the
/// conversation it was submitted from, so a queued action cannot retarget itself
/// to whatever page happens to be visible when it drains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Action {
    /// Clear the conversation and start a new session (`/clear`, `/reset`, `/new`).
    SessionReset,
    #[serde(rename_all = "camelCase")]
    SessionRename { name: String },
    /// Clean expired session storage (`/gc`).
    SessionGarbageCollect,
    #[serde(rename_all = "camelCase")]
    SessionShare {
        public: bool,
        open: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<PathBuf>,
    },
    #[serde(rename_all = "camelCase")]
    SessionChangeDirectory { path: PathBuf },
    /// Compact the context. It follows the page it was submitted on rather than
    /// the page that is visible when it runs.
    #[serde(rename_all = "camelCase")]
    ConversationCompact {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    ConversationRewind {
        target: RewindTarget,
        mode: RewindMode,
    },
    #[serde(rename_all = "camelCase")]
    ModelSelect { model: String },
    #[serde(rename_all = "camelCase")]
    ProviderSelect { provider: String },
    #[serde(rename_all = "camelCase")]
    ProviderLogin { provider: String },
    #[serde(rename_all = "camelCase")]
    ProviderLogout { provider: String },
    #[serde(rename_all = "camelCase")]
    ThinkingSelect { level: ThinkingLevel },
    #[serde(rename_all = "camelCase")]
    PermissionModeSet { mode: PermissionMode },
    #[serde(rename_all = "camelCase")]
    PermissionRuleAdd {
        decision: PermissionRuleDecision,
        rule: String,
    },
    #[serde(rename_all = "camelCase")]
    PermissionRuleRemove {
        decision: PermissionRuleDecision,
        rule: String,
    },
    #[serde(rename_all = "camelCase")]
    McpEnable { server: String },
    #[serde(rename_all = "camelCase")]
    McpDisable { server: String },
    #[serde(rename_all = "camelCase")]
    McpReconnect {
        /// Absent reconnects every configured server.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        server: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    SkillInvoke {
        skill: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    TeamStart {
        /// Absent starts the whole blueprint.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        members: Option<Vec<String>>,
    },
    #[serde(rename_all = "camelCase")]
    TeamAssign { member: String, task: String },
    #[serde(rename_all = "camelCase")]
    TeamStop {
        /// Absent stops the whole team.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        member: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    RoomJoin { room: String },
    #[serde(rename_all = "camelCase")]
    RoomLeave { room: String },
    /// Send the foreground command to the background. Interrupting it is
    /// `turn/interrupt`, not an action.
    #[serde(rename_all = "camelCase")]
    CommandPromote { item_id: ItemId },
    /// A persisted theme preference. Which colours get painted is frontend-local;
    /// that the choice was saved is not.
    #[serde(rename_all = "camelCase")]
    ThemeSet { theme: ThemeChoice },
}

impl Action {
    /// The action's stable identity. Metadata and dispatch read this same
    /// function, so a renamed variant cannot keep an old menu entry alive.
    pub fn id(&self) -> ActionId {
        ActionId(self.id_str().to_string())
    }

    pub fn family(&self) -> ActionFamily {
        match self {
            Self::SessionReset
            | Self::SessionRename { .. }
            | Self::SessionGarbageCollect
            | Self::SessionShare { .. }
            | Self::SessionChangeDirectory { .. } => ActionFamily::Session,
            Self::ConversationCompact { .. } | Self::ConversationRewind { .. } => {
                ActionFamily::Conversation
            }
            Self::ModelSelect { .. } | Self::ThinkingSelect { .. } => ActionFamily::Model,
            Self::ProviderSelect { .. }
            | Self::ProviderLogin { .. }
            | Self::ProviderLogout { .. } => ActionFamily::Provider,
            Self::PermissionModeSet { .. }
            | Self::PermissionRuleAdd { .. }
            | Self::PermissionRuleRemove { .. } => ActionFamily::Permission,
            Self::McpEnable { .. } | Self::McpDisable { .. } | Self::McpReconnect { .. } => {
                ActionFamily::Mcp
            }
            Self::SkillInvoke { .. } => ActionFamily::Skill,
            Self::TeamStart { .. } | Self::TeamAssign { .. } | Self::TeamStop { .. } => {
                ActionFamily::Team
            }
            Self::RoomJoin { .. } | Self::RoomLeave { .. } => ActionFamily::Room,
            Self::CommandPromote { .. } => ActionFamily::Command,
            Self::ThemeSet { .. } => ActionFamily::Settings,
        }
    }

    fn id_str(&self) -> &'static str {
        match self {
            Self::SessionReset => "session.reset",
            Self::SessionRename { .. } => "session.rename",
            Self::SessionGarbageCollect => "session.gc",
            Self::SessionShare { .. } => "session.share",
            Self::SessionChangeDirectory { .. } => "session.cd",
            Self::ConversationCompact { .. } => "conversation.compact",
            Self::ConversationRewind { .. } => "conversation.rewind",
            Self::ModelSelect { .. } => "model.select",
            Self::ProviderSelect { .. } => "provider.select",
            Self::ProviderLogin { .. } => "provider.login",
            Self::ProviderLogout { .. } => "provider.logout",
            Self::ThinkingSelect { .. } => "thinking.select",
            Self::PermissionModeSet { .. } => "permission.mode",
            Self::PermissionRuleAdd { .. } => "permission.ruleAdd",
            Self::PermissionRuleRemove { .. } => "permission.ruleRemove",
            Self::McpEnable { .. } => "mcp.enable",
            Self::McpDisable { .. } => "mcp.disable",
            Self::McpReconnect { .. } => "mcp.reconnect",
            Self::SkillInvoke { .. } => "skill.invoke",
            Self::TeamStart { .. } => "team.start",
            Self::TeamAssign { .. } => "team.assign",
            Self::TeamStop { .. } => "team.stop",
            Self::RoomJoin { .. } => "room.join",
            Self::RoomLeave { .. } => "room.leave",
            Self::CommandPromote { .. } => "command.promote",
            Self::ThemeSet { .. } => "theme.set",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ArgumentKind {
    String,
    Integer,
    Boolean,
    Path,
    /// One of `choices`.
    Enumeration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActionArgument {
    pub name: String,
    pub kind: ArgumentKind,
    pub required: bool,
    pub description: String,
    pub choices: Vec<String>,
}

/// What `action/list` publishes for one action. The command menu and `/help`
/// derive from the same registry that dispatches it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActionInfo {
    pub id: ActionId,
    pub family: ActionFamily,
    pub label: String,
    pub description: String,
    pub available: bool,
    /// Why it is unavailable right now, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    pub arguments: Vec<ActionArgument>,
    /// The revision a mutation of this action must carry, when it needs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precondition_scope: Option<crate::app::snapshot::RevisionScope>,
}

/// Every mutation the core accepts, whichever frontend asked for it.
///
/// Reads (`session/read`, `conversation/read`, the catalogs) are not commands:
/// they take a snapshot cut rather than changing one. The request envelope that
/// carries a command to the actor, and the reply channel it comes back on,
/// arrive with the actor in B2.
///
/// It has no serialization of its own, for the same reason [`crate::app::event::AppEvent`]
/// has none: the wire form of a mutation is the request that carried it
/// (`ClientRequest`), and a second encoding would be a second contract.
#[derive(Debug, Clone, PartialEq)]
pub enum AppCommand {
    StartSession {
        cwd: Option<PathBuf>,
        provider: Option<String>,
        model: Option<String>,
        thinking: Option<ThinkingLevel>,
        permission_mode: Option<PermissionMode>,
    },
    ResumeSession {
        locator: SessionLocator,
    },
    CloseSession,
    DeleteSession {
        locator: SessionLocator,
    },
    /// Submit input to a conversation. The one deep path: routing, queueing, and
    /// steering are decided behind it.
    Submit {
        conversation_id: ConversationId,
        input: Submission,
    },
    Execute {
        origin_conversation_id: ConversationId,
        precondition: Option<ResourceRevision>,
        action: Action,
    },
    Interrupt {
        conversation_id: ConversationId,
        turn_id: TurnId,
    },
    RespondInteraction {
        interaction_id: InteractionId,
        activation: ActivationKind,
        decision: InteractionDecision,
    },
    /// Advance attention cursors. Only this does; reading a conversation never
    /// does.
    MarkRead {
        conversation_id: ConversationId,
        last_item_id: Option<ItemId>,
        last_room_seq: Option<u64>,
        expected_revision: u64,
    },
    /// Pull back the newest queue entry, losing the race to a barrier that
    /// already absorbed it.
    ReclaimQueueTail {
        conversation_id: ConversationId,
        expected_revision: Option<u64>,
    },
    RegisterAsset {
        path: PathBuf,
        expected_mime: Option<String>,
        expected_sha256: Option<String>,
    },
    /// Interrupt active work, resolve interactions fail-closed, persist, exit.
    Shutdown,
}

/// A read the core answers from an atomic cut of its state. Not a command: it
/// changes nothing, including attention.
#[derive(Debug, Clone, PartialEq)]
pub enum AppQuery {
    ListSessions {
        cursor: Option<String>,
        limit: Option<u32>,
    },
    ReadSession,
    ListConversations {
        cursor: Option<String>,
        limit: Option<u32>,
    },
    ReadConversation {
        conversation_id: ConversationId,
        cursor: Option<ItemCursor>,
        limit: Option<u32>,
    },
    ReadQueue {
        conversation_id: ConversationId,
        cursor: Option<String>,
        limit: Option<u32>,
    },
    ListActions {
        origin_conversation_id: Option<ConversationId>,
    },
    ReadConfig,
    /// Answerable before a session exists: a GUI picks its provider and model on
    /// the way in.
    ReadCatalog {
        catalog: crate::app::snapshot::CatalogKind,
        provider: Option<String>,
        cursor: Option<String>,
        limit: Option<u32>,
    },
    ReadResource {
        resource: crate::app::snapshot::ResourceKind,
        cursor: Option<String>,
        limit: Option<u32>,
    },
    ReadAssetChunk {
        asset_id: AssetId,
        offset: u64,
        length: u32,
    },
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::app::snapshot::{RewindMode, ThemeChoice};

    /// Every action, once. The list is the fixture other tests and B5's registry
    /// completeness check read from.
    pub(crate) fn every_action() -> Vec<Action> {
        vec![
            Action::SessionReset,
            Action::SessionRename {
                name: "Renamed".to_string(),
            },
            Action::SessionGarbageCollect,
            Action::SessionShare {
                public: false,
                open: false,
                output: None,
            },
            Action::SessionChangeDirectory {
                path: PathBuf::from("/tmp"),
            },
            Action::ConversationCompact { instructions: None },
            Action::ConversationRewind {
                target: RewindTarget::Latest,
                mode: RewindMode::Preview,
            },
            Action::ModelSelect {
                model: "sonnet".to_string(),
            },
            Action::ProviderSelect {
                provider: "default".to_string(),
            },
            Action::ProviderLogin {
                provider: "default".to_string(),
            },
            Action::ProviderLogout {
                provider: "default".to_string(),
            },
            Action::ThinkingSelect {
                level: ThinkingLevel::High,
            },
            Action::PermissionModeSet {
                mode: PermissionMode::Default,
            },
            Action::PermissionRuleAdd {
                decision: PermissionRuleDecision::Allow,
                rule: "Bash(cargo test:*)".to_string(),
            },
            Action::PermissionRuleRemove {
                decision: PermissionRuleDecision::Allow,
                rule: "Bash(cargo test:*)".to_string(),
            },
            Action::McpEnable {
                server: "docs".to_string(),
            },
            Action::McpDisable {
                server: "docs".to_string(),
            },
            Action::McpReconnect { server: None },
            Action::SkillInvoke {
                skill: "guide".to_string(),
                input: None,
            },
            Action::TeamStart { members: None },
            Action::TeamAssign {
                member: "scout".to_string(),
                task: "survey the crate".to_string(),
            },
            Action::TeamStop { member: None },
            Action::RoomJoin {
                room: "#design".to_string(),
            },
            Action::RoomLeave {
                room: "#design".to_string(),
            },
            Action::CommandPromote {
                item_id: ItemId::new("item_1"),
            },
            Action::ThemeSet {
                theme: ThemeChoice::Dark,
            },
        ]
    }

    #[test]
    fn every_action_has_a_distinct_stable_id() {
        let actions = every_action();
        assert_eq!(
            actions.len(),
            26,
            "the action union is a contract; adding a family is a decision"
        );
        let mut ids: Vec<String> = actions.iter().map(|a| a.id().0).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), actions.len(), "two actions share an id");
        for action in &actions {
            let id = action.id();
            assert!(
                id.as_str().contains('.') && id.as_str().is_ascii(),
                "{id} must be a dotted ascii name"
            );
        }
    }

    #[test]
    fn an_action_is_tagged_by_type() {
        let action = Action::SessionRename {
            name: "Renamed".to_string(),
        };
        let value = serde_json::to_value(&action).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            value,
            serde_json::json!({"type": "sessionRename", "name": "Renamed"})
        );
        let decoded: Action =
            serde_json::from_value(value).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(decoded, action);
    }

    #[test]
    fn a_submission_names_its_shape() {
        let submission = Submission::Composer {
            mode: ComposerMode::Normal,
            text: "Run the tests".to_string(),
            attachments: Vec::new(),
        };
        assert_eq!(
            serde_json::to_value(&submission).unwrap_or_else(|error| panic!("{error}")),
            serde_json::json!({
                "type": "composer",
                "mode": "normal",
                "text": "Run the tests",
                "attachments": []
            })
        );
    }
}
