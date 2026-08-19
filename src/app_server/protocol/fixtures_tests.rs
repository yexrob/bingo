//! One JSON fixture per request, result, notification, and error variant.
//!
//! Each asserts two directions: the typed value serializes to exactly this JSON,
//! and this JSON deserializes back to exactly that value. A field renamed by
//! accident fails here rather than in a client.

use serde_json::{Value, json};

use crate::app::command::{
    Action, ActionArgument, ActionFamily, ActionId, ActionInfo, ActionResult, ActionResultStatus,
    ArgumentKind, ComposerMode, RewindTarget, Submission, SubmitDisposition,
};
use crate::app::event::*;
use crate::app::ids::*;
use crate::app::snapshot::*;
use crate::app_server::protocol::envelope::{
    ClientNotificationFrame, NotificationFrame, RequestFrame, ResponseFrame,
};
use crate::app_server::protocol::error::{ErrorData, ErrorScope, ProtocolErrorKind, RpcError};
use crate::app_server::protocol::notifications::{NotificationParams, ServerNotification};
use crate::app_server::protocol::requests::*;

const TS: u64 = 1_760_000_000_000;

fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or_else(|error| panic!("{error}"))
}

fn from_value<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap_or_else(|error| panic!("{error}"))
}

// ---------------------------------------------------------------------------
// Sample resources, each paired with the JSON it must produce
// ---------------------------------------------------------------------------

fn session_locator() -> SessionLocator {
    SessionLocator::Stem {
        stem: "bingo-1".to_string(),
    }
}

fn session_locator_json() -> Value {
    json!({"type": "stem", "stem": "bingo-1"})
}

fn session_summary() -> SessionSummary {
    SessionSummary {
        id: SessionId::new("sess_1"),
        epoch: EpochId::new("epoch_1"),
        title: "bingo".to_string(),
        state: SessionState::Active,
        cwd: "/repo".into(),
        locator: session_locator(),
        provider: "default".to_string(),
        model: "sonnet".to_string(),
        thinking: ThinkingLevel::Off,
        permission_mode: PermissionMode::Default,
        created_at: TS,
        updated_at: TS,
        resumed: false,
    }
}

fn session_summary_json() -> Value {
    json!({
        "id": "sess_1",
        "epoch": "epoch_1",
        "title": "bingo",
        "state": "active",
        "cwd": "/repo",
        "locator": session_locator_json(),
        "provider": "default",
        "model": "sonnet",
        "thinking": "off",
        "permissionMode": "default",
        "createdAt": TS,
        "updatedAt": TS,
        "resumed": false
    })
}

fn conversation_summary() -> ConversationSummary {
    ConversationSummary {
        id: ConversationId::new("conv_main"),
        kind: ConversationKind::Main,
        title: "main".to_string(),
        revision: 4,
        history_generation: 1,
        run_state: ConversationRunState::Running,
        active_turn_id: Some(TurnId::new("turn_9")),
        unread: 0,
        mentions: 0,
        read_cursor: Some(ItemId::new("item_11")),
        last_item_id: Some(ItemId::new("item_12")),
        obligations: vec![Obligation {
            kind: ObligationKind::AwaitingUser,
            from: Some("scout".to_string()),
            item_id: Some(ItemId::new("item_10")),
            since: TS,
        }],
        is_member: true,
        queue_revision: 2,
        queue_count: 1,
        pending_interactions: 0,
        last_activity_at: Some(TS),
    }
}

fn conversation_summary_json() -> Value {
    json!({
        "id": "conv_main",
        "kind": {"type": "main"},
        "title": "main",
        "revision": 4,
        "historyGeneration": 1,
        "runState": "running",
        "activeTurnId": "turn_9",
        "unread": 0,
        "mentions": 0,
        "readCursor": "item_11",
        "lastItemId": "item_12",
        "obligations": [{
            "kind": "awaitingUser",
            "from": "scout",
            "itemId": "item_10",
            "since": TS
        }],
        "isMember": true,
        "queueRevision": 2,
        "queueCount": 1,
        "pendingInteractions": 0,
        "lastActivityAt": TS
    })
}

fn item() -> Item {
    Item {
        id: ItemId::new("item_12"),
        status: ItemStatus::Completed,
        turn_id: Some(TurnId::new("turn_9")),
        started_at: Some(TS),
        completed_at: Some(TS + 1),
        body: ItemBody::AssistantMessage {
            text: "I will run the tests.".to_string(),
        },
    }
}

fn item_json() -> Value {
    json!({
        "id": "item_12",
        "status": "completed",
        "turnId": "turn_9",
        "startedAt": TS,
        "completedAt": TS + 1,
        "type": "assistantMessage",
        "text": "I will run the tests."
    })
}

fn turn_usage() -> TurnUsage {
    TurnUsage {
        input_tokens: 120,
        output_tokens: 34,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        authoritative: true,
    }
}

fn turn_usage_json() -> Value {
    json!({
        "inputTokens": 120,
        "outputTokens": 34,
        "cacheReadTokens": 0,
        "cacheWriteTokens": 0,
        "authoritative": true
    })
}

fn turn() -> Turn {
    Turn {
        id: TurnId::new("turn_9"),
        conversation_id: ConversationId::new("conv_main"),
        status: TurnStatus::Running,
        origin: TurnOrigin::User,
        round: 1,
        input_item_ids: vec![ItemId::new("item_11")],
        started_at: TS,
        completed_at: None,
        usage: Some(turn_usage()),
        error: None,
    }
}

fn turn_json() -> Value {
    json!({
        "id": "turn_9",
        "conversationId": "conv_main",
        "status": "running",
        "origin": "user",
        "round": 1,
        "inputItemIds": ["item_11"],
        "startedAt": TS,
        "usage": turn_usage_json()
    })
}

fn queue_entry() -> QueueEntry {
    QueueEntry {
        id: QueueId::new("queue_1"),
        origin_conversation_id: ConversationId::new("conv_main"),
        text: "and then lint".to_string(),
        attachments: Vec::new(),
        steer_eligible: true,
        queued_at: TS,
    }
}

fn queue_entry_json() -> Value {
    json!({
        "id": "queue_1",
        "originConversationId": "conv_main",
        "text": "and then lint",
        "attachments": [],
        "steerEligible": true,
        "queuedAt": TS
    })
}

fn interaction() -> Interaction {
    Interaction {
        id: InteractionId::new("int_3"),
        conversation_id: ConversationId::new("conv_main"),
        turn_id: Some(TurnId::new("turn_9")),
        item_id: Some(ItemId::new("item_12")),
        opened_at: TS,
        remaining_guard_ms: 400,
        prompt: InteractionPrompt::Permission {
            title: "Allow running Bash".to_string(),
            reason: Some("Run the test suite".to_string()),
            tool: ToolRequest {
                name: "Bash".to_string(),
                input: json!({"command": "cargo test"}),
            },
            preview: Some(InteractionPreview::Command {
                command: "cargo test".to_string(),
            }),
            decisions: vec![
                PermissionDecisionKind::AllowOnce,
                PermissionDecisionKind::AllowSession,
                PermissionDecisionKind::Deny,
            ],
            session_scope: Some(SessionScope {
                id: ScopeId::new("scope_8"),
                label: "Bash: cargo test".to_string(),
            }),
            allows_feedback: true,
        },
    }
}

fn interaction_json() -> Value {
    json!({
        "id": "int_3",
        "conversationId": "conv_main",
        "turnId": "turn_9",
        "itemId": "item_12",
        "openedAt": TS,
        "remainingGuardMs": 400,
        "prompt": {
            "type": "permission",
            "title": "Allow running Bash",
            "reason": "Run the test suite",
            "tool": {"name": "Bash", "input": {"command": "cargo test"}},
            "preview": {"type": "command", "command": "cargo test"},
            "decisions": ["allowOnce", "allowSession", "deny"],
            "sessionScope": {"id": "scope_8", "label": "Bash: cargo test"},
            "allowsFeedback": true
        }
    })
}

fn operation() -> Operation {
    Operation {
        id: OperationId::new("op_1"),
        kind: OperationKind::TeamStart,
        status: OperationStatus::Running,
        conversation_id: Some(ConversationId::new("conv_main")),
        progress: Some(operation_progress()),
        started_at: TS,
        completed_at: None,
        error: None,
    }
}

fn operation_json() -> Value {
    json!({
        "id": "op_1",
        "kind": "teamStart",
        "status": "running",
        "conversationId": "conv_main",
        "progress": operation_progress_json(),
        "startedAt": TS
    })
}

fn operation_progress() -> OperationProgress {
    OperationProgress {
        label: "Starting the crew".to_string(),
        done: Some(1),
        total: Some(3),
    }
}

fn operation_progress_json() -> Value {
    json!({"label": "Starting the crew", "done": 1, "total": 3})
}

fn agent_resource() -> AgentResource {
    AgentResource {
        id: AgentId::new("agent_1"),
        name: "scout".to_string(),
        def: Some("explorer".to_string()),
        description: "Surveys the crate".to_string(),
        kind: AgentKind::Crew,
        state: AgentState::Running,
        model: "sonnet".to_string(),
        provider: "default".to_string(),
        thinking: ThinkingLevel::Medium,
        cwd: "/repo".into(),
        conversation_id: Some(ConversationId::new("conv_scout")),
        pending: 0,
        unacked: 1,
        elapsed_ms: Some(4200),
        output_tokens: 512,
        tool_uses: 3,
        last_active_at: TS,
    }
}

fn agent_resource_json() -> Value {
    json!({
        "id": "agent_1",
        "name": "scout",
        "def": "explorer",
        "description": "Surveys the crate",
        "kind": "crew",
        "state": "running",
        "model": "sonnet",
        "provider": "default",
        "thinking": "medium",
        "cwd": "/repo",
        "conversationId": "conv_scout",
        "pending": 0,
        "unacked": 1,
        "elapsedMs": 4200,
        "outputTokens": 512,
        "toolUses": 3,
        "lastActiveAt": TS
    })
}

fn room_resource() -> RoomResource {
    RoomResource {
        id: RoomId::new("room_1"),
        name: "#design".to_string(),
        topic: Some("the app-server".to_string()),
        mode: RoomMode::Broadcast,
        members: vec!["scout".to_string()],
        user_is_member: true,
        conversation_id: Some(ConversationId::new("conv_design")),
        message_count: 12,
        last_seq: 12,
        unread: 2,
        mentions: 1,
    }
}

fn room_resource_json() -> Value {
    json!({
        "id": "room_1",
        "name": "#design",
        "topic": "the app-server",
        "mode": "broadcast",
        "members": ["scout"],
        "userIsMember": true,
        "conversationId": "conv_design",
        "messageCount": 12,
        "lastSeq": 12,
        "unread": 2,
        "mentions": 1
    })
}

fn task_resource() -> TaskResource {
    TaskResource {
        id: TaskId::new("task_1"),
        subject: "Land the contract".to_string(),
        description: "B1".to_string(),
        status: TaskStatus::InProgress,
        owner: Some("scout".to_string()),
        active_form: Some("Landing the contract".to_string()),
        blocks: vec![TaskId::new("task_2")],
        blocked_by: Vec::new(),
    }
}

fn task_resource_json() -> Value {
    json!({
        "id": "task_1",
        "subject": "Land the contract",
        "description": "B1",
        "status": "inProgress",
        "owner": "scout",
        "activeForm": "Landing the contract",
        "blocks": ["task_2"],
        "blockedBy": []
    })
}

fn delivery_resource() -> DeliveryResource {
    DeliveryResource {
        id: DeliveryId::new("dm_1"),
        from: "main".to_string(),
        to: "scout".to_string(),
        private: true,
        state: DeliveryState::Delivered,
        message_item_id: Some(ItemId::new("item_20")),
        follow_ups: 1,
        max_follow_ups: 3,
        reason: None,
        updated_at: TS,
    }
}

fn delivery_resource_json() -> Value {
    json!({
        "id": "dm_1",
        "from": "main",
        "to": "scout",
        "private": true,
        "state": "delivered",
        "messageItemId": "item_20",
        "followUps": 1,
        "maxFollowUps": 3,
        "updatedAt": TS
    })
}

fn background_command() -> BackgroundCommandResource {
    BackgroundCommandResource {
        id: CommandId::new("cmd_1"),
        label: "cargo test".to_string(),
        command: "cargo test".to_string(),
        state: BackgroundCommandState::Running,
        started_at: TS,
        duration_ms: 900,
        exit_code: None,
        conversation_id: Some(ConversationId::new("conv_main")),
        item_id: Some(ItemId::new("item_13")),
    }
}

fn background_command_json() -> Value {
    json!({
        "id": "cmd_1",
        "label": "cargo test",
        "command": "cargo test",
        "state": "running",
        "startedAt": TS,
        "durationMs": 900,
        "conversationId": "conv_main",
        "itemId": "item_13"
    })
}

fn mcp_server() -> McpServerState {
    McpServerState {
        name: "docs".to_string(),
        enabled: true,
        status: McpStatus::Connected,
        tools: 4,
        error: None,
    }
}

fn mcp_server_json() -> Value {
    json!({"name": "docs", "enabled": true, "status": "connected", "tools": 4})
}

fn asset_record() -> AssetRecord {
    AssetRecord {
        id: AssetId::new("asset_1"),
        kind: AssetKind::Image,
        origin: AssetOrigin::Session,
        mime: "image/png".to_string(),
        bytes: 2048,
        sha256: "abc123".to_string(),
        width: Some(64),
        height: Some(32),
        created_at: TS,
    }
}

fn asset_record_json() -> Value {
    json!({
        "id": "asset_1",
        "kind": "image",
        "origin": "session",
        "mime": "image/png",
        "bytes": 2048,
        "sha256": "abc123",
        "width": 64,
        "height": 32,
        "createdAt": TS
    })
}

fn feedback() -> Feedback {
    Feedback {
        id: FeedbackId::new("fb_1"),
        level: NoticeLevel::Warning,
        code: "SERVER_ERROR".to_string(),
        message: "MCP server docs did not connect.".to_string(),
        detail: None,
        conversation_id: None,
        raised_at: TS,
        expires_at: Some(TS + 10_000),
    }
}

fn feedback_json() -> Value {
    json!({
        "id": "fb_1",
        "level": "warning",
        "code": "SERVER_ERROR",
        "message": "MCP server docs did not connect.",
        "raisedAt": TS,
        "expiresAt": TS + 10_000u64
    })
}

fn config_snapshot() -> ConfigSnapshot {
    ConfigSnapshot {
        revision: 3,
        model: "sonnet".to_string(),
        provider: "default".to_string(),
        thinking: ThinkingLevel::Off,
        permission_mode: PermissionMode::Default,
        theme: ThemeChoice::Auto,
        cwd: "/repo".into(),
        shell: "/bin/zsh".to_string(),
        shell_dialect: ShellDialect::Posix,
        permissions: vec![PermissionRule {
            decision: PermissionRuleDecision::Allow,
            rule: "Bash(cargo test:*)".to_string(),
            session_scoped: true,
        }],
        layers: vec![ConfigLayer {
            path: "/repo/.bingo/settings.json".into(),
            keys: vec!["model".to_string()],
        }],
        mcp_servers: vec![mcp_server()],
    }
}

fn config_snapshot_json() -> Value {
    json!({
        "revision": 3,
        "model": "sonnet",
        "provider": "default",
        "thinking": "off",
        "permissionMode": "default",
        "theme": "auto",
        "cwd": "/repo",
        "shell": "/bin/zsh",
        "shellDialect": "posix",
        "permissions": [{
            "decision": "allow",
            "rule": "Bash(cargo test:*)",
            "sessionScoped": true
        }],
        "layers": [{"path": "/repo/.bingo/settings.json", "keys": ["model"]}],
        "mcpServers": [mcp_server_json()]
    })
}

fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        multi_conversation: true,
        reasoning: true,
        images: true,
        teams: true,
        rooms: true,
        shell: true,
    }
}

fn capabilities_json() -> Value {
    json!({
        "multiConversation": true,
        "reasoning": true,
        "images": true,
        "teams": true,
        "rooms": true,
        "shell": true
    })
}

fn session_snapshot() -> SessionSnapshot {
    SessionSnapshot {
        session: session_summary(),
        capabilities: capabilities(),
        conversations: Collection {
            revision: 4,
            count: 1,
            active: vec![conversation_summary()],
        },
        active_turns: vec![turn()],
        interactions: vec![interaction()],
        operations: vec![operation()],
        collections: RuntimeCollections {
            agents: Collection {
                revision: 2,
                count: 1,
                active: vec![agent_resource()],
            },
            rooms: Collection {
                revision: 1,
                count: 1,
                active: vec![room_resource()],
            },
            tasks: Collection {
                revision: 1,
                count: 1,
                active: vec![task_resource()],
            },
            deliveries: Collection {
                revision: 1,
                count: 1,
                active: vec![delivery_resource()],
            },
            background_commands: Collection {
                revision: 1,
                count: 1,
                active: vec![background_command()],
            },
            mcp_servers: vec![mcp_server()],
        },
        feedback: vec![feedback()],
        config: config_snapshot(),
        event_cursor: 101,
    }
}

fn session_snapshot_json() -> Value {
    json!({
        "session": session_summary_json(),
        "capabilities": capabilities_json(),
        "conversations": {"revision": 4, "count": 1, "active": [conversation_summary_json()]},
        "activeTurns": [turn_json()],
        "interactions": [interaction_json()],
        "operations": [operation_json()],
        "collections": {
            "agents": {"revision": 2, "count": 1, "active": [agent_resource_json()]},
            "rooms": {"revision": 1, "count": 1, "active": [room_resource_json()]},
            "tasks": {"revision": 1, "count": 1, "active": [task_resource_json()]},
            "deliveries": {"revision": 1, "count": 1, "active": [delivery_resource_json()]},
            "backgroundCommands": {
                "revision": 1,
                "count": 1,
                "active": [background_command_json()]
            },
            "mcpServers": [mcp_server_json()]
        },
        "feedback": [feedback_json()],
        "config": config_snapshot_json(),
        "eventCursor": 101
    })
}

fn conversation_snapshot() -> ConversationSnapshot {
    ConversationSnapshot {
        conversation: conversation_summary(),
        items: Page {
            items: vec![item()],
            revision: 4,
            next_cursor: Some("item_11".to_string()),
        },
        history_generation: 1,
        active_turn: Some(turn()),
        queue: Page {
            items: vec![queue_entry()],
            revision: 2,
            next_cursor: None,
        },
        interactions: vec![interaction()],
        context_usage: Some(ContextUsage {
            used: 12_000,
            window: 200_000,
            trigger: 160_000,
        }),
        event_cursor: 101,
    }
}

fn conversation_snapshot_json() -> Value {
    json!({
        "conversation": conversation_summary_json(),
        "items": {"items": [item_json()], "revision": 4, "nextCursor": "item_11"},
        "historyGeneration": 1,
        "activeTurn": turn_json(),
        "queue": {"items": [queue_entry_json()], "revision": 2},
        "interactions": [interaction_json()],
        "contextUsage": {"used": 12000, "window": 200000, "trigger": 160000},
        "eventCursor": 101
    })
}

fn event_meta() -> EventMeta {
    EventMeta {
        seq: 101,
        ts: TS,
        session_id: SessionId::new("sess_1"),
        caused_by: None,
        coalesced_from: None,
    }
}

fn event_meta_json() -> Value {
    json!({"seq": 101, "ts": TS, "sessionId": "sess_1"})
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// Every request variant and the frame it must serialize to.
fn every_request() -> Vec<(ClientRequest, Value)> {
    vec![
        (
            ClientRequest::Initialize(InitializeParams {
                protocol: ProtocolRange {
                    major: 1,
                    min_minor: 0,
                    max_minor: 0,
                },
                client: ClientInfo {
                    name: "bingo-gui".to_string(),
                    version: "0.1.0".to_string(),
                },
                capabilities: ClientCapabilities {
                    interaction_response: true,
                    experimental: Vec::new(),
                },
            }),
            json!({
                "protocol": {"major": 1, "minMinor": 0, "maxMinor": 0},
                "client": {"name": "bingo-gui", "version": "0.1.0"},
                "capabilities": {"interactionResponse": true, "experimental": []}
            }),
        ),
        (ClientRequest::Shutdown(ShutdownParams {}), json!({})),
        (
            ClientRequest::SessionList(SessionListParams {
                cursor: Some("bingo-2".to_string()),
                limit: Some(20),
            }),
            json!({"cursor": "bingo-2", "limit": 20}),
        ),
        (
            ClientRequest::SessionStart(SessionStartParams {
                cwd: Some("/repo".into()),
                provider: Some("default".to_string()),
                model: Some("sonnet".to_string()),
                thinking: Some(ThinkingLevel::High),
                permission_mode: Some(PermissionMode::AcceptEdits),
            }),
            json!({
                "cwd": "/repo",
                "provider": "default",
                "model": "sonnet",
                "thinking": "high",
                "permissionMode": "acceptEdits"
            }),
        ),
        (
            ClientRequest::SessionResume(SessionResumeParams {
                locator: session_locator(),
            }),
            json!({"locator": session_locator_json()}),
        ),
        (ClientRequest::SessionRead(SessionReadParams {}), json!({})),
        (
            ClientRequest::SessionClose(SessionCloseParams {}),
            json!({}),
        ),
        (
            ClientRequest::SessionDelete(SessionDeleteParams {
                locator: SessionLocator::Latest,
            }),
            json!({"locator": {"type": "latest"}}),
        ),
        (
            ClientRequest::ConversationList(ConversationListParams {
                cursor: None,
                limit: Some(50),
            }),
            json!({"limit": 50}),
        ),
        (
            ClientRequest::ConversationRead(ConversationReadParams {
                conversation_id: ConversationId::new("conv_main"),
                cursor: Some(ItemCursor {
                    history_generation: 1,
                    after: ItemId::new("item_11"),
                }),
                limit: Some(100),
            }),
            json!({
                "conversationId": "conv_main",
                "cursor": {"historyGeneration": 1, "after": "item_11"},
                "limit": 100
            }),
        ),
        (
            ClientRequest::ConversationMarkRead(ConversationMarkReadParams {
                conversation_id: ConversationId::new("conv_main"),
                last_item_id: Some(ItemId::new("item_12")),
                last_room_seq: None,
                expected_revision: 4,
            }),
            json!({
                "conversationId": "conv_main",
                "lastItemId": "item_12",
                "expectedRevision": 4
            }),
        ),
        (
            ClientRequest::ConversationSubmit(ConversationSubmitParams {
                conversation_id: ConversationId::new("conv_main"),
                input: Submission::Composer {
                    mode: ComposerMode::Normal,
                    text: "Run the tests".to_string(),
                    attachments: vec![AssetId::new("asset_1")],
                },
            }),
            json!({
                "conversationId": "conv_main",
                "input": {
                    "type": "composer",
                    "mode": "normal",
                    "text": "Run the tests",
                    "attachments": ["asset_1"]
                }
            }),
        ),
        (
            ClientRequest::TurnInterrupt(TurnInterruptParams {
                conversation_id: ConversationId::new("conv_main"),
                turn_id: TurnId::new("turn_9"),
            }),
            json!({"conversationId": "conv_main", "turnId": "turn_9"}),
        ),
        (
            ClientRequest::QueueRead(QueueReadParams {
                conversation_id: ConversationId::new("conv_main"),
                cursor: None,
                limit: None,
            }),
            json!({"conversationId": "conv_main"}),
        ),
        (
            ClientRequest::QueueReclaimTail(QueueReclaimTailParams {
                conversation_id: ConversationId::new("conv_main"),
                expected_revision: Some(2),
            }),
            json!({"conversationId": "conv_main", "expectedRevision": 2}),
        ),
        (
            ClientRequest::InteractionRespond(InteractionRespondParams {
                interaction_id: InteractionId::new("int_3"),
                activation: ActivationKind::Pointer,
                decision: InteractionDecision::AllowSession {
                    scope_id: ScopeId::new("scope_8"),
                },
            }),
            json!({
                "interactionId": "int_3",
                "activation": "pointer",
                "decision": {"type": "allowSession", "scopeId": "scope_8"}
            }),
        ),
        (
            ClientRequest::ActionList(ActionListParams {
                origin_conversation_id: Some(ConversationId::new("conv_main")),
            }),
            json!({"originConversationId": "conv_main"}),
        ),
        (
            ClientRequest::ActionExecute(ActionExecuteParams {
                origin_conversation_id: ConversationId::new("conv_main"),
                precondition: Some(ResourceRevision {
                    scope: RevisionScope::Config,
                    revision: 3,
                }),
                action: Action::ConversationCompact {
                    instructions: Some("keep the decisions".to_string()),
                },
            }),
            json!({
                "originConversationId": "conv_main",
                "precondition": {"scope": "config", "revision": 3},
                "action": {
                    "type": "conversationCompact",
                    "instructions": "keep the decisions"
                }
            }),
        ),
        (
            ClientRequest::ConfigRead(ConfigReadParams {
                sections: Some(vec![ConfigSection::Selection, ConfigSection::Permissions]),
            }),
            json!({"sections": ["selection", "permissions"]}),
        ),
        (
            ClientRequest::CatalogRead(CatalogReadParams {
                catalog: CatalogKind::Models,
                provider: Some("default".to_string()),
                cursor: None,
                limit: None,
            }),
            json!({"catalog": "models", "provider": "default"}),
        ),
        (
            ClientRequest::ResourceRead(ResourceReadParams {
                resource: ResourceKind::Agents,
                cursor: None,
                limit: Some(25),
            }),
            json!({"resource": "agents", "limit": 25}),
        ),
        (
            ClientRequest::AssetRegisterPath(AssetRegisterPathParams {
                path: "/tmp/shot.png".into(),
                expected_mime: Some("image/png".to_string()),
                expected_sha256: Some("abc123".to_string()),
            }),
            json!({
                "path": "/tmp/shot.png",
                "expectedMime": "image/png",
                "expectedSha256": "abc123"
            }),
        ),
        (
            ClientRequest::AssetReadChunk(AssetReadChunkParams {
                asset_id: AssetId::new("asset_1"),
                offset: 0,
                length: 65_536,
            }),
            json!({"assetId": "asset_1", "offset": 0, "length": 65536}),
        ),
    ]
}

#[test]
fn every_request_variant_round_trips() {
    let requests = every_request();
    assert_eq!(
        requests.len(),
        RequestMethod::ALL.len(),
        "every method needs a fixture"
    );
    for (request, params) in requests {
        let method = request.method();
        let frame = RequestFrame::new(7, request.clone());
        let expected = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": method.as_str(),
            "params": params
        });
        assert_eq!(to_value(&frame), expected, "{}", method.as_str());
        let decoded: RequestFrame = from_value(expected);
        assert_eq!(decoded, frame, "{}", method.as_str());
        assert_eq!(decoded.call, request);
    }
}

#[test]
fn the_client_notification_round_trips() {
    let frame = ClientNotificationFrame::new(ClientNotification::Initialized(InitializedParams {}));
    let expected = json!({"jsonrpc": "2.0", "method": "initialized", "params": {}});
    assert_eq!(to_value(&frame), expected);
    let decoded: ClientNotificationFrame = from_value(expected);
    assert_eq!(decoded, frame);
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

fn every_result() -> Vec<(ResponseResult, Value)> {
    vec![
        (
            ResponseResult::Initialize(InitializeResult {
                protocol: ProtocolVersion { major: 1, minor: 0 },
                server: ServerInfo {
                    name: "bingo".to_string(),
                    version: "0.4.0".to_string(),
                    epoch: EpochId::new("epoch_1"),
                },
                limits: FrameLimits {
                    max_client_frame_bytes: 1_048_576,
                    max_server_frame_bytes: 8_388_608,
                },
                capabilities: capabilities(),
            }),
            json!({
                "protocol": {"major": 1, "minor": 0},
                "server": {"name": "bingo", "version": "0.4.0", "epoch": "epoch_1"},
                "limits": {
                    "maxClientFrameBytes": 1_048_576,
                    "maxServerFrameBytes": 8_388_608
                },
                "capabilities": capabilities_json()
            }),
        ),
        (
            ResponseResult::Shutdown(ShutdownResult {
                interrupted_turns: 1,
                denied_interactions: 0,
            }),
            json!({"interruptedTurns": 1, "deniedInteractions": 0}),
        ),
        (
            ResponseResult::SessionList(SessionListResult {
                sessions: Page {
                    items: vec![SessionListEntry {
                        locator: session_locator(),
                        title: "bingo".to_string(),
                        cwd: "/repo".into(),
                        updated_at: TS,
                        message_count: 12,
                        open: true,
                    }],
                    revision: 1,
                    next_cursor: None,
                },
            }),
            json!({
                "sessions": {
                    "items": [{
                        "locator": session_locator_json(),
                        "title": "bingo",
                        "cwd": "/repo",
                        "updatedAt": TS,
                        "messageCount": 12,
                        "open": true
                    }],
                    "revision": 1
                }
            }),
        ),
        (
            ResponseResult::SessionStart(SessionStartResult {
                snapshot: session_snapshot(),
            }),
            json!({"snapshot": session_snapshot_json()}),
        ),
        (
            ResponseResult::SessionResume(SessionResumeResult {
                snapshot: session_snapshot(),
            }),
            json!({"snapshot": session_snapshot_json()}),
        ),
        (
            ResponseResult::SessionRead(SessionReadResult {
                snapshot: session_snapshot(),
            }),
            json!({"snapshot": session_snapshot_json()}),
        ),
        (
            ResponseResult::SessionClose(SessionCloseResult {
                session_id: SessionId::new("sess_1"),
            }),
            json!({"sessionId": "sess_1"}),
        ),
        (
            ResponseResult::SessionDelete(SessionDeleteResult {
                locator: session_locator(),
                deleted: true,
            }),
            json!({"locator": session_locator_json(), "deleted": true}),
        ),
        (
            ResponseResult::ConversationList(ConversationListResult {
                conversations: Page {
                    items: vec![conversation_summary()],
                    revision: 4,
                    next_cursor: None,
                },
            }),
            json!({
                "conversations": {"items": [conversation_summary_json()], "revision": 4}
            }),
        ),
        (
            ResponseResult::ConversationRead(ConversationReadResult {
                snapshot: conversation_snapshot(),
            }),
            json!({"snapshot": conversation_snapshot_json()}),
        ),
        (
            ResponseResult::ConversationMarkRead(ConversationMarkReadResult {
                conversation: conversation_summary(),
            }),
            json!({"conversation": conversation_summary_json()}),
        ),
        (
            ResponseResult::ConversationSubmit(ConversationSubmitResult {
                disposition: SubmitDisposition::TurnStarted {
                    turn_id: TurnId::new("turn_9"),
                },
            }),
            json!({"disposition": {"type": "turnStarted", "turnId": "turn_9"}}),
        ),
        (
            ResponseResult::TurnInterrupt(TurnInterruptResult {
                turn_id: TurnId::new("turn_9"),
                accepted: true,
            }),
            json!({"turnId": "turn_9", "accepted": true}),
        ),
        (
            ResponseResult::QueueRead(QueueReadResult {
                entries: Page {
                    items: vec![queue_entry()],
                    revision: 2,
                    next_cursor: None,
                },
                count: 1,
            }),
            json!({
                "entries": {"items": [queue_entry_json()], "revision": 2},
                "count": 1
            }),
        ),
        (
            ResponseResult::QueueReclaimTail(QueueReclaimTailResult {
                outcome: ReclaimOutcome::AlreadyAbsorbed {
                    queue_id: QueueId::new("queue_1"),
                },
            }),
            json!({"outcome": {"type": "alreadyAbsorbed", "queueId": "queue_1"}}),
        ),
        (
            ResponseResult::InteractionRespond(InteractionRespondResult {
                status: RespondStatus::Accepted,
                item_id: Some(ItemId::new("item_14")),
            }),
            json!({"status": "accepted", "itemId": "item_14"}),
        ),
        (
            ResponseResult::ActionList(ActionListResult {
                actions: vec![ActionInfo {
                    id: ActionId::from("session.rename"),
                    family: ActionFamily::Session,
                    label: "Rename session".to_string(),
                    description: "Rename the current session".to_string(),
                    available: true,
                    unavailable_reason: None,
                    arguments: vec![ActionArgument {
                        name: "name".to_string(),
                        kind: ArgumentKind::String,
                        required: true,
                        description: "The new name".to_string(),
                        choices: Vec::new(),
                    }],
                    precondition_scope: None,
                }],
                revision: 1,
            }),
            json!({
                "actions": [{
                    "id": "session.rename",
                    "family": "session",
                    "label": "Rename session",
                    "description": "Rename the current session",
                    "available": true,
                    "arguments": [{
                        "name": "name",
                        "kind": "string",
                        "required": true,
                        "description": "The new name",
                        "choices": []
                    }]
                }],
                "revision": 1
            }),
        ),
        (
            ResponseResult::ActionExecute(ActionExecuteResult {
                disposition: SubmitDisposition::Applied {
                    result: ActionResult {
                        status: ActionResultStatus::Applied,
                        revision: Some(ResourceRevision {
                            scope: RevisionScope::Config,
                            revision: 4,
                        }),
                        message: None,
                    },
                },
            }),
            json!({
                "disposition": {
                    "type": "applied",
                    "result": {
                        "status": "applied",
                        "revision": {"scope": "config", "revision": 4}
                    }
                }
            }),
        ),
        (
            ResponseResult::ConfigRead(ConfigReadResult {
                config: config_snapshot(),
            }),
            json!({"config": config_snapshot_json()}),
        ),
        (
            ResponseResult::CatalogRead(CatalogReadResult {
                catalog: Catalog::Models(Page {
                    items: vec![ModelInfo {
                        id: "sonnet".to_string(),
                        provider: "default".to_string(),
                        display_name: Some("Sonnet".to_string()),
                        family: Some("claude".to_string()),
                        context_window: Some(200_000),
                        supports_images: true,
                        supports_thinking: true,
                    }],
                    revision: 1,
                    next_cursor: None,
                }),
            }),
            json!({
                "catalog": {
                    "catalog": "models",
                    "items": [{
                        "id": "sonnet",
                        "provider": "default",
                        "displayName": "Sonnet",
                        "family": "claude",
                        "contextWindow": 200_000,
                        "supportsImages": true,
                        "supportsThinking": true
                    }],
                    "revision": 1
                }
            }),
        ),
        (
            ResponseResult::ResourceRead(ResourceReadResult {
                resource: ResourcePage::Agents(Page {
                    items: vec![agent_resource()],
                    revision: 2,
                    next_cursor: None,
                }),
            }),
            json!({
                "resource": {
                    "resource": "agents",
                    "items": [agent_resource_json()],
                    "revision": 2
                }
            }),
        ),
        (
            ResponseResult::AssetRegisterPath(AssetRegisterPathResult {
                asset: asset_record(),
            }),
            json!({"asset": asset_record_json()}),
        ),
        (
            ResponseResult::AssetReadChunk(AssetReadChunkResult {
                data: "aGk=".to_string(),
                next_offset: 3,
                eof: true,
            }),
            json!({"data": "aGk=", "nextOffset": 3, "eof": true}),
        ),
    ]
}

#[test]
fn every_result_variant_round_trips() {
    let results = every_result();
    assert_eq!(
        results.len(),
        RequestMethod::ALL.len(),
        "every method needs a result fixture"
    );
    for (result, payload) in results {
        let method = result.method();
        let frame = ResponseFrame::result(7, result.clone());
        let expected = json!({"jsonrpc": "2.0", "id": 7, "result": payload.clone()});
        assert_eq!(to_value(&frame), expected, "{}", method.as_str());
        let decoded = ResponseResult::from_value(method, payload)
            .unwrap_or_else(|error| panic!("{}: {error}", method.as_str()));
        assert_eq!(decoded, result, "{}", method.as_str());
    }
}

#[test]
fn a_result_decodes_against_the_method_that_produced_it() {
    let (result, payload) = every_result()
        .into_iter()
        .find(|(result, _)| result.method() == RequestMethod::TurnInterrupt)
        .unwrap_or_else(|| panic!("no turn/interrupt fixture"));
    assert_eq!(
        ResponseResult::from_value(RequestMethod::TurnInterrupt, payload.clone())
            .unwrap_or_else(|error| panic!("{error}")),
        result
    );
    // The same payload against another method is a decode failure, not a
    // silently different result.
    assert!(ResponseResult::from_value(RequestMethod::SessionRead, payload).is_err());
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

fn every_notification() -> Vec<(ServerNotification, Value)> {
    let meta = event_meta;
    vec![
        (
            ServerNotification::SessionUpdated(NotificationParams::new(
                meta(),
                SessionUpdated {
                    session: session_summary(),
                },
            )),
            json!({"event": event_meta_json(), "session": session_summary_json()}),
        ),
        (
            ServerNotification::SessionClosed(NotificationParams::new(
                meta(),
                SessionClosed {
                    session_id: SessionId::new("sess_1"),
                    reason: SessionCloseReason::Requested,
                },
            )),
            json!({"event": event_meta_json(), "sessionId": "sess_1", "reason": "requested"}),
        ),
        (
            ServerNotification::SessionDeleted(NotificationParams::new(
                meta(),
                SessionDeleted {
                    locator: session_locator(),
                },
            )),
            json!({"event": event_meta_json(), "locator": session_locator_json()}),
        ),
        (
            ServerNotification::ConversationCreated(NotificationParams::new(
                meta(),
                ConversationChanged {
                    conversation: conversation_summary(),
                },
            )),
            json!({"event": event_meta_json(), "conversation": conversation_summary_json()}),
        ),
        (
            ServerNotification::ConversationUpdated(NotificationParams::new(
                meta(),
                ConversationChanged {
                    conversation: conversation_summary(),
                },
            )),
            json!({"event": event_meta_json(), "conversation": conversation_summary_json()}),
        ),
        (
            ServerNotification::ConversationRemoved(NotificationParams::new(
                meta(),
                ConversationRemoved {
                    conversation_id: ConversationId::new("conv_scout"),
                },
            )),
            json!({"event": event_meta_json(), "conversationId": "conv_scout"}),
        ),
        (
            ServerNotification::TurnStarted(NotificationParams::new(
                meta(),
                TurnChanged {
                    conversation_id: ConversationId::new("conv_main"),
                    turn: turn(),
                },
            )),
            json!({
                "event": event_meta_json(),
                "conversationId": "conv_main",
                "turn": turn_json()
            }),
        ),
        (
            ServerNotification::TurnRoundStarted(NotificationParams::new(
                meta(),
                TurnRoundStarted {
                    conversation_id: ConversationId::new("conv_main"),
                    turn_id: TurnId::new("turn_9"),
                    round: 2,
                },
            )),
            json!({
                "event": event_meta_json(),
                "conversationId": "conv_main",
                "turnId": "turn_9",
                "round": 2
            }),
        ),
        (
            ServerNotification::TurnRetrying(NotificationParams::new(
                meta(),
                TurnRetrying {
                    conversation_id: ConversationId::new("conv_main"),
                    turn_id: TurnId::new("turn_9"),
                    round: 2,
                    attempt: 1,
                    max_attempts: 3,
                    delay_ms: 800,
                    removed_item_ids: vec![ItemId::new("item_12")],
                    code: Some("STREAM_INTERRUPTED".to_string()),
                    reason: Some("The stream ended early.".to_string()),
                },
            )),
            json!({
                "event": event_meta_json(),
                "conversationId": "conv_main",
                "turnId": "turn_9",
                "round": 2,
                "attempt": 1,
                "maxAttempts": 3,
                "delayMs": 800,
                "removedItemIds": ["item_12"],
                "code": "STREAM_INTERRUPTED",
                "reason": "The stream ended early."
            }),
        ),
        (
            ServerNotification::TurnRoundCompleted(NotificationParams::new(
                meta(),
                TurnRoundCompleted {
                    conversation_id: ConversationId::new("conv_main"),
                    turn_id: TurnId::new("turn_9"),
                    round: 2,
                    usage: Some(turn_usage()),
                },
            )),
            json!({
                "event": event_meta_json(),
                "conversationId": "conv_main",
                "turnId": "turn_9",
                "round": 2,
                "usage": turn_usage_json()
            }),
        ),
        (
            ServerNotification::TurnUsageUpdated(NotificationParams::new(
                meta(),
                TurnUsageUpdated {
                    conversation_id: ConversationId::new("conv_main"),
                    turn_id: TurnId::new("turn_9"),
                    usage: turn_usage(),
                    context_usage: Some(ContextUsage {
                        used: 12_000,
                        window: 200_000,
                        trigger: 160_000,
                    }),
                },
            )),
            json!({
                "event": event_meta_json(),
                "conversationId": "conv_main",
                "turnId": "turn_9",
                "usage": turn_usage_json(),
                "contextUsage": {"used": 12000, "window": 200000, "trigger": 160000}
            }),
        ),
        (
            ServerNotification::TurnCompleted(NotificationParams::new(
                meta(),
                TurnChanged {
                    conversation_id: ConversationId::new("conv_main"),
                    turn: Turn {
                        status: TurnStatus::Completed,
                        completed_at: Some(TS + 5),
                        ..turn()
                    },
                },
            )),
            json!({
                "event": event_meta_json(),
                "conversationId": "conv_main",
                "turn": {
                    "id": "turn_9",
                    "conversationId": "conv_main",
                    "status": "completed",
                    "origin": "user",
                    "round": 1,
                    "inputItemIds": ["item_11"],
                    "startedAt": TS,
                    "completedAt": TS + 5,
                    "usage": turn_usage_json()
                }
            }),
        ),
        (
            ServerNotification::ItemStarted(NotificationParams::new(
                meta(),
                ItemChanged {
                    conversation_id: ConversationId::new("conv_main"),
                    turn_id: Some(TurnId::new("turn_9")),
                    item: item(),
                },
            )),
            json!({
                "event": event_meta_json(),
                "conversationId": "conv_main",
                "turnId": "turn_9",
                "item": item_json()
            }),
        ),
        (
            ServerNotification::ItemTextDelta(NotificationParams::new(
                meta(),
                ItemDelta {
                    conversation_id: ConversationId::new("conv_main"),
                    turn_id: Some(TurnId::new("turn_9")),
                    item_id: ItemId::new("item_12"),
                    delta_seq: 1,
                    delta: "I will run ".to_string(),
                },
            )),
            json!({
                "event": event_meta_json(),
                "conversationId": "conv_main",
                "turnId": "turn_9",
                "itemId": "item_12",
                "deltaSeq": 1,
                "delta": "I will run "
            }),
        ),
        (
            ServerNotification::ItemReasoningDelta(NotificationParams::new(
                meta(),
                ItemDelta {
                    conversation_id: ConversationId::new("conv_main"),
                    turn_id: Some(TurnId::new("turn_9")),
                    item_id: ItemId::new("item_13"),
                    delta_seq: 2,
                    delta: "weighing the options".to_string(),
                },
            )),
            json!({
                "event": event_meta_json(),
                "conversationId": "conv_main",
                "turnId": "turn_9",
                "itemId": "item_13",
                "deltaSeq": 2,
                "delta": "weighing the options"
            }),
        ),
        (
            ServerNotification::ItemCommandTailUpdated(NotificationParams::new(
                meta(),
                ItemCommandTailUpdated {
                    conversation_id: ConversationId::new("conv_main"),
                    turn_id: None,
                    item_id: ItemId::new("item_13"),
                    tail: CommandTail {
                        lines: vec!["running 12 tests".to_string()],
                        total_lines: 40,
                    },
                },
            )),
            json!({
                "event": event_meta_json(),
                "conversationId": "conv_main",
                "itemId": "item_13",
                "tail": {"lines": ["running 12 tests"], "totalLines": 40}
            }),
        ),
        (
            ServerNotification::ItemUpdated(NotificationParams::new(
                meta(),
                ItemChanged {
                    conversation_id: ConversationId::new("conv_main"),
                    turn_id: Some(TurnId::new("turn_9")),
                    item: item(),
                },
            )),
            json!({
                "event": event_meta_json(),
                "conversationId": "conv_main",
                "turnId": "turn_9",
                "item": item_json()
            }),
        ),
        (
            ServerNotification::ItemCompleted(NotificationParams::new(
                meta(),
                ItemChanged {
                    conversation_id: ConversationId::new("conv_main"),
                    turn_id: Some(TurnId::new("turn_9")),
                    item: item(),
                },
            )),
            json!({
                "event": event_meta_json(),
                "conversationId": "conv_main",
                "turnId": "turn_9",
                "item": item_json()
            }),
        ),
        (
            ServerNotification::QueueItemAdded(NotificationParams::new(
                meta(),
                QueueItemAdded {
                    conversation_id: ConversationId::new("conv_main"),
                    revision: 3,
                    position: 0,
                    entry: queue_entry(),
                },
            )),
            json!({
                "event": event_meta_json(),
                "conversationId": "conv_main",
                "revision": 3,
                "position": 0,
                "entry": queue_entry_json()
            }),
        ),
        (
            ServerNotification::QueueItemRemoved(NotificationParams::new(
                meta(),
                QueueItemRemoved {
                    conversation_id: ConversationId::new("conv_main"),
                    revision: 4,
                    queue_id: QueueId::new("queue_1"),
                    reason: QueueRemovalReason::Reclaimed,
                },
            )),
            json!({
                "event": event_meta_json(),
                "conversationId": "conv_main",
                "revision": 4,
                "queueId": "queue_1",
                "reason": "reclaimed"
            }),
        ),
        (
            ServerNotification::QueueItemAbsorbed(NotificationParams::new(
                meta(),
                QueueItemAbsorbed {
                    conversation_id: ConversationId::new("conv_main"),
                    revision: 5,
                    queue_id: QueueId::new("queue_1"),
                    turn_id: TurnId::new("turn_9"),
                    item_id: ItemId::new("item_15"),
                },
            )),
            json!({
                "event": event_meta_json(),
                "conversationId": "conv_main",
                "revision": 5,
                "queueId": "queue_1",
                "turnId": "turn_9",
                "itemId": "item_15"
            }),
        ),
        (
            ServerNotification::InteractionOpened(NotificationParams::new(
                meta(),
                InteractionOpened {
                    interaction: interaction(),
                },
            )),
            json!({"event": event_meta_json(), "interaction": interaction_json()}),
        ),
        (
            ServerNotification::InteractionResolved(NotificationParams::new(
                meta(),
                InteractionResolved {
                    interaction_id: InteractionId::new("int_3"),
                    conversation_id: ConversationId::new("conv_main"),
                    decision: InteractionDecision::Deny {
                        feedback: Some("run the unit tests only".to_string()),
                    },
                    item_id: Some(ItemId::new("item_16")),
                },
            )),
            json!({
                "event": event_meta_json(),
                "interactionId": "int_3",
                "conversationId": "conv_main",
                "decision": {"type": "deny", "feedback": "run the unit tests only"},
                "itemId": "item_16"
            }),
        ),
        (
            ServerNotification::InteractionCancelled(NotificationParams::new(
                meta(),
                InteractionCancelled {
                    interaction_id: InteractionId::new("int_3"),
                    conversation_id: ConversationId::new("conv_main"),
                    reason: InteractionCancelReason::Interrupted,
                },
            )),
            json!({
                "event": event_meta_json(),
                "interactionId": "int_3",
                "conversationId": "conv_main",
                "reason": "interrupted"
            }),
        ),
        (
            ServerNotification::AgentChanged(NotificationParams::new(
                meta(),
                AgentChanged {
                    agent: agent_resource(),
                },
            )),
            json!({"event": event_meta_json(), "agent": agent_resource_json()}),
        ),
        (
            ServerNotification::AgentRemoved(NotificationParams::new(
                meta(),
                AgentRemoved {
                    agent_id: AgentId::new("agent_1"),
                },
            )),
            json!({"event": event_meta_json(), "agentId": "agent_1"}),
        ),
        (
            ServerNotification::RoomChanged(NotificationParams::new(
                meta(),
                RoomChanged {
                    room: room_resource(),
                },
            )),
            json!({"event": event_meta_json(), "room": room_resource_json()}),
        ),
        (
            ServerNotification::TaskChanged(NotificationParams::new(
                meta(),
                TaskChanged {
                    task: task_resource(),
                },
            )),
            json!({"event": event_meta_json(), "task": task_resource_json()}),
        ),
        (
            ServerNotification::TaskRemoved(NotificationParams::new(
                meta(),
                TaskRemoved {
                    task_id: TaskId::new("task_1"),
                },
            )),
            json!({"event": event_meta_json(), "taskId": "task_1"}),
        ),
        (
            ServerNotification::DeliveryChanged(NotificationParams::new(
                meta(),
                DeliveryChanged {
                    delivery: delivery_resource(),
                },
            )),
            json!({"event": event_meta_json(), "delivery": delivery_resource_json()}),
        ),
        (
            ServerNotification::CommandChanged(NotificationParams::new(
                meta(),
                CommandChanged {
                    command: background_command(),
                },
            )),
            json!({"event": event_meta_json(), "command": background_command_json()}),
        ),
        (
            ServerNotification::OperationStarted(NotificationParams::new(
                meta(),
                OperationChanged {
                    operation: operation(),
                },
            )),
            json!({"event": event_meta_json(), "operation": operation_json()}),
        ),
        (
            ServerNotification::OperationProgress(NotificationParams::new(
                meta(),
                OperationProgressed {
                    operation_id: OperationId::new("op_1"),
                    progress: operation_progress(),
                },
            )),
            json!({
                "event": event_meta_json(),
                "operationId": "op_1",
                "progress": operation_progress_json()
            }),
        ),
        (
            ServerNotification::OperationCompleted(NotificationParams::new(
                meta(),
                OperationChanged {
                    operation: Operation {
                        status: OperationStatus::Completed,
                        completed_at: Some(TS + 9),
                        progress: None,
                        ..operation()
                    },
                },
            )),
            json!({
                "event": event_meta_json(),
                "operation": {
                    "id": "op_1",
                    "kind": "teamStart",
                    "status": "completed",
                    "conversationId": "conv_main",
                    "startedAt": TS,
                    "completedAt": TS + 9
                }
            }),
        ),
        (
            ServerNotification::ConfigChanged(NotificationParams::new(
                meta(),
                ConfigChanged {
                    config: config_snapshot(),
                },
            )),
            json!({"event": event_meta_json(), "config": config_snapshot_json()}),
        ),
        (
            ServerNotification::CatalogChanged(NotificationParams::new(
                meta(),
                CatalogChanged {
                    catalog: CatalogKind::Providers,
                    revision: 2,
                },
            )),
            json!({"event": event_meta_json(), "catalog": "providers", "revision": 2}),
        ),
        (
            ServerNotification::AssetAvailable(NotificationParams::new(
                meta(),
                AssetAvailable {
                    asset: asset_record(),
                },
            )),
            json!({"event": event_meta_json(), "asset": asset_record_json()}),
        ),
        (
            ServerNotification::FeedbackRaised(NotificationParams::new(
                meta(),
                FeedbackRaised {
                    feedback: feedback(),
                },
            )),
            json!({"event": event_meta_json(), "feedback": feedback_json()}),
        ),
        (
            ServerNotification::FeedbackCleared(NotificationParams::new(
                meta(),
                FeedbackCleared {
                    feedback_id: FeedbackId::new("fb_1"),
                },
            )),
            json!({"event": event_meta_json(), "feedbackId": "fb_1"}),
        ),
    ]
}

#[test]
fn every_notification_variant_round_trips() {
    let notifications = every_notification();
    assert_eq!(
        notifications.len(),
        ServerNotification::METHODS.len(),
        "every notification needs a fixture"
    );
    let mut seen: Vec<&str> = Vec::new();
    for (notification, params) in notifications {
        let method = notification.method();
        assert!(!seen.contains(&method), "{method} has two fixtures");
        seen.push(method);
        let frame = NotificationFrame::new(notification.clone());
        let expected = json!({"jsonrpc": "2.0", "method": method, "params": params});
        assert_eq!(to_value(&frame), expected, "{method}");
        let decoded: NotificationFrame = from_value(expected);
        assert_eq!(decoded, frame, "{method}");
    }
    assert_eq!(seen, ServerNotification::METHODS.to_vec());
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[test]
fn every_declared_error_round_trips() {
    for kind in ProtocolErrorKind::ALL {
        let error = RpcError::application(kind);
        let expected = json!({
            "code": kind.code(),
            "message": kind.message(),
            "data": {
                "bingoCode": kind.bingo_code(),
                "recoverable": kind.recoverable(),
                "scope": to_value(&kind.scope()),
                "suggestedAction": to_value(&kind.suggested_action())
            }
        });
        // A kind with no advice omits the key rather than sending null.
        let expected = if kind.suggested_action().is_none() {
            json!({
                "code": kind.code(),
                "message": kind.message(),
                "data": {
                    "bingoCode": kind.bingo_code(),
                    "recoverable": kind.recoverable(),
                    "scope": to_value(&kind.scope())
                }
            })
        } else {
            expected
        };
        assert_eq!(to_value(&error), expected, "{}", kind.bingo_code());
        let decoded: RpcError = from_value(expected);
        assert_eq!(decoded, error, "{}", kind.bingo_code());
    }
}

#[test]
fn an_error_frame_carries_every_identifier_it_can() {
    let error = RpcError {
        code: ProtocolErrorKind::TurnClosed.code(),
        message: ProtocolErrorKind::TurnClosed.message().to_string(),
        data: Some(ErrorData {
            bingo_code: ProtocolErrorKind::TurnClosed.bingo_code().to_string(),
            recoverable: true,
            scope: ErrorScope::Turn,
            session_id: Some(SessionId::new("sess_1")),
            conversation_id: Some(ConversationId::new("conv_main")),
            turn_id: Some(TurnId::new("turn_9")),
            item_id: Some(ItemId::new("item_12")),
            queue_id: Some(QueueId::new("queue_1")),
            interaction_id: Some(InteractionId::new("int_3")),
            operation_id: Some(OperationId::new("op_1")),
            asset_id: Some(AssetId::new("asset_1")),
            suggested_action: Some(
                ProtocolErrorKind::TurnClosed
                    .suggested_action()
                    .unwrap_or(crate::app_server::protocol::error::SuggestedAction::Retry),
            ),
        }),
    };
    let frame = ResponseFrame::error(7, error.clone());
    let expected = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "error": {
            "code": -32010,
            "message": "The turn is no longer active.",
            "data": {
                "bingoCode": "TURN_CLOSED",
                "recoverable": true,
                "scope": "turn",
                "sessionId": "sess_1",
                "conversationId": "conv_main",
                "turnId": "turn_9",
                "itemId": "item_12",
                "queueId": "queue_1",
                "interactionId": "int_3",
                "operationId": "op_1",
                "assetId": "asset_1",
                "suggestedAction": "refreshConversation"
            }
        }
    });
    assert_eq!(to_value(&frame), expected);
    let data = expected["error"].clone();
    let decoded: RpcError = from_value(data);
    assert_eq!(decoded, error);
}

/// A line that could not be read has no id to echo, and JSON-RPC says the reply
/// carries null rather than inventing one.
#[test]
fn a_parse_error_answers_with_the_null_id() {
    use crate::app_server::protocol::envelope::RequestId;
    use crate::app_server::protocol::error::PARSE_ERROR;
    let frame = ResponseFrame::error(
        RequestId::Null,
        RpcError::standard(PARSE_ERROR, "Invalid JSON was received."),
    );
    assert_eq!(
        to_value(&frame),
        json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": {"code": -32700, "message": "Invalid JSON was received."}
        })
    );
}

/// A transport that merged a run of appends says so, so "seq 105 then seq 108"
/// reads as one coalesced frame rather than three lost events.
#[test]
fn a_coalesced_delta_frame_names_the_run_it_stands_for() {
    let notification = ServerNotification::ItemTextDelta(NotificationParams::new(
        EventMeta {
            seq: 108,
            ts: TS,
            session_id: SessionId::new("sess_1"),
            caused_by: None,
            coalesced_from: Some(105),
        },
        ItemDelta {
            conversation_id: ConversationId::new("conv_main"),
            turn_id: Some(TurnId::new("turn_9")),
            item_id: ItemId::new("item_12"),
            delta_seq: 4,
            delta: "I will run the tests".to_string(),
        },
    ));
    let frame = NotificationFrame::new(notification.clone());
    let expected = json!({
        "jsonrpc": "2.0",
        "method": "item/textDelta",
        "params": {
            "event": {
                "seq": 108,
                "ts": TS,
                "sessionId": "sess_1",
                "coalescedFrom": 105
            },
            "conversationId": "conv_main",
            "turnId": "turn_9",
            "itemId": "item_12",
            "deltaSeq": 4,
            "delta": "I will run the tests"
        }
    });
    assert_eq!(to_value(&frame), expected);
    let decoded: NotificationFrame = from_value(expected);
    assert_eq!(decoded, frame);
}

// ---------------------------------------------------------------------------
// Additive evolution
// ---------------------------------------------------------------------------

#[test]
fn unknown_fields_are_ignored_within_a_major_version() {
    let mut request = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "turn/interrupt",
        "params": {
            "conversationId": "conv_main",
            "turnId": "turn_9",
            "reasonFromAFutureMinor": "user pressed escape twice"
        },
        "traceparent": "00-0af7-00f0-01"
    });
    let decoded: RequestFrame = from_value(request.take());
    assert_eq!(
        decoded.call,
        ClientRequest::TurnInterrupt(TurnInterruptParams {
            conversation_id: ConversationId::new("conv_main"),
            turn_id: TurnId::new("turn_9"),
        })
    );

    let notification = json!({
        "jsonrpc": "2.0",
        "method": "queue/itemRemoved",
        "params": {
            "event": {
                "seq": 101,
                "ts": TS,
                "sessionId": "sess_1",
                "originFromAFutureMinor": "watchdog"
            },
            "conversationId": "conv_main",
            "revision": 4,
            "queueId": "queue_1",
            "reason": "reclaimed",
            "byFromAFutureMinor": "user"
        }
    });
    let decoded: NotificationFrame = from_value(notification);
    assert_eq!(decoded.notification.method(), "queue/itemRemoved");

    let result = json!({"turnId": "turn_9", "accepted": true, "graceMsFromAFutureMinor": 50});
    assert_eq!(
        ResponseResult::from_value(RequestMethod::TurnInterrupt, result)
            .unwrap_or_else(|error| panic!("{error}")),
        ResponseResult::TurnInterrupt(TurnInterruptResult {
            turn_id: TurnId::new("turn_9"),
            accepted: true,
        })
    );
}

// ---------------------------------------------------------------------------
// Unions the fixtures above only sample once
// ---------------------------------------------------------------------------

#[test]
fn every_item_body_round_trips() {
    let bodies: Vec<(ItemBody, Value)> = vec![
        (
            ItemBody::UserMessage {
                text: "Run the tests".to_string(),
                attachments: vec![AssetId::new("asset_1")],
            },
            json!({
                "type": "userMessage",
                "text": "Run the tests",
                "attachments": ["asset_1"]
            }),
        ),
        (
            ItemBody::AssistantMessage {
                text: "Running them.".to_string(),
            },
            json!({"type": "assistantMessage", "text": "Running them."}),
        ),
        (
            ItemBody::PeerMessage {
                from: "scout".to_string(),
                to: Some("main".to_string()),
                text: "the crate has two entry points".to_string(),
                delivery_id: Some(DeliveryId::new("dm_1")),
            },
            json!({
                "type": "peerMessage",
                "from": "scout",
                "to": "main",
                "text": "the crate has two entry points",
                "deliveryId": "dm_1"
            }),
        ),
        (
            ItemBody::RoomMessage {
                room_id: RoomId::new("room_1"),
                from: "scout".to_string(),
                text: "@main the contract is ready".to_string(),
                room_seq: 12,
                mentions: vec!["main".to_string()],
            },
            json!({
                "type": "roomMessage",
                "roomId": "room_1",
                "from": "scout",
                "text": "@main the contract is ready",
                "roomSeq": 12,
                "mentions": ["main"]
            }),
        ),
        (
            ItemBody::Reasoning {
                text: "weighing the options".to_string(),
            },
            json!({"type": "reasoning", "text": "weighing the options"}),
        ),
        (
            ItemBody::ToolCall {
                tool_call_id: "toolu_1".to_string(),
                name: "Bash".to_string(),
                input: json!({"command": "cargo test"}),
                summary: "cargo test".to_string(),
                output: "ok".to_string(),
                duration_ms: 1200,
                diff: None,
                artifact: Some(AssetId::new("asset_2")),
            },
            json!({
                "type": "toolCall",
                "toolCallId": "toolu_1",
                "name": "Bash",
                "input": {"command": "cargo test"},
                "summary": "cargo test",
                "output": "ok",
                "durationMs": 1200,
                "artifact": "asset_2"
            }),
        ),
        (
            ItemBody::Command {
                command: "cargo fmt".to_string(),
                dialect: ShellDialect::Posix,
                output: String::new(),
                tail: Some(CommandTail {
                    lines: vec!["formatting".to_string()],
                    total_lines: 1,
                }),
                exit_code: None,
                duration_ms: 40,
                background: true,
                command_id: Some(CommandId::new("cmd_1")),
                artifact: None,
            },
            json!({
                "type": "command",
                "command": "cargo fmt",
                "dialect": "posix",
                "output": "",
                "tail": {"lines": ["formatting"], "totalLines": 1},
                "durationMs": 40,
                "background": true,
                "commandId": "cmd_1"
            }),
        ),
        (
            ItemBody::Compaction {
                before_tokens: 120_000,
                after_tokens: 40_000,
                replaced_messages: 62,
                duration_ms: 5_400,
            },
            json!({
                "type": "compaction",
                "beforeTokens": 120_000,
                "afterTokens": 40_000,
                "replacedMessages": 62,
                "durationMs": 5_400
            }),
        ),
        (
            ItemBody::Rewind {
                mode: RewindMode::Applied,
                removed_items: 4,
                target_item_id: Some(ItemId::new("item_8")),
            },
            json!({
                "type": "rewind",
                "mode": "applied",
                "removedItems": 4,
                "targetItemId": "item_8"
            }),
        ),
        (
            ItemBody::Interruption {
                marker: "[Request interrupted by user]".to_string(),
            },
            json!({"type": "interruption", "marker": "[Request interrupted by user]"}),
        ),
        (
            ItemBody::Notice {
                code: "CONFIG_INVALID".to_string(),
                level: NoticeLevel::Warning,
                text: "thinkingLevel is invalid; fell back to off".to_string(),
            },
            json!({
                "type": "notice",
                "code": "CONFIG_INVALID",
                "level": "warning",
                "text": "thinkingLevel is invalid; fell back to off"
            }),
        ),
        (
            ItemBody::QuestionAnswer {
                interaction_id: InteractionId::new("int_4"),
                question: "Which suite?".to_string(),
                answer: "the unit tests".to_string(),
                option_id: Some("unit".to_string()),
            },
            json!({
                "type": "questionAnswer",
                "interactionId": "int_4",
                "question": "Which suite?",
                "answer": "the unit tests",
                "optionId": "unit"
            }),
        ),
        (
            ItemBody::PermissionReceipt {
                interaction_id: InteractionId::new("int_3"),
                tool: "Bash".to_string(),
                decision: PermissionDecisionKind::AllowSession,
                scope_id: Some(ScopeId::new("scope_8")),
                feedback: None,
            },
            json!({
                "type": "permissionReceipt",
                "interactionId": "int_3",
                "tool": "Bash",
                "decision": "allowSession",
                "scopeId": "scope_8"
            }),
        ),
        (
            ItemBody::Asset {
                asset_id: AssetId::new("asset_1"),
                label: Some("screenshot".to_string()),
            },
            json!({"type": "asset", "assetId": "asset_1", "label": "screenshot"}),
        ),
    ];
    assert_eq!(bodies.len(), ItemBody::KINDS.len());
    for (body, expected) in bodies {
        let item = Item {
            id: ItemId::new("item_1"),
            status: ItemStatus::Completed,
            turn_id: None,
            started_at: None,
            completed_at: None,
            body: body.clone(),
        };
        let mut merged = json!({"id": "item_1", "status": "completed"});
        if let (Some(target), Some(source)) = (merged.as_object_mut(), expected.as_object()) {
            for (key, value) in source {
                target.insert(key.clone(), value.clone());
            }
        }
        let value = to_value(&item);
        assert_eq!(value, merged, "{expected}");
        // The body is flattened beside the envelope, so a body field named like
        // an envelope field would silently shadow it.
        assert_eq!(value["id"], json!("item_1"), "{expected}");
        assert_eq!(value["status"], json!("completed"), "{expected}");
        let decoded: Item = from_value(merged);
        assert_eq!(decoded, item);
    }
}

#[test]
fn every_submit_disposition_round_trips() {
    let dispositions: Vec<(SubmitDisposition, Value)> = vec![
        (
            SubmitDisposition::TurnStarted {
                turn_id: TurnId::new("turn_9"),
            },
            json!({"type": "turnStarted", "turnId": "turn_9"}),
        ),
        (
            SubmitDisposition::Queued {
                queue_id: QueueId::new("queue_1"),
                position: 1,
                steer_eligible: true,
            },
            json!({
                "type": "queued",
                "queueId": "queue_1",
                "position": 1,
                "steerEligible": true
            }),
        ),
        (
            SubmitDisposition::Delivered {
                message_id: ItemId::new("item_20"),
            },
            json!({"type": "delivered", "messageId": "item_20"}),
        ),
        (
            SubmitDisposition::Applied {
                result: ActionResult {
                    status: ActionResultStatus::NoChange,
                    revision: None,
                    message: Some("already on sonnet".to_string()),
                },
            },
            json!({
                "type": "applied",
                "result": {"status": "noChange", "message": "already on sonnet"}
            }),
        ),
        (
            SubmitDisposition::OperationStarted {
                operation_id: OperationId::new("op_1"),
            },
            json!({"type": "operationStarted", "operationId": "op_1"}),
        ),
    ];
    assert_eq!(dispositions.len(), 5);
    for (disposition, expected) in dispositions {
        assert_eq!(to_value(&disposition), expected);
        let decoded: SubmitDisposition = from_value(expected);
        assert_eq!(decoded, disposition);
    }
}

#[test]
fn every_interaction_prompt_and_decision_round_trips() {
    let prompts: Vec<(InteractionPrompt, Value)> = vec![
        (
            InteractionPrompt::Question {
                title: "Which suite?".to_string(),
                question: "Which test suite should run?".to_string(),
                options: vec![QuestionOption {
                    id: "unit".to_string(),
                    label: "Unit tests".to_string(),
                    description: None,
                }],
                allows_free_text: true,
            },
            json!({
                "type": "question",
                "title": "Which suite?",
                "question": "Which test suite should run?",
                "options": [{"id": "unit", "label": "Unit tests"}],
                "allowsFreeText": true
            }),
        ),
        (
            InteractionPrompt::Confirmation {
                title: "Delete this session?".to_string(),
                detail: "The transcript will be removed.".to_string(),
                confirm_label: "Delete".to_string(),
            },
            json!({
                "type": "confirmation",
                "title": "Delete this session?",
                "detail": "The transcript will be removed.",
                "confirmLabel": "Delete"
            }),
        ),
    ];
    for (prompt, expected) in prompts {
        assert_eq!(to_value(&prompt), expected);
        let decoded: InteractionPrompt = from_value(expected);
        assert_eq!(decoded, prompt);
    }

    let decisions: Vec<(InteractionDecision, Value)> = vec![
        (InteractionDecision::AllowOnce, json!({"type": "allowOnce"})),
        (
            InteractionDecision::AllowSession {
                scope_id: ScopeId::new("scope_8"),
            },
            json!({"type": "allowSession", "scopeId": "scope_8"}),
        ),
        (
            InteractionDecision::Deny { feedback: None },
            json!({"type": "deny"}),
        ),
        (
            InteractionDecision::Answer {
                option_id: Some("unit".to_string()),
                text: None,
            },
            json!({"type": "answer", "optionId": "unit"}),
        ),
        (InteractionDecision::Confirm, json!({"type": "confirm"})),
        (InteractionDecision::Cancel, json!({"type": "cancel"})),
    ];
    assert_eq!(decisions.len(), 6);
    for (decision, expected) in decisions {
        assert_eq!(to_value(&decision), expected);
        let decoded: InteractionDecision = from_value(expected);
        assert_eq!(decoded, decision);
    }
}

#[test]
fn every_action_round_trips() {
    for action in crate::app::command::tests::every_action() {
        let value = to_value(&action);
        assert!(
            value.get("type").and_then(Value::as_str).is_some(),
            "{action:?} must be tagged"
        );
        let decoded: Action = from_value(value);
        assert_eq!(decoded, action);
    }
    assert_eq!(
        to_value(&Action::ConversationRewind {
            target: RewindTarget::Item {
                item_id: ItemId::new("item_8")
            },
            mode: RewindMode::Preview,
        }),
        json!({
            "type": "conversationRewind",
            "target": {"type": "item", "itemId": "item_8"},
            "mode": "preview"
        })
    );
    // The two the registry added (D146): `/team new` and `/team memory gc` were
    // mutations the CLI had and the union did not.
    assert_eq!(
        to_value(&Action::TeamScaffold {
            name: "crew".to_string()
        }),
        json!({"type": "teamScaffold", "name": "crew"})
    );
    assert_eq!(
        to_value(&Action::TeamMemoryGarbageCollect),
        json!({"type": "teamMemoryGarbageCollect"})
    );
}

#[test]
fn every_catalog_and_resource_page_names_its_kind() {
    let catalogs: Vec<(Catalog, &str)> = vec![
        (
            Catalog::Models(Page {
                items: Vec::new(),
                revision: 1,
                next_cursor: None,
            }),
            "models",
        ),
        (
            Catalog::Providers(Page {
                items: vec![ProviderInfo {
                    name: "default".to_string(),
                    protocol: "anthropic".to_string(),
                    api_base_url: "https://api.anthropic.com".to_string(),
                    builtin: true,
                    supports_images: true,
                    credential: CredentialState {
                        configured: true,
                        source: CredentialSource::Environment,
                        status: CredentialStatus::Present,
                    },
                }],
                revision: 1,
                next_cursor: None,
            }),
            "providers",
        ),
        (
            Catalog::Skills(Page {
                items: Vec::new(),
                revision: 1,
                next_cursor: None,
            }),
            "skills",
        ),
        (
            Catalog::McpServers(Page {
                items: Vec::new(),
                revision: 1,
                next_cursor: None,
            }),
            "mcpServers",
        ),
        (
            Catalog::Images(Page {
                items: Vec::new(),
                revision: 1,
                next_cursor: None,
            }),
            "images",
        ),
    ];
    for (catalog, kind) in catalogs {
        let value = to_value(&catalog);
        assert_eq!(value["catalog"].as_str(), Some(kind));
        let decoded: Catalog = from_value(value);
        assert_eq!(decoded, catalog);
    }
    // A provider entry carries presence, source, and status — never a key.
    let providers = to_value(&Catalog::Providers(Page {
        items: vec![ProviderInfo {
            name: "default".to_string(),
            protocol: "anthropic".to_string(),
            api_base_url: "https://api.anthropic.com".to_string(),
            builtin: true,
            supports_images: true,
            credential: CredentialState {
                configured: true,
                source: CredentialSource::Environment,
                status: CredentialStatus::Present,
            },
        }],
        revision: 1,
        next_cursor: None,
    }));
    assert_eq!(
        providers["items"][0]["credential"],
        json!({"configured": true, "source": "environment", "status": "present"})
    );

    let resources: Vec<(ResourcePage, &str)> = vec![
        (
            ResourcePage::Agents(Page {
                items: Vec::new(),
                revision: 1,
                next_cursor: None,
            }),
            "agents",
        ),
        (
            ResourcePage::Rooms(Page {
                items: Vec::new(),
                revision: 1,
                next_cursor: None,
            }),
            "rooms",
        ),
        (
            ResourcePage::Tasks(Page {
                items: Vec::new(),
                revision: 1,
                next_cursor: None,
            }),
            "tasks",
        ),
        (
            ResourcePage::Deliveries(Page {
                items: Vec::new(),
                revision: 1,
                next_cursor: None,
            }),
            "deliveries",
        ),
        (
            ResourcePage::BackgroundCommands(Page {
                items: vec![background_command()],
                revision: 1,
                next_cursor: None,
            }),
            "backgroundCommands",
        ),
    ];
    for (resource, kind) in resources {
        let value = to_value(&resource);
        assert_eq!(value["resource"].as_str(), Some(kind));
        let decoded: ResourcePage = from_value(value);
        assert_eq!(decoded, resource);
    }
}

#[test]
fn a_core_event_becomes_its_notification_without_a_translation_table() {
    let event = AppEvent {
        meta: event_meta(),
        payload: AppEventPayload::TurnStarted(TurnChanged {
            conversation_id: ConversationId::new("conv_main"),
            turn: turn(),
        }),
    };
    let method = event.method();
    let notification = ServerNotification::from(event);
    assert_eq!(notification.method(), method);
    assert_eq!(
        to_value(&NotificationFrame::new(notification)),
        json!({
            "jsonrpc": "2.0",
            "method": "turn/started",
            "params": {
                "event": event_meta_json(),
                "conversationId": "conv_main",
                "turn": turn_json()
            }
        })
    );
}
