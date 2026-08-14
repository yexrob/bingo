//! Channel tools (experimental feature `experimental.agentChannels`).
//!
//! `Post`: any member (hub + depth-1 sub-agents) posts to a channel — the sender is stamped
//! by the session instance name (the model cannot forge it); on a serial channel, lagging
//! posts are bounced back with the increments attached (as a tool result, not an error —
//! the model reads the increments in the same turn and decides whether to resend, amend,
//! or drop). `Channel`: hub-only room management (create/invite/kick/list).
//! Delivery wake-up: messages land in every member's inbox; idle members are woken
//! immediately, busy members get batched injection at turn boundaries; the hub is injected
//! via hub_mail before the next round of reasoning. Silence = not calling Post.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use crate::channels::{ChannelMode, HUB_NAME, PostOutcome};
use crate::query::Session;
use crate::tool::agent::flush_agent_inbox;
use crate::tool::{Tool, ToolContext, ToolError, ToolResult, parse_input};
use crate::watch::{WatchKind, WatchState};

/// Channel display row (◇ #name · N messages · latest…): no polling; updated proactively on post.
struct ChannelWatch {
    label: String,
}

impl crate::watch::Watchable for ChannelWatch {
    fn label(&self) -> String {
        self.label.clone()
    }
    fn poll(&self) -> crate::watch::WatchPoll {
        crate::watch::WatchPoll {
            state: WatchState::Running,
            detail: Some("0 messages".to_string()),
            payload: None,
            signal: None,
        }
    }
    fn check_interval(&self) -> Option<std::time::Duration> {
        None
    }
    fn kind(&self) -> WatchKind {
        WatchKind::Channel
    }
}

/// Refresh the channel display row after a post (detail = message count and latest post, payload = log tail).
fn refresh_channel_row(session: &Arc<Session>, name: &str) {
    if let Some((Some(id), detail, payload)) = session.channels.row_snapshot(name) {
        session.watch.set_state(
            id,
            WatchState::Running,
            Some(detail),
            Some(serde_json::json!(payload)),
        );
    }
}

/// This session's member name in a channel: sub-agents = instance name, main session = main.
fn sender_of(session: &Arc<Session>) -> String {
    session
        .instance
        .clone()
        .unwrap_or_else(|| HUB_NAME.to_string())
}

/// Post outcome (the two exit paths of deliver_post).
pub(crate) enum PostDelivery {
    Sent {
        seq: u64,
    },
    Stale {
        missed: Vec<crate::channels::ChannelMessage>,
    },
}

/// Post + delivery wake-up + display row refresh — the Post tool and the TUI channel room
/// share the same path (the user's posts as `user` in the room go through here too).
pub(crate) fn deliver_post(
    session: &Arc<Session>,
    watch: &Arc<crate::watch::WatchRegistry>,
    from: &str,
    channel: &str,
    text: &str,
) -> Result<PostDelivery, String> {
    match session.channels.post(from, channel, text)? {
        PostOutcome::Sent { seq, deliveries } => {
            refresh_channel_row(session, channel);
            // Deposit first, then claim idle members in one pass. Running members observe the
            // inbox signal and absorb everything waiting at their next tool round.
            for (member, msg) in deliveries {
                session.agents.deposit(
                    &member,
                    crate::agents::InboxItem::Channel {
                        channel: channel.to_string(),
                        from: msg.from.clone(),
                        text: msg.text.clone(),
                        seq: msg.seq,
                    },
                );
            }
            flush_agent_inbox(session, watch);
            Ok(PostDelivery::Sent { seq })
        }
        PostOutcome::Stale { missed } => Ok(PostDelivery::Stale { missed }),
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct PostInput {
    #[schemars(description = "Channel name (without #)")]
    channel: String,
    #[schemars(description = "Message content")]
    message: String,
}

/// Post to a channel (sender = this session's instance name, stamped by the runtime).
pub struct PostTool {
    session: Arc<Session>,
}

impl PostTool {
    pub fn new(session: Arc<Session>) -> Self {
        Self { session }
    }
}

#[async_trait]
impl Tool for PostTool {
    fn name(&self) -> String {
        "Post".to_string()
    }
    fn description(&self) -> String {
        let who = sender_of(&self.session);
        format!(
            "Speak in an agent channel. Your name in the channel is {who} (the sender is stamped by the runtime and cannot be forged). \
This tool is the only way to put words in the room: the text you write as your turn result goes to the hub, not to the channel, so deciding to answer means calling this. \
Channel messages enter every member's context (in the same order); in a serial channel, if you are behind the latest message the send bounces back with the new content attached — read it, then decide to resend, amend, or drop. \
Every message wakes every other member, so who you are answering matters: when `user` or `main` addresses the room, answer once, briefly, the way a person answers the room they are in; when another member speaks you owe nothing unless they named you or you can unblock them; never answer an answer — that is what floods a room. When you have nothing to add, simply don't call this tool (silence costs nothing and wakes nobody). \
The audience picks the lane: something every member should know belongs here; what concerns one agent alone goes to them directly — the hub reaches a member with SendMessage, a member reaches the hub in its turn text — not into everyone's context."
        )
    }
    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<PostInput>()
    }
    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }
    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        false
    }
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: PostInput = parse_input(&input)?;
        let from = sender_of(&self.session);
        let channel = params.channel.trim_start_matches('#').to_string();
        match deliver_post(&self.session, &ctx.watch, &from, &channel, &params.message)
            .map_err(ToolError::failed)?
        {
            PostDelivery::Sent { seq } => Ok(ToolResult {
                content: serde_json::Value::String(format!("sent (#{channel} msg #{seq})")),
                is_error: false,
                diff: None,
            }),
            PostDelivery::Stale { missed } => {
                let lines: Vec<String> = missed
                    .iter()
                    .map(|m| format!("[#{channel} msg #{}] {}: {}", m.seq, m.from, m.text))
                    .collect();
                Ok(ToolResult {
                    content: serde_json::Value::String(format!(
                        "not sent — the channel got new messages while you were drafting:\n{}\n\
Decide again from the latest content: resend as-is (call again unchanged), edit and resend, or drop the message.",
                        lines.join("\n")
                    )),
                    is_error: false,
                    diff: None,
                })
            }
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ChannelAction {
    /// Create a channel (hub joins automatically).
    Create,
    /// Invite a member into the channel (starts listening from the current head, no backlog).
    Invite,
    /// Kick a member out.
    Kick,
    /// List all channels.
    List,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ChannelInput {
    #[schemars(
        description = "Action: create a channel / invite a member / kick a member / list channels"
    )]
    action: ChannelAction,
    #[serde(default)]
    #[schemars(description = "Channel name (required for create/invite/kick; without #)")]
    channel: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Member instance names: initial roster for create; target (single) for invite/kick"
    )]
    members: Option<Vec<String>>,
    #[serde(default)]
    #[schemars(
        description = "Posting mode (for create): serial (must have seen the latest before speaking; laggards bounce back) / free (interleaving allowed); default serial"
    )]
    mode: Option<String>,
}

/// Channel management (hub only): create, invite/kick members, list.
pub struct ChannelTool {
    session: Arc<Session>,
}

impl ChannelTool {
    pub fn new(session: Arc<Session>) -> Self {
        Self { session }
    }

    fn require_channel(input: &ChannelInput) -> Result<&str, ToolError> {
        input
            .channel
            .as_deref()
            .map(|c| c.trim_start_matches('#'))
            .filter(|c| !c.is_empty())
            .ok_or_else(|| ToolError::failed("channel parameter (channel name) is required"))
    }

    /// Cohort validation: members must be existing direct sub-agents (depth==1).
    fn validate_member(&self, member: &str) -> Result<(), ToolError> {
        if member == HUB_NAME {
            return Ok(());
        }
        match self.session.agents.depth_of(member) {
            Some(1) => Ok(()),
            Some(_) => Err(ToolError::failed(format!(
                "{member} is not a direct subagent (channel members are limited to instances spawned directly by the main session)"
            ))),
            None => Err(ToolError::failed(format!(
                "no subagent instance named {member} (spawn one with Agent first)"
            ))),
        }
    }
}

#[async_trait]
impl Tool for ChannelTool {
    fn name(&self) -> String {
        "Channel".to_string()
    }
    fn description(&self) -> String {
        "Manage agent channels: create one (members is the subagent instance roster; you join automatically as main; mode defaults to serial), invite/kick members, list channels. Channel messages enter all members' contexts (in the same order); members speak via Post.".to_string()
    }
    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<ChannelInput>()
    }
    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }
    fn is_read_only(&self, input: &serde_json::Value) -> bool {
        input.get("action").and_then(|a| a.as_str()) == Some("list")
    }
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: ChannelInput = parse_input(&input)?;
        let text = match params.action {
            ChannelAction::Create => {
                let name = Self::require_channel(&params)?.to_string();
                let members = params.members.clone().unwrap_or_default();
                for m in &members {
                    self.validate_member(m)?;
                }
                let mode = match &params.mode {
                    Some(m) => ChannelMode::parse(m).map_err(ToolError::failed)?,
                    None => ChannelMode::Serial,
                };
                self.session
                    .channels
                    .create(&name, members.clone(), mode)
                    .map_err(ToolError::failed)?;
                let id = ctx.watch.register_with_conditions(
                    Box::new(ChannelWatch {
                        label: format!("#{name}"),
                    }),
                    Vec::new(),
                    ctx.instance.clone(),
                );
                self.session.channels.set_watch(&name, id);
                format!(
                    "channel #{name} created ({}, members: main{}{})",
                    mode.label(),
                    if members.is_empty() { "" } else { ", " },
                    members.join(", ")
                )
            }
            ChannelAction::Invite => {
                let name = Self::require_channel(&params)?.to_string();
                let member = params
                    .members
                    .as_deref()
                    .and_then(|m| m.first())
                    .ok_or_else(|| {
                        ToolError::failed("invite requires members (target instance names)")
                    })?
                    .clone();
                self.validate_member(&member)?;
                self.session
                    .channels
                    .invite(&name, &member)
                    .map_err(ToolError::failed)?;
                format!("{member} joined #{name} (listening from the current message head)")
            }
            ChannelAction::Kick => {
                let name = Self::require_channel(&params)?.to_string();
                let member = params
                    .members
                    .as_deref()
                    .and_then(|m| m.first())
                    .ok_or_else(|| {
                        ToolError::failed("kick requires members (target instance names)")
                    })?
                    .clone();
                self.session
                    .channels
                    .kick(&name, &member)
                    .map_err(ToolError::failed)?;
                format!("{member} removed from #{name}")
            }
            ChannelAction::List => {
                let statuses = self.session.channels.list();
                if statuses.is_empty() {
                    "no channels right now".to_string()
                } else {
                    statuses
                        .iter()
                        .map(|s| {
                            format!(
                                "- #{} ({}, {} messages{}): {}",
                                s.name,
                                s.mode.label(),
                                s.seq,
                                if s.frozen { ", frozen" } else { "" },
                                s.members.join(", ")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
        };
        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: false,
            diff: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentRegistry;

    fn hub_session() -> Arc<Session> {
        Arc::new(Session {
            client: crate::api::client::Client::new("k".into(), "http://x".into()),
            runtime: crate::query::Runtime::new("m".into(), None, Default::default()),
            permission_mode: crate::permission::PermissionMode::Default,
            settings: crate::settings::Settings::default(),
            system: Vec::new(),
            depth: 0,
            cwd: Arc::new(std::sync::Mutex::new(std::env::temp_dir())),
            home: std::env::temp_dir(),
            user_config_dir: std::env::temp_dir().join(".config"),
            quiet: true,
            compact_failures: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "t")),
            expand_tasks: tokio::sync::watch::channel(false).0,
            agents: AgentRegistry::new(),
            channels: crate::channels::ChannelRegistry::new(Default::default()),
            instance: None,
            attachments: crate::api::image::Attachments::new(),
        })
    }

    /// A depth-1 sub-session under the same registry/channel table (instance name stamped).
    fn sub_session(hub: &Arc<Session>, instance: &str) -> Arc<Session> {
        Arc::new(Session {
            depth: 1,
            instance: Some(instance.to_string()),
            ..(**hub).clone()
        })
    }

    fn ctx(session: &Arc<Session>) -> ToolContext {
        ToolContext {
            cwd: std::path::PathBuf::from("/tmp"),
            home: std::env::temp_dir(),
            watch: session.watch.clone(),
            live: Default::default(),
            http: reqwest::Client::new(),
            tasks: session.tasks.clone(),
            hooks: crate::settings::HooksConfig::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
            instance: None,
        }
    }

    #[tokio::test]
    async fn create_validates_cohort_and_registers_row() {
        let hub = hub_session();
        let tool = ChannelTool::new(hub.clone());
        // Unknown members are rejected.
        let err = tool
            .call(
                serde_json::json!({"action": "create", "channel": "t", "members": ["ghost"]}),
                &ctx(&hub),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("ghost"), "{err}");
        // depth-1 members pass; the hub joins automatically.
        hub.agents.insert(
            "a",
            crate::agents::AgentKind::Hire,
            None,
            "a".into(),
            sub_session(&hub, "a"),
        );
        let out = tool
            .call(
                serde_json::json!({"action": "create", "channel": "t", "members": ["a"]}),
                &ctx(&hub),
            )
            .await
            .unwrap();
        let text = out.content.as_str().unwrap();
        assert!(text.contains("#t") && text.contains("serial"), "{text}");
        // Deeper instances are refused entry to the channel.
        let deep = Arc::new(Session {
            depth: 2,
            ..(*sub_session(&hub, "deep")).clone()
        });
        hub.agents.insert(
            "deep",
            crate::agents::AgentKind::Hire,
            None,
            "d".into(),
            deep,
        );
        let err = tool
            .call(
                serde_json::json!({"action": "invite", "channel": "t", "members": ["deep"]}),
                &ctx(&hub),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("direct subagent"), "{err}");
        // list outputs members and mode.
        let out = tool
            .call(serde_json::json!({"action": "list"}), &ctx(&hub))
            .await
            .unwrap();
        assert!(out.content.as_str().unwrap().contains("main, user, a"));
        assert!(tool.is_read_only(&serde_json::json!({"action": "list"})));
    }

    #[tokio::test]
    async fn post_stamps_sender_and_queues_to_running_members() {
        let hub = hub_session();
        hub.agents.insert(
            "a",
            crate::agents::AgentKind::Hire,
            None,
            "a".into(),
            sub_session(&hub, "a"),
        );
        hub.agents.insert(
            "b",
            crate::agents::AgentKind::Hire,
            None,
            "b".into(),
            sub_session(&hub, "b"),
        );
        let mgmt = ChannelTool::new(hub.clone());
        let _ = mgmt
            .call(
                serde_json::json!({"action": "create", "channel": "t", "members": ["a", "b"], "mode": "free"}),
                &ctx(&hub),
            )
            .await
            .unwrap();
        // Post from a's (Running) perspective: stamped a; b is Running → accumulates in its inbox.
        let post_a = PostTool::new(sub_session(&hub, "a"));
        let out = post_a
            .call(
                serde_json::json!({"channel": "t", "message": "hello everyone"}),
                &ctx(&hub),
            )
            .await
            .unwrap();
        assert!(out.content.as_str().unwrap().contains("msg #1"));
        let items = hub
            .agents
            .finish("b", Vec::new(), 1)
            .unwrap_or_else(|| panic!("b's inbox should have a message"))
            .items;
        assert!(
            matches!(&items[..], [crate::agents::InboxItem::Channel { from, text, .. }]
                if from == "a" && text == "hello everyone"),
            "stamped as a"
        );
        // Hub posts: stamped main; hub_mail only receives others' posts.
        let post_hub = PostTool::new(hub.clone());
        let _ = post_hub
            .call(
                serde_json::json!({"channel": "t", "message": "quiet"}),
                &ctx(&hub),
            )
            .await
            .unwrap();
        let mail = hub.channels.drain_hub_mail();
        assert_eq!(mail.len(), 1, "{mail:?}");
        assert!(mail[0].contains("a: hello everyone"));
        // Non-member posts error out.
        let post_c = PostTool::new(sub_session(&hub, "c"));
        let err = post_c
            .call(
                serde_json::json!({"channel": "t", "message": "x"}),
                &ctx(&hub),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("is not"), "{err}");
    }

    #[tokio::test]
    async fn serial_bounce_returns_increments_as_result() {
        let hub = hub_session();
        hub.agents.insert(
            "a",
            crate::agents::AgentKind::Hire,
            None,
            "a".into(),
            sub_session(&hub, "a"),
        );
        hub.agents.insert(
            "b",
            crate::agents::AgentKind::Hire,
            None,
            "b".into(),
            sub_session(&hub, "b"),
        );
        let mgmt = ChannelTool::new(hub.clone());
        let _ = mgmt
            .call(
                serde_json::json!({"action": "create", "channel": "count", "members": ["a", "b"]}),
                &ctx(&hub),
            )
            .await
            .unwrap();
        let post_a = PostTool::new(sub_session(&hub, "a"));
        let post_b = PostTool::new(sub_session(&hub, "b"));
        let _ = post_a
            .call(
                serde_json::json!({"channel": "count", "message": "1"}),
                &ctx(&hub),
            )
            .await
            .unwrap();
        // b lags → bounced back (tool result, not an error) with increments attached.
        let out = post_b
            .call(
                serde_json::json!({"channel": "count", "message": "1"}),
                &ctx(&hub),
            )
            .await
            .unwrap();
        let text = out.content.as_str().unwrap();
        assert!(!out.is_error);
        assert!(text.contains("not sent") && text.contains("a: 1"), "{text}");
        // Resend lands (the model changed its message).
        let out = post_b
            .call(
                serde_json::json!({"channel": "count", "message": "2"}),
                &ctx(&hub),
            )
            .await
            .unwrap();
        assert!(out.content.as_str().unwrap().contains("msg #2"));
    }
}
