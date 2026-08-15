//! Channel tools (experimental feature `experimental.agentChannels`).
//!
//! The domain calls them channels; everything anybody reads calls them **rooms**
//! (D95). A room's roster is an arbitrary subset of the team, so this layer —
//! the one place that knows *who is calling* — owns the seating policy: the
//! creator is stamped into the roster it creates, and every other member has to
//! be named. Nobody, including the user, is seated behind the caller's back.
//!
//! Speaking in a room is `SendMessage(to: "#room")` (D98): the tool wrapper that
//! used to own it retired, but [`deliver_post`] — the machinery it wrapped — is
//! unchanged and is still the one path a post takes, whoever sends it. The
//! sender is stamped from the session instance name (the model cannot forge it);
//! on a serial room, lagging posts are bounced back with the increments attached
//! (as a tool result, not an error — the model reads the increments in the same
//! turn and decides whether to resend, amend, or drop).
//!
//! `Channel`: room management (create/invite/kick/list), available to the
//! main agent and to direct sub-agents alike, because a team that can only be
//! grouped from the top is not a team that can organize itself.
//! Delivery wake-up: messages land in every member's inbox; idle members are woken
//! immediately, busy members get batched injection at turn boundaries; the main
//! agent's copy lands in hub_mail and is digested on a debounce (D98).
//! Silence = not sending.

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

/// Post + delivery wake-up + display row refresh — `SendMessage(to: "#room")`
/// and the TUI channel room share the same path (the user's posts as `user` in
/// the room go through here too).
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
#[serde(rename_all = "lowercase")]
pub enum ChannelAction {
    /// Create a room (you are seated in it; everyone else must be named in members).
    Create,
    /// Invite a member into the room (starts listening from the current head, no backlog).
    Invite,
    /// Remove a member from the room.
    Kick,
    /// List all rooms and their members.
    List,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ChannelInput {
    #[schemars(
        description = "Action: create a room / invite a member / remove a member / list rooms"
    )]
    action: ChannelAction,
    #[serde(default)]
    #[schemars(description = "Room name (required for create/invite/kick; without #)")]
    channel: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Member names: the roster to seat alongside you for create; the target (single) for invite/kick. Use \"user\" to seat the human and \"main\" to seat the main agent — neither is added for you."
    )]
    members: Option<Vec<String>>,
    #[serde(default)]
    #[schemars(
        description = "Posting mode (for create): serial (must have seen the latest before speaking; laggards bounce back) / free (interleaving allowed); default serial"
    )]
    mode: Option<String>,
}

/// Room management: create, invite/remove members, list.
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
            .ok_or_else(|| ToolError::failed("channel parameter (room name) is required"))
    }

    /// Cohort validation: members must be existing direct sub-agents (depth==1),
    /// the main agent, or the user — the last because a room that cannot invite
    /// the human is a room the human can only ever gate-crash.
    fn validate_member(&self, member: &str) -> Result<(), ToolError> {
        if member == HUB_NAME || member == crate::channels::USER_NAME {
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
        let who = sender_of(&self.session);
        format!(
            "Manage rooms. A room is the only group conversation there is: its members are any subset of the team, and it does not have to include the human. \
Creating one seats you ({who}) and nobody else — every other member has to be named in `members`, including `user` (the human) and `main` (the main agent). \
That is the point: you can form a room with the two agents you need to work something out, without putting it in front of anyone else. \
Actions: create (mode defaults to serial), invite (the new member starts listening from the current head and gets no backlog), kick, list (rooms with their rosters). \
Joins and departures are written into the room where everyone in it can see them, so there is no quiet way in or out. Messages reach every member's context in one order; members speak with SendMessage(to: \"#room\")."
        )
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
                // The runtime seats the caller, because the caller's identity is
                // the one fact the model cannot state for itself; everyone else
                // is exactly who was asked for. `create` de-duplicates, so
                // naming yourself as well is harmless rather than an error.
                let mut roster = vec![sender_of(&self.session)];
                roster.extend(members.iter().cloned());
                self.session
                    .channels
                    .create(&name, roster.clone(), mode)
                    .map_err(ToolError::failed)?;
                let id = ctx.watch.register_with_conditions(
                    Box::new(ChannelWatch {
                        label: format!("#{name}"),
                    }),
                    Vec::new(),
                    ctx.instance.clone(),
                );
                self.session.channels.set_watch(&name, id);
                let seated = self
                    .session
                    .channels
                    .info(&name)
                    .map(|status| status.members.join(", "))
                    .unwrap_or_else(|| roster.join(", "));
                format!("room #{name} created ({}, members: {seated})", mode.label())
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
                    "no rooms right now".to_string()
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
            // Rooms are behind the experimental gate, and `SendMessage`'s room
            // addressing reads the same flag the retired `Post` was assembled by.
            settings: {
                let mut settings = crate::settings::Settings::default();
                settings.experimental.agent_channels = true;
                settings
            },
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
            rewind: Default::default(),
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
        assert!(out.content.as_str().unwrap().contains("main, a"));
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
        // Speaking from a's (Running) perspective: stamped a; b is Running → accumulates in its inbox.
        let post_a = crate::tool::agent::SendMessageTool::new(sub_session(&hub, "a"));
        let out = post_a
            .call(
                serde_json::json!({"to": "#t", "message": "hello everyone"}),
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
        // Main posts: stamped main; its own inbox only receives others' posts.
        let post_hub = crate::tool::agent::SendMessageTool::new(hub.clone());
        let _ = post_hub
            .call(
                serde_json::json!({"to": "#t", "message": "quiet"}),
                &ctx(&hub),
            )
            .await
            .unwrap();
        let mail = hub.channels.drain_hub_mail();
        assert_eq!(mail.len(), 1, "{mail:?}");
        assert!(mail[0].contains("a: hello everyone"));
        // Non-member posts error out — refused by the addressing rules before
        // the room's own membership check is ever reached.
        let post_c = crate::tool::agent::SendMessageTool::new(sub_session(&hub, "c"));
        let err = post_c
            .call(serde_json::json!({"to": "#t", "message": "x"}), &ctx(&hub))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not a member of #t"), "{err}");
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
        let post_a = crate::tool::agent::SendMessageTool::new(sub_session(&hub, "a"));
        let post_b = crate::tool::agent::SendMessageTool::new(sub_session(&hub, "b"));
        let _ = post_a
            .call(
                serde_json::json!({"to": "#count", "message": "1"}),
                &ctx(&hub),
            )
            .await
            .unwrap();
        // b lags → bounced back (tool result, not an error) with increments attached.
        let out = post_b
            .call(
                serde_json::json!({"to": "#count", "message": "1"}),
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
                serde_json::json!({"to": "#count", "message": "2"}),
                &ctx(&hub),
            )
            .await
            .unwrap();
        assert!(out.content.as_str().unwrap().contains("msg #2"));
    }
}
