//! The one vocabulary that crosses kernel → client. A `Frame` is one line of a
//! session's journal; `Event` is everything that can happen in a session; an
//! `Item` is one unit in transcript order. Deltas, notices and lag markers are
//! ephemeral (never journaled); everything else is durable.

use std::collections::BTreeMap;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::KernelError;
use crate::ids::{IntentId, InteractionId, ItemId, Seq, SessionId, TurnId};
use crate::model::{ContentPart, ProviderMetadata, Usage};
use crate::view::View;

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_empty_meta(m: &ProviderMetadata) -> bool {
    m.is_empty()
}

fn is_empty_map(m: &serde_json::Map<String, Value>) -> bool {
    m.is_empty()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    pub seq: Seq,
    #[schemars(with = "String")]
    pub ts: Timestamp,
    pub session: SessionId,
    /// The client intent this frame answers or results from, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<IntentId>,
    pub event: Event,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Event {
    SessionUpdated {
        summary: SessionSummary,
    },
    SessionClosed {
        reason: CloseReason,
    },

    TurnStarted {
        turn: TurnId,
        inputs: Vec<ItemId>,
        origin: TurnOrigin,
    },
    TurnRetrying {
        turn: TurnId,
        attempt: u32,
        max: u32,
        delay_ms: u64,
        dropped: Vec<ItemId>,
        reason: String,
    },
    TurnUsage {
        turn: TurnId,
        usage: Usage,
        context: ContextUsage,
    },
    TurnCompleted {
        turn: TurnId,
        status: TurnStatus,
        usage: Usage,
    },

    ItemStarted {
        item: Item,
    },
    /// Ephemeral. `ItemCompleted` is authoritative over every delta before it.
    ItemDelta {
        item: ItemId,
        n: u32,
        kind: DeltaKind,
        data: String,
    },
    ItemUpdated {
        item: Item,
    },
    ItemCompleted {
        item: Item,
    },

    QueueChanged {
        revision: u64,
        entries: Vec<QueueEntry>,
    },

    InteractionOpened {
        interaction: Interaction,
    },
    InteractionResolved {
        id: InteractionId,
        answer: Answer,
        by: ResolvedBy,
    },
    InteractionCancelled {
        id: InteractionId,
        reason: CancelReason,
    },

    IntentAck {
        intent: IntentId,
        outcome: IntentOutcome,
    },

    Compacted {
        generation: u64,
        boundary: ItemId,
        summary: ItemId,
        kept: Vec<ItemId>,
    },
    Rewound {
        generation: u64,
        to_turn: TurnId,
        dropped: Vec<ItemId>,
        files_restored: Vec<String>,
    },

    ConfigChanged {
        config: ConfigView,
    },
    CatalogChanged {
        kind: String,
    },

    /// Ephemeral. Transcript-worthy notices are `ItemBody::Notice`.
    Notice {
        level: Level,
        code: String,
        text: String,
    },
    /// A plugin-owned resource changed (roster, room, task…). The kernel does not enumerate these.
    Extension {
        plugin: String,
        kind: String,
        payload: Value,
    },
    /// Ephemeral. A plugin's live state (ADR-0013 §2): the latest payload per
    /// `(plugin, kind)` is the whole of it; `Null` removes it; none survives a resume.
    Signal {
        plugin: String,
        kind: String,
        payload: Value,
    },

    /// Ephemeral, transport only: this subscriber missed `from..=to`; re-read the journal.
    Lagged {
        from: Seq,
        to: Seq,
    },
}

impl Event {
    /// Durable events are journaled; the rest exist only on the live stream.
    pub fn is_durable(&self) -> bool {
        !matches!(
            self,
            Event::ItemDelta { .. }
                | Event::Notice { .. }
                | Event::Signal { .. }
                | Event::Lagged { .. }
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DeltaKind {
    /// Append to assistant text.
    Text,
    /// Append to reasoning text.
    Reasoning,
    /// Replace the progress tail of a running tool call.
    Tail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CloseReason {
    Client,
    Shutdown,
    Deleted,
    Error { message: String },
}

/// Who submitted the input, as every surface records it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Origin {
    pub surface: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
}

impl Origin {
    pub fn surface(name: impl Into<String>) -> Self {
        Self {
            surface: name.into(),
            principal: None,
            conversation: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TurnOrigin {
    /// A client submitted while the session was idle.
    Submit,
    /// Drained from the queue when the previous turn ended.
    Queue,
    /// Another session posted into this one.
    Peer,
    /// The kernel or a plugin opened the turn with no prose.
    Auto,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TurnStatus {
    Completed,
    Failed { error: KernelError },
    Interrupted { reason: InterruptReason },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum InterruptReason {
    UserCancel,
    NewInput,
    Shutdown,
    Budget,
}

/// The one ruler for context: used tokens, the model window, and the point at
/// which compaction triggers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    /// Tokens the next request would carry, anchored on the server's last count.
    pub used: u64,
    /// The input side of the model's window: what is left once the output
    /// budget is reserved (ADR-0006).
    pub window: u64,
    /// `used` at which the older turns are summarised.
    pub trigger: u64,
}

impl ContextUsage {
    pub fn percent(&self) -> u64 {
        (self.used * 100).checked_div(self.window).unwrap_or(0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: ItemId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnId>,
    #[serde(default)]
    pub round: u32,
    pub status: ItemStatus,
    #[schemars(with = "String")]
    pub started_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub completed_at: Option<Timestamp>,
    /// The client intent that produced this item, when one did (user items).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<IntentId>,
    pub body: ItemBody,
    #[serde(default, skip_serializing_if = "is_empty_map")]
    pub meta: serde_json::Map<String, Value>,
}

impl Item {
    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ItemStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Interrupted,
}

impl ItemStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            ItemStatus::Completed | ItemStatus::Failed | ItemStatus::Interrupted
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ItemBody {
    User {
        parts: Vec<ContentPart>,
        origin: Origin,
    },
    Assistant {
        text: String,
    },
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "is_empty_meta")]
        provider_metadata: ProviderMetadata,
    },
    ToolCall {
        call_id: String,
        name: String,
        input: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<ToolOutput>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    /// A long-running non-turn operation (login, reconnect, team start).
    Action {
        name: String,
        #[serde(default)]
        args: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
    },
    Compaction {
        summary: String,
        replaced: u32,
        before: u64,
        after: u64,
        duration_ms: u64,
    },
    Rewind {
        to_turn: TurnId,
        dropped: u32,
    },
    Interruption {
        marker: String,
    },
    Notice {
        level: Level,
        code: String,
        text: String,
    },
    QuestionAnswer {
        interaction: InteractionId,
        question: String,
        answer: String,
    },
    PermissionReceipt {
        interaction: InteractionId,
        tool: String,
        decision: DecisionKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feedback: Option<String>,
    },
    Asset {
        asset: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DecisionKind {
    Allow,
    AllowSession,
    Deny,
}

/// What a tool returned: the parts the model sees, plus what a person sees
/// instead when the tool has something better than the text (ADR-0013 §2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutput {
    pub parts: Vec<ContentPart>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<View>,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            parts: vec![ContentPart::text(text)],
            is_error: false,
            display: None,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            parts: vec![ContentPart::text(text)],
            is_error: true,
            display: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    pub intent: IntentId,
    pub position: u32,
    pub preview: String,
    pub steerable: bool,
    pub origin: Origin,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Interaction {
    pub id: InteractionId,
    pub session: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
    #[schemars(with = "String")]
    pub opened_at: Timestamp,
    /// Keyboard approvals before this instant are rejected with `NOT_READY`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub guard_until: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub expires_at: Option<Timestamp>,
    pub kind: InteractionKind,
    /// Exactly the answers the kernel will accept.
    pub answers: Vec<AnswerSpec>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum InteractionKind {
    Permission {
        tool: String,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preview: Option<Preview>,
        /// The rule `AllowSession` would install, when one would silence the prompt.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_scope: Option<String>,
    },
    Question {
        question: String,
        /// A short tag a surface may show before the question (`Auth method`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header: Option<String>,
        options: Vec<QuestionOption>,
        #[serde(default)]
        free_text: bool,
        #[serde(default)]
        multi: bool,
    },
    Confirm {
        title: String,
        detail: String,
    },
    Login {
        provider: String,
        flow: LoginFlow,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOption {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Preview {
    Diff { unified: String },
    Command { command: String, cwd: String },
    Url { url: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LoginFlow {
    Browser { url: String },
    Device { url: String, code: String },
    Paste,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AnswerSpec {
    AllowOnce,
    AllowSession,
    Deny,
    Choice,
    Text,
    Confirm,
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Answer {
    AllowOnce,
    AllowSession {
        scope: String,
    },
    Deny {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feedback: Option<String>,
    },
    Choice {
        ids: Vec<String>,
    },
    Text {
        text: String,
    },
    Confirm,
    Cancel,
}

impl Answer {
    pub fn spec(&self) -> AnswerSpec {
        match self {
            Answer::AllowOnce => AnswerSpec::AllowOnce,
            Answer::AllowSession { .. } => AnswerSpec::AllowSession,
            Answer::Deny { .. } => AnswerSpec::Deny,
            Answer::Choice { .. } => AnswerSpec::Choice,
            Answer::Text { .. } => AnswerSpec::Text,
            Answer::Confirm => AnswerSpec::Confirm,
            Answer::Cancel => AnswerSpec::Cancel,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Activation {
    Keyboard,
    Pointer,
    Programmatic,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ResolvedBy {
    Client { name: String, surface: String },
    Kernel,
    Policy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CancelReason {
    TurnEnded,
    /// The holding command that asked has finished (ADR-0012 §5).
    CommandEnded,
    Interrupted,
    SessionClosed,
    Expired,
    Superseded,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum IntentOutcome {
    TurnStarted { turn: TurnId },
    Queued { position: u32 },
    Applied { result: Value },
    Rejected { error: KernelError },
}

/// Where a child hangs in the tree: its parent, and the tool call that
/// spawned it when one did (ADR-0011 §3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ParentLink {
    pub session: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
}

/// What a session does with what it is told (ADR-0011 §1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Driver {
    /// A model answers: every prose input opens a turn.
    #[default]
    Model,
    /// Nothing answers: every input is recorded, and the journal is the point.
    Log,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ParentLink>,
    #[serde(default)]
    pub driver: Driver,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// What the session was opened with and a resume must give it back:
    /// the prompt appended to the kernel's own, and the tool set it is held to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_extra: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[schemars(with = "String")]
    pub created_at: Timestamp,
    #[schemars(with = "String")]
    pub updated_at: Timestamp,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub busy: bool,
}

/// Configuration as a client sees it: the kernel's keys and each plugin's claimed slice.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigView {
    #[serde(default)]
    pub kernel: Value,
    #[serde(default)]
    pub plugins: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts() -> Timestamp {
        Timestamp::from_second(1_700_000_000).unwrap()
    }

    fn item(id: &str, body: ItemBody) -> Item {
        Item {
            id: ItemId::from_raw(id),
            turn: Some(TurnId::from_raw("trn_1")),
            round: 0,
            status: ItemStatus::Completed,
            started_at: ts(),
            completed_at: Some(ts()),
            intent: None,
            body,
            meta: Default::default(),
        }
    }

    fn frame(seq: u64, event: Event) -> Frame {
        Frame {
            seq: Seq(seq),
            ts: ts(),
            session: SessionId::from_raw("ses_1"),
            cause: None,
            event,
        }
    }

    /// One representative frame per event variant, pinned as JSON. Changing a
    /// snapshot is a wire change and deserves an ADR line.
    #[test]
    fn every_event_variant_has_a_pinned_wire_form() {
        let interaction = Interaction {
            id: InteractionId::from_raw("int_1"),
            session: SessionId::from_raw("ses_1"),
            turn: Some(TurnId::from_raw("trn_1")),
            item: Some(ItemId::from_raw("itm_3")),
            opened_at: ts(),
            guard_until: None,
            expires_at: None,
            kind: InteractionKind::Permission {
                tool: "Edit".into(),
                summary: "Edit src/lib.rs".into(),
                preview: Some(Preview::Diff {
                    unified: "--- a\n+++ b\n".into(),
                }),
                session_scope: Some("Edit(src/)".into()),
            },
            answers: vec![
                AnswerSpec::AllowOnce,
                AnswerSpec::AllowSession,
                AnswerSpec::Deny,
            ],
        };
        let summary = SessionSummary {
            id: SessionId::from_raw("ses_1"),
            key: None,
            title: Some("hello".into()),
            cwd: "/tmp/p".into(),
            parent: None,
            driver: Driver::Model,
            model: Some("fake-1".into()),
            system_extra: None,
            tools: None,
            provider: Some("fake".into()),
            created_at: ts(),
            updated_at: ts(),
            usage: Usage::default(),
            busy: false,
        };
        let frames = vec![
            frame(
                1,
                Event::SessionUpdated {
                    summary: summary.clone(),
                },
            ),
            frame(
                2,
                Event::TurnStarted {
                    turn: TurnId::from_raw("trn_1"),
                    inputs: vec![ItemId::from_raw("itm_1")],
                    origin: TurnOrigin::Submit,
                },
            ),
            frame(
                3,
                Event::ItemCompleted {
                    item: item(
                        "itm_1",
                        ItemBody::User {
                            parts: vec![ContentPart::text("run tests")],
                            origin: Origin::surface("tui"),
                        },
                    ),
                },
            ),
            frame(
                4,
                Event::IntentAck {
                    intent: IntentId::from_raw("req_1"),
                    outcome: IntentOutcome::TurnStarted {
                        turn: TurnId::from_raw("trn_1"),
                    },
                },
            ),
            frame(
                5,
                Event::ItemStarted {
                    item: Item {
                        status: ItemStatus::Running,
                        completed_at: None,
                        ..item(
                            "itm_2",
                            ItemBody::Assistant {
                                text: String::new(),
                            },
                        )
                    },
                },
            ),
            frame(
                6,
                Event::ItemDelta {
                    item: ItemId::from_raw("itm_2"),
                    n: 0,
                    kind: DeltaKind::Text,
                    data: "Sure".into(),
                },
            ),
            frame(
                7,
                Event::ItemCompleted {
                    item: item(
                        "itm_2",
                        ItemBody::Assistant {
                            text: "Sure".into(),
                        },
                    ),
                },
            ),
            frame(
                8,
                Event::ItemStarted {
                    item: Item {
                        status: ItemStatus::Pending,
                        completed_at: None,
                        ..item(
                            "itm_3",
                            ItemBody::ToolCall {
                                call_id: "call_1".into(),
                                name: "Edit".into(),
                                input: serde_json::json!({"file_path": "src/lib.rs"}),
                                output: None,
                                progress: None,
                                duration_ms: None,
                            },
                        )
                    },
                },
            ),
            frame(9, Event::InteractionOpened { interaction }),
            frame(
                10,
                Event::InteractionResolved {
                    id: InteractionId::from_raw("int_1"),
                    answer: Answer::AllowOnce,
                    by: ResolvedBy::Client {
                        name: "gui-2".into(),
                        surface: "gui".into(),
                    },
                },
            ),
            frame(
                11,
                Event::InteractionCancelled {
                    id: InteractionId::from_raw("int_2"),
                    reason: CancelReason::TurnEnded,
                },
            ),
            frame(
                12,
                Event::ItemUpdated {
                    item: Item {
                        status: ItemStatus::Running,
                        completed_at: None,
                        ..item(
                            "itm_3",
                            ItemBody::ToolCall {
                                call_id: "call_1".into(),
                                name: "Edit".into(),
                                input: serde_json::json!({}),
                                output: None,
                                progress: Some("writing…".into()),
                                duration_ms: None,
                            },
                        )
                    },
                },
            ),
            frame(
                13,
                Event::TurnUsage {
                    turn: TurnId::from_raw("trn_1"),
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 4,
                        ..Default::default()
                    },
                    context: ContextUsage {
                        used: 14,
                        window: 200_000,
                        trigger: 180_000,
                    },
                },
            ),
            frame(
                14,
                Event::TurnRetrying {
                    turn: TurnId::from_raw("trn_1"),
                    attempt: 1,
                    max: 10,
                    delay_ms: 500,
                    dropped: vec![ItemId::from_raw("itm_4")],
                    reason: "server error 503".into(),
                },
            ),
            frame(
                15,
                Event::QueueChanged {
                    revision: 1,
                    entries: vec![QueueEntry {
                        intent: IntentId::from_raw("req_2"),
                        position: 0,
                        preview: "also fix the docs".into(),
                        steerable: true,
                        origin: Origin::surface("tui"),
                    }],
                },
            ),
            frame(
                16,
                Event::Compacted {
                    generation: 1,
                    boundary: ItemId::from_raw("itm_2"),
                    summary: ItemId::from_raw("itm_9"),
                    kept: vec![ItemId::from_raw("itm_1")],
                },
            ),
            frame(
                17,
                Event::Rewound {
                    generation: 2,
                    to_turn: TurnId::from_raw("trn_1"),
                    dropped: vec![ItemId::from_raw("itm_3")],
                    files_restored: vec!["src/lib.rs".into()],
                },
            ),
            frame(
                18,
                Event::ConfigChanged {
                    config: ConfigView::default(),
                },
            ),
            frame(
                19,
                Event::CatalogChanged {
                    kind: "models".into(),
                },
            ),
            frame(
                20,
                Event::Notice {
                    level: Level::Warn,
                    code: "COUNT_TOKENS_UNAVAILABLE".into(),
                    text: "estimating".into(),
                },
            ),
            frame(
                21,
                Event::Extension {
                    plugin: "bingo.tasks".into(),
                    kind: "task.changed".into(),
                    payload: serde_json::json!({"id": 1}),
                },
            ),
            frame(
                22,
                Event::Lagged {
                    from: Seq(3),
                    to: Seq(9),
                },
            ),
            frame(
                25,
                Event::Signal {
                    plugin: "bingo.demo.ui".into(),
                    kind: "progress".into(),
                    payload: serde_json::json!({"kind": "progress", "value": 3, "total": 10}),
                },
            ),
            frame(
                23,
                Event::TurnCompleted {
                    turn: TurnId::from_raw("trn_1"),
                    status: TurnStatus::Completed,
                    usage: Usage::default(),
                },
            ),
            frame(
                24,
                Event::SessionClosed {
                    reason: CloseReason::Client,
                },
            ),
        ];
        insta::assert_json_snapshot!("frames", frames);
        for f in &frames {
            let json = serde_json::to_string(f).unwrap();
            assert_eq!(&serde_json::from_str::<Frame>(&json).unwrap(), f);
        }
    }

    #[test]
    fn only_deltas_notices_and_lag_are_ephemeral() {
        assert!(
            !Event::ItemDelta {
                item: ItemId::from_raw("i"),
                n: 0,
                kind: DeltaKind::Text,
                data: String::new()
            }
            .is_durable()
        );
        assert!(
            !Event::Notice {
                level: Level::Info,
                code: "X".into(),
                text: String::new()
            }
            .is_durable()
        );
        assert!(
            !Event::Lagged {
                from: Seq(1),
                to: Seq(2)
            }
            .is_durable()
        );
        assert!(
            Event::SessionClosed {
                reason: CloseReason::Client
            }
            .is_durable()
        );
    }
}
