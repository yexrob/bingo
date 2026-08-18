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
//! Delivery wakes, and nothing gates it (v7): a message lands in every
//! member's inbox and whoever is idle starts a run on it — named or not —
//! while a running member absorbs it at its next tool boundary, which costs
//! input tokens and no model call. The `@` decides what is *owed*, never what
//! is read. The main agent's copy lands in main_mail and is digested on a
//! debounce (D98) — coalescing a burst into one turn, holding nothing back.
//! Silence = not sending.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use crate::channels::{ChannelMode, MAIN_NAME, PostOutcome};
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
    crate::tool::address::sender_of(session)
}

/// Post outcome (the two exit paths of deliver_post).
pub(crate) enum PostDelivery {
    Sent {
        seq: u64,
        /// `@tokens` that resolved to nobody in the room: told to the sender
        /// so a typo is caught in the sending turn.
        unknown_mentions: Vec<String>,
        /// Members a mention named whose copy could not be delivered (stopped):
        /// the needs-you-now promise silently not kept unless the sender hears it.
        undelivered_mentions: Vec<String>,
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
    watch: &crate::watch::WatchHandle,
    from: &str,
    channel: &str,
    text: &str,
) -> Result<PostDelivery, String> {
    match session.channels.post(from, channel, text)? {
        PostOutcome::Sent {
            seq,
            deliveries,
            unknown_mentions,
        } => {
            refresh_channel_row(session, channel);
            // Deposit first, then claim every idle member in one pass (v7):
            // the deposit pulses the inbox signal, so a running member absorbs
            // it at its next tool round and an idle one is woken here. Nothing
            // waits on a count or a clock any more.
            let mut undelivered_mentions = Vec::new();
            for delivery in deliveries {
                let accepted = session.agents.deposit(
                    &delivery.member,
                    crate::agents::InboxItem::Channel {
                        channel: channel.to_string(),
                        from: delivery.msg.from.clone(),
                        text: delivery.msg.text.clone(),
                        seq: delivery.msg.seq,
                    },
                );
                if !accepted && delivery.mentioned {
                    undelivered_mentions.push(delivery.member);
                } else if delivery.mentioned {
                    spawn_mention_watchdog(
                        session.clone(),
                        watch.clone(),
                        channel.to_string(),
                        seq,
                        delivery.member,
                        from.to_string(),
                        crate::tool::agent::excerpt(text),
                    );
                }
            }
            // `@all` is one debt against the room rather than one per member
            // (R4), so it gets one watchdog rather than N — the room is chased
            // by nudging nobody and telling the sender, because a nudge would
            // have to pick a member the sigil deliberately did not.
            if crate::channels::mention_tokens(text)
                .iter()
                .any(|t| t == crate::channels::ALL_NAME)
            {
                spawn_mention_watchdog(
                    session.clone(),
                    watch.clone(),
                    channel.to_string(),
                    seq,
                    crate::channels::ALL_NAME.to_string(),
                    from.to_string(),
                    crate::tool::agent::excerpt(text),
                );
            }
            flush_agent_inbox(session, watch);
            Ok(PostDelivery::Sent {
                seq,
                unknown_mentions,
                undelivered_mentions,
            })
        }
        PostOutcome::Stale { missed } => Ok(PostDelivery::Stale { missed }),
    }
}

/// Chase one unanswered `@` (v7 batch 3) — the room's half of the watchdog a
/// direct message has had since D44, and the reason the ledger acts rather than
/// only displays.
///
/// The wait is not a parameter. `SendMessage`'s `ack_timeout` has always been
/// documented as ignored for a room, and a per-post number would put the
/// correctness of the check back on the sender remembering to ask for one —
/// the exact failure the default exists to remove. It is the same five minutes
/// the direct path defaults to, for the same reason: long enough that a member
/// genuinely working is not nagged, short enough that a hang is not discovered
/// an hour later.
///
/// The `@all` case chases without nudging anybody: the sigil deliberately did
/// not pick a member, so neither does this — the sender is told, and the room's
/// row says what it is waiting on.
fn spawn_mention_watchdog(
    session: Arc<Session>,
    watch: crate::watch::WatchHandle,
    channel: String,
    seq: u64,
    to: String,
    from: String,
    excerpt: String,
) {
    let owner = session.instance.clone();
    let label = format!("{to} #{channel} msg #{seq} answer");
    let everyone = to == crate::channels::ALL_NAME;
    tokio::spawn(async move {
        // Registered on the first missed deadline, exactly as the direct
        // path does: an `@` answered on time leaves no trace, so the line
        // itself means "this one needed chasing".
        let mut line: Option<crate::watch::WatchId> = None;
        let report = |state: crate::watch::WatchState, detail: String, line: &mut Option<_>| {
            let id = *line.get_or_insert_with(|| {
                watch.register_with_conditions(
                    Box::new(MentionWatch {
                        label: label.clone(),
                    }),
                    Vec::new(),
                    owner.clone(),
                )
            });
            watch.set_state(id, state, Some(detail), None);
        };
        for round in 1..=crate::agents::MAX_FOLLOW_UPS {
            tokio::time::sleep(MENTION_CHASE).await;
            let Some(owed) = session.channels.open_mention(&channel, seq, &to) else {
                return;
            };
            let waited = MENTION_CHASE * u32::from(round);
            if everyone {
                report(
                    crate::watch::WatchState::Running,
                    format!("#{channel} msg #{seq} asked the room and nobody has answered"),
                    &mut line,
                );
                continue;
            }
            let accepted = session.agents.deposit(
                &to,
                crate::agents::InboxItem::Unanswered {
                    channel: channel.clone(),
                    seq,
                    from: owed.from.clone(),
                    excerpt: excerpt.clone(),
                    round,
                    waited,
                },
            );
            if !accepted {
                report(
                    crate::watch::WatchState::Failed,
                    format!(
                        "{to} is stopped; the `@` in #{channel} msg #{seq} will not be answered"
                    ),
                    &mut line,
                );
                return;
            }
            report(
                crate::watch::WatchState::Running,
                format!(
                    "waiting on @{to} in #{channel}, chased {round}/{}",
                    crate::agents::MAX_FOLLOW_UPS
                ),
                &mut line,
            );
            flush_agent_inbox(&session, &watch);
        }
        if session.channels.open_mention(&channel, seq, &to).is_some() {
            let who = if everyone {
                format!("nobody in #{channel}")
            } else {
                format!("@{to}")
            };
            report(
                crate::watch::WatchState::Failed,
                format!(
                    "{} follow-ups and {who} has still not answered {from}'s #{channel} msg #{seq}",
                    crate::agents::MAX_FOLLOW_UPS
                ),
                &mut line,
            );
        }
    });
}

/// How long an `@` may go unanswered before the room chases it. See
/// [`spawn_mention_watchdog`] for why it is a constant and not a parameter.
const MENTION_CHASE: std::time::Duration = std::time::Duration::from_secs(300);

/// Watch line for a chased `@`: driven entirely by [`spawn_mention_watchdog`],
/// so it declares no polling interval of its own.
struct MentionWatch {
    label: String,
}

impl crate::watch::Watchable for MentionWatch {
    fn label(&self) -> String {
        self.label.clone()
    }
    fn poll(&self) -> crate::watch::WatchPoll {
        crate::watch::WatchPoll {
            state: crate::watch::WatchState::Running,
            detail: None,
            payload: None,
            signal: None,
        }
    }
    fn check_interval(&self) -> Option<std::time::Duration> {
        None
    }
    fn kind(&self) -> crate::watch::WatchKind {
        crate::watch::WatchKind::Agent
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
        if member == MAIN_NAME || member == crate::channels::USER_NAME {
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

    fn main_session() -> Arc<Session> {
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
            watch: crate::app::AppCore::start(Default::default()).watch(),
            tasks: Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "t")),
            expand_tasks: tokio::sync::watch::channel(false).0,
            agents: AgentRegistry::new(),
            channels: crate::channels::ChannelRegistry::new(Default::default()),
            instance: None,
            attachments: crate::api::image::Attachments::new(),
        })
    }

    /// A depth-1 sub-session under the same registry/channel table (instance name stamped).
    fn sub_session(main: &Arc<Session>, instance: &str) -> Arc<Session> {
        Arc::new(Session {
            depth: 1,
            instance: Some(instance.to_string()),
            ..(**main).clone()
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
        let main = main_session();
        let tool = ChannelTool::new(main.clone());
        // Unknown members are rejected.
        let err = tool
            .call(
                serde_json::json!({"action": "create", "channel": "t", "members": ["ghost"]}),
                &ctx(&main),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("ghost"), "{err}");
        // depth-1 members pass; main joins automatically.
        main.agents.insert(
            "a",
            crate::agents::AgentKind::Hire,
            None,
            "a".into(),
            sub_session(&main, "a"),
        );
        let out = tool
            .call(
                serde_json::json!({"action": "create", "channel": "t", "members": ["a"]}),
                &ctx(&main),
            )
            .await
            .unwrap();
        let text = out.content.as_str().unwrap();
        assert!(text.contains("#t") && text.contains("serial"), "{text}");
        // Deeper instances are refused entry to the channel.
        let deep = Arc::new(Session {
            depth: 2,
            ..(*sub_session(&main, "deep")).clone()
        });
        main.agents.insert(
            "deep",
            crate::agents::AgentKind::Hire,
            None,
            "d".into(),
            deep,
        );
        let err = tool
            .call(
                serde_json::json!({"action": "invite", "channel": "t", "members": ["deep"]}),
                &ctx(&main),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("direct subagent"), "{err}");
        // list outputs members and mode.
        let out = tool
            .call(serde_json::json!({"action": "list"}), &ctx(&main))
            .await
            .unwrap();
        assert!(out.content.as_str().unwrap().contains("main, a"));
        assert!(tool.is_read_only(&serde_json::json!({"action": "list"})));
    }

    #[tokio::test]
    async fn post_stamps_sender_and_queues_to_running_members() {
        let main = main_session();
        main.agents.insert(
            "a",
            crate::agents::AgentKind::Hire,
            None,
            "a".into(),
            sub_session(&main, "a"),
        );
        main.agents.insert(
            "b",
            crate::agents::AgentKind::Hire,
            None,
            "b".into(),
            sub_session(&main, "b"),
        );
        let mgmt = ChannelTool::new(main.clone());
        let _ = mgmt
            .call(
                serde_json::json!({"action": "create", "channel": "t", "members": ["a", "b"], "mode": "free"}),
                &ctx(&main),
            )
            .await
            .unwrap();
        // Speaking from a's (Running) perspective: stamped a; b is Running →
        // accumulates in its inbox. The post names @b so the line passes the
        // v6 wake gate and b's finish turns it into a continuation.
        let post_a = crate::tool::agent::SendMessageTool::new(sub_session(&main, "a"));
        let out = post_a
            .call(
                serde_json::json!({"to": "#t", "message": "@b hello"}),
                &ctx(&main),
            )
            .await
            .unwrap();
        assert!(out.content.as_str().unwrap().contains("msg #1"));
        let items = main
            .agents
            .finish("b", Vec::new(), 1)
            .unwrap_or_else(|| panic!("b's inbox should have a message"))
            .items;
        assert!(
            matches!(&items[..], [crate::agents::InboxItem::Channel { from, text, .. }]
                if from == "a" && text == "@b hello"),
            "stamped as a, and naming b set the needs-you-now bit"
        );
        // Main posts: stamped main; its own inbox only receives others' posts.
        let post_main = crate::tool::agent::SendMessageTool::new(main.clone());
        let _ = post_main
            .call(
                serde_json::json!({"to": "#t", "message": "quiet"}),
                &ctx(&main),
            )
            .await
            .unwrap();
        // The line named b, not main, so it waits in main's pen (v6); force
        // the age release to read it.
        let mail = main.channels.drain_main_mail();
        assert_eq!(mail.len(), 1, "{mail:?}");
        assert!(mail[0].contains("a: @b hello"));
        // Non-member posts error out — refused by the addressing rules before
        // the room's own membership check is ever reached.
        let post_c = crate::tool::agent::SendMessageTool::new(sub_session(&main, "c"));
        let err = post_c
            .call(serde_json::json!({"to": "#t", "message": "x"}), &ctx(&main))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not a member of #t"), "{err}");
    }

    /// The @-gate at the delivery layer (v6): a mention rides into a running
    /// member's turn at its next tool round, unnamed traffic stays queued for
    /// the batch clock, and mentions that reach nobody — unknown or stopped —
    /// come back in the sender's tool result.
    #[tokio::test]
    async fn a_mention_interrupts_and_a_misfire_is_reported() {
        let main = main_session();
        for name in ["a", "b"] {
            main.agents.insert(
                name,
                crate::agents::AgentKind::Hire,
                None,
                name.into(),
                sub_session(&main, name),
            );
        }
        let mgmt = ChannelTool::new(main.clone());
        let _ = mgmt
            .call(
                serde_json::json!({"action": "create", "channel": "t", "members": ["a", "b"], "mode": "free"}),
                &ctx(&main),
            )
            .await
            .unwrap();
        let post_a = crate::tool::agent::SendMessageTool::new(sub_session(&main, "a"));

        // v7: a running member absorbs whatever is waiting at its next tool
        // boundary, named or not — that is the steer. Two lines land while b
        // works and both come out together, in room order.
        let _ = post_a
            .call(
                serde_json::json!({"to": "#t", "message": "fyi: still digging"}),
                &ctx(&main),
            )
            .await
            .unwrap();
        let _ = post_a
            .call(
                serde_json::json!({"to": "#t", "message": "@b your turn"}),
                &ctx(&main),
            )
            .await
            .unwrap();
        let items = main.agents.take_running("b", 0);
        assert_eq!(items.len(), 2, "both lines ride the same boundary");
        assert!(
            matches!(&items[0], crate::agents::InboxItem::Channel { seq: 1, .. }),
            "room order first, so the seen cursor never skips a line"
        );
        assert!(matches!(
            &items[1],
            crate::agents::InboxItem::Channel { seq: 2, .. }
        ));

        // Misfires: an unknown token, then a stopped member, both named to the sender.
        let out = post_a
            .call(
                serde_json::json!({"to": "#t", "message": "@ghost see above"}),
                &ctx(&main),
            )
            .await
            .unwrap();
        assert!(
            out.content
                .as_str()
                .unwrap_or_default()
                .contains("@ghost is not in #t"),
            "{out:?}"
        );
        let _ = main.agents.stop("b");
        let out = post_a
            .call(
                serde_json::json!({"to": "#t", "message": "@b are you there?"}),
                &ctx(&main),
            )
            .await
            .unwrap();
        assert!(
            out.content
                .as_str()
                .unwrap_or_default()
                .contains("@b is stopped"),
            "{out:?}"
        );
    }

    #[tokio::test]
    async fn serial_bounce_returns_increments_as_result() {
        let main = main_session();
        main.agents.insert(
            "a",
            crate::agents::AgentKind::Hire,
            None,
            "a".into(),
            sub_session(&main, "a"),
        );
        main.agents.insert(
            "b",
            crate::agents::AgentKind::Hire,
            None,
            "b".into(),
            sub_session(&main, "b"),
        );
        let mgmt = ChannelTool::new(main.clone());
        let _ = mgmt
            .call(
                serde_json::json!({"action": "create", "channel": "count", "members": ["a", "b"]}),
                &ctx(&main),
            )
            .await
            .unwrap();
        let post_a = crate::tool::agent::SendMessageTool::new(sub_session(&main, "a"));
        let post_b = crate::tool::agent::SendMessageTool::new(sub_session(&main, "b"));
        let _ = post_a
            .call(
                serde_json::json!({"to": "#count", "message": "1"}),
                &ctx(&main),
            )
            .await
            .unwrap();
        // b lags → bounced back (tool result, not an error) with increments attached.
        let out = post_b
            .call(
                serde_json::json!({"to": "#count", "message": "1"}),
                &ctx(&main),
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
                &ctx(&main),
            )
            .await
            .unwrap();
        assert!(out.content.as_str().unwrap().contains("msg #2"));
    }
}
