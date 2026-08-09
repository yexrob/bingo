//! Agent channels (D29 step two, experimental feature `experimental.agentChannels`).
//!
//! The engine has only four primitives; everything else is prompting:
//! 1. A channel = a member list (visibility: messages go to every member's inbox,
//!    delivered in total order);
//! 2. serial | free commit check (serial: a sender behind the channel head is bounced
//!    back with the increments; the runtime only judges "staleness" — semantic
//!    conflicts are left to the model: optimistic locking with the model as resolver);
//! 3. Wake-up follows delivery (capability is universal, choice is autonomous:
//!    silence = don't Post after waking, a zero-cost absorbing state);
//! 4. Sender stamping by the runtime (from comes from the session instance name and
//!    cannot be forged) + a budget gate (freezes the channel on overrun and notifies
//!    the main agent instead of silently burning money).
//!
//! This module is pure state (no watch/agents dependencies); delivery wake-ups and
//! display-row updates are orchestrated by the tool layer (`tool::channel`). The main
//! agent's member name is always `main`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// Reserved member name of the main agent in channels.
pub const HUB_NAME: &str = "main";
/// Reserved member name of the user (a human) in channels: speaks under this identity
/// in the TUI channel room, shown right-aligned in the WeChat-style view; like main,
/// auto-seated, cannot be removed, exempt from budgets.
pub const USER_NAME: &str = "user";

/// Channel speaking mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMode {
    /// Commit check: must have seen the latest message before speaking; falling behind bounces back (emergent ordering).
    Serial,
    /// Allows interleaving (brainstorming, parallel independent output).
    Free,
}

impl ChannelMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Serial => "serial",
            Self::Free => "free",
        }
    }
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "serial" => Ok(Self::Serial),
            "free" => Ok(Self::Free),
            other => Err(format!("unknown mode {other} (available: serial / free)")),
        }
    }
}

/// A channel message (seq is total order within the channel).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub seq: u64,
    pub from: String,
    pub text: String,
    /// Wall-clock landing time, unix seconds. `0` = unknown: share documents
    /// written before D43 carry no clock, and the room renders them without a
    /// time stamp rather than inventing one.
    #[serde(default)]
    pub at: u64,
}

/// Wall-clock now in unix seconds (0 if the system clock predates the epoch).
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Budgets: freeze on overrun (read from settings.experimental; defaults 500/50).
#[derive(Debug, Clone, Copy)]
pub struct ChannelLimits {
    /// Total message cap per channel.
    pub channel_total: u64,
    /// Message cap per agent per channel.
    pub per_agent: u64,
}

impl Default for ChannelLimits {
    fn default() -> Self {
        Self {
            channel_total: 500,
            per_agent: 50,
        }
    }
}

impl ChannelLimits {
    pub fn from_settings(settings: &crate::settings::Settings) -> Self {
        let d = Self::default();
        Self {
            channel_total: settings
                .experimental
                .channel_message_limit
                .unwrap_or(d.channel_total),
            per_agent: settings
                .experimental
                .agent_message_limit
                .unwrap_or(d.per_agent),
        }
    }
}

/// Result of post.
#[derive(Debug)]
pub enum PostOutcome {
    /// Committed: deliver to these members (excluding the sender and hub — hub goes through hub_mail).
    Sent {
        seq: u64,
        deliveries: Vec<(String, ChannelMessage)>,
    },
    /// serial behind: not sent; attaches the missed messages (already counted as read by
    /// the sender — they reach its context via the tool result, and it decides whether
    /// to send as-is, revise, or drop).
    Stale { missed: Vec<ChannelMessage> },
}

/// Snapshot for list.
#[derive(Debug, Clone)]
pub struct ChannelStatus {
    pub name: String,
    pub members: Vec<String>,
    pub mode: ChannelMode,
    pub seq: u64,
    pub frozen: bool,
}

struct Channel {
    members: Vec<String>,
    mode: ChannelMode,
    seq: u64,
    log: Vec<ChannelMessage>,
    /// Highest channel sequence each member has seen (the cursor for serial commit checks).
    seen: HashMap<String, u64>,
    /// Per-member post count (per_agent budget).
    sent: HashMap<String, u64>,
    frozen: bool,
    /// Channel-level total message cap override (D31 team.json channel.messageLimit;
    /// None = use registry-level ChannelLimits.channel_total).
    message_limit: Option<u64>,
    /// Watch entry of the display row (◇ #name).
    watch_id: Option<crate::watch::WatchId>,
}

struct Inner {
    channels: HashMap<String, Channel>,
    /// Channel messages pending injection into the main agent's context (formatted text).
    hub_mail: Vec<String>,
    limits: ChannelLimits,
}

/// Session-level channel registry (Session holds the Arc; shared by child sessions).
pub struct ChannelRegistry {
    inner: Mutex<Inner>,
    /// Share persistence (Option semantics: behavior is unchanged when not attached; once attached, create/invite/kick/post sync snapshots).
    share: Mutex<Option<Arc<crate::share::ShareStore>>>,
}

fn format_hub_line(channel: &str, msg: &ChannelMessage) -> String {
    format!("[#{channel} msg #{}] {}: {}", msg.seq, msg.from, msg.text)
}

impl ChannelRegistry {
    pub fn new(limits: ChannelLimits) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                channels: HashMap::new(),
                hub_mail: Vec::new(),
                limits,
            }),
            share: Mutex::new(None),
        })
    }

    /// Attach share persistence: channel metadata/message changes sync into the share document from now on.
    pub fn attach_share(&self, store: Arc<crate::share::ShareStore>) {
        *self.share.lock().unwrap_or_else(|e| e.into_inner()) = Some(store);
    }

    /// Write a channel's latest metadata (mode + members) into the share document (no-op without a store).
    fn sync_channel_meta(&self, name: &str) {
        let Some(store) = self.share.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
            return;
        };
        let inner = self.lock();
        let Some(ch) = inner.channels.get(name) else {
            return;
        };
        store.upsert_channel_meta(name, ch.mode, ch.members.clone());
        store.persist();
    }

    /// Append a landed channel message to the share document (no-op without a store).
    fn sync_channel_message(&self, name: &str, msg: &ChannelMessage) {
        let Some(store) = self.share.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
            return;
        };
        store.append_channel_message(name, msg.clone());
        store.persist();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Create a channel (hub auto-joins as member). Member existence/depth is validated by the tool layer.
    pub fn create(
        &self,
        name: &str,
        members: Vec<String>,
        mode: ChannelMode,
    ) -> Result<(), String> {
        let name = name.trim_start_matches('#');
        if name.is_empty() {
            return Err("channel name must not be empty".to_string());
        }
        {
            let mut inner = self.lock();
            if inner.channels.contains_key(name) {
                return Err(format!("channel #{name} already exists"));
            }
            let mut all = vec![HUB_NAME.to_string(), USER_NAME.to_string()];
            for m in members {
                if m != HUB_NAME && m != USER_NAME && !all.contains(&m) {
                    all.push(m);
                }
            }
            inner.channels.insert(
                name.to_string(),
                Channel {
                    members: all,
                    mode,
                    seq: 0,
                    log: Vec::new(),
                    seen: HashMap::new(),
                    sent: HashMap::new(),
                    frozen: false,
                    message_limit: None,
                    watch_id: None,
                },
            );
        }
        self.sync_channel_meta(name);
        Ok(())
    }

    /// Channel-level total message cap override (D31 team.json channel.messageLimit).
    pub fn set_message_limit(&self, name: &str, limit: u64) -> Result<(), String> {
        if limit == 0 {
            return Err("messageLimit must be a positive integer".to_string());
        }
        let mut inner = self.lock();
        let Some(ch) = inner.channels.get_mut(name) else {
            return Err(format!("no channel #{name}"));
        };
        ch.message_limit = Some(limit);
        Ok(())
    }

    pub fn set_watch(&self, name: &str, id: crate::watch::WatchId) {
        if let Some(ch) = self.lock().channels.get_mut(name) {
            ch.watch_id = Some(id);
        }
    }

    pub fn invite(&self, name: &str, member: &str) -> Result<(), String> {
        {
            let mut inner = self.lock();
            let Some(ch) = inner.channels.get_mut(name) else {
                return Err(format!("no channel #{name}"));
            };
            if ch.members.iter().any(|m| m == member) {
                return Err(format!("{member} is already in #{name}"));
            }
            ch.members.push(member.to_string());
            // Late joiners don't get backlog replay: they start "listening" from the current
            // head (seen set to the current seq, so the serial check won't bounce on pre-join history).
            let seq = ch.seq;
            ch.seen.insert(member.to_string(), seq);
        }
        self.sync_channel_meta(name);
        Ok(())
    }

    pub fn kick(&self, name: &str, member: &str) -> Result<(), String> {
        if member == HUB_NAME || member == USER_NAME {
            return Err(format!(
                "{member} is a reserved member and cannot be removed from a channel"
            ));
        }
        {
            let mut inner = self.lock();
            let Some(ch) = inner.channels.get_mut(name) else {
                return Err(format!("no channel #{name}"));
            };
            let before = ch.members.len();
            ch.members.retain(|m| m != member);
            if ch.members.len() == before {
                return Err(format!("{member} is not in #{name}"));
            }
        }
        self.sync_channel_meta(name);
        Ok(())
    }

    /// Remove an instance from all channels on deletion (called by the tool layer on AgentControl delete).
    pub fn remove_member_everywhere(&self, member: &str) {
        let mut inner = self.lock();
        for ch in inner.channels.values_mut() {
            ch.members.retain(|m| m != member);
        }
    }

    /// Post a message. The runtime only does three things: stamping (from is taken by the
    /// caller from the session instance name; the model can't specify it), serial staleness
    /// check, and the budget gate; what to say / whether to resend is entirely up to the model.
    pub fn post(&self, from: &str, name: &str, text: &str) -> Result<PostOutcome, String> {
        let mut inner = self.lock();
        let limits = inner.limits;
        let hub_line;
        let outcome = {
            let Some(ch) = inner.channels.get_mut(name) else {
                return Err(format!("no channel #{name}"));
            };
            if !ch.members.iter().any(|m| m == from) {
                return Err(format!("{from} is not a member of #{name}"));
            }
            // Channel-level cap: team override wins, otherwise registry-level.
            let channel_total = ch.message_limit.unwrap_or(limits.channel_total);
            if ch.frozen {
                return Err(format!(
                    "#{name} is frozen (hit the {channel_total} total message cap); no more posts"
                ));
            }
            // Serial commit check: fall behind → bounce back + increments (the bounced
            // content enters the context, counted as read).
            if ch.mode == ChannelMode::Serial {
                let seen = ch.seen.get(from).copied().unwrap_or(0);
                if seen < ch.seq {
                    let missed: Vec<ChannelMessage> =
                        ch.log.iter().filter(|m| m.seq > seen).cloned().collect();
                    ch.seen.insert(from.to_string(), ch.seq);
                    return Ok(PostOutcome::Stale { missed });
                }
            }
            let sent = ch.sent.get(from).copied().unwrap_or(0);
            if from != HUB_NAME && from != USER_NAME && sent >= limits.per_agent {
                return Err(format!(
                    "your posts in #{name} hit the per-agent cap {} (budget gate)",
                    limits.per_agent
                ));
            }
            if ch.seq >= channel_total {
                ch.frozen = true;
                inner.hub_mail.push(format!(
                    "⚠ channel #{name} hit the {channel_total} total message cap and is now frozen (further posts will be rejected)",
                ));
                return Err(format!(
                    "#{name} hit the {channel_total} total message cap; the channel is frozen"
                ));
            }
            ch.seq += 1;
            let msg = ChannelMessage {
                seq: ch.seq,
                from: from.to_string(),
                text: text.to_string(),
                at: now_unix(),
            };
            ch.log.push(msg.clone());
            self.sync_channel_message(name, &msg);
            ch.seen.insert(from.to_string(), ch.seq);
            *ch.sent.entry(from.to_string()).or_insert(0) += 1;
            let deliveries: Vec<(String, ChannelMessage)> = ch
                .members
                .iter()
                .filter(|m| m.as_str() != from && m.as_str() != HUB_NAME && m.as_str() != USER_NAME)
                .map(|m| (m.clone(), msg.clone()))
                .collect();
            hub_line = if from != HUB_NAME && ch.members.iter().any(|m| m == HUB_NAME) {
                Some(format_hub_line(name, &msg))
            } else {
                None
            };
            PostOutcome::Sent {
                seq: msg.seq,
                deliveries,
            }
        };
        if let Some(line) = hub_line {
            inner.hub_mail.push(line);
        }
        Ok(outcome)
    }

    /// Mark the member's inbox as digested up to seq (its running turn was injected with channel messages up to seq).
    pub fn mark_seen(&self, member: &str, name: &str, seq: u64) {
        if let Some(ch) = self.lock().channels.get_mut(name) {
            let cursor = ch.seen.entry(member.to_string()).or_insert(0);
            if *cursor < seq {
                *cursor = seq;
            }
        }
    }

    /// How far a member has read (0 = nothing). The sidebar turns this into an
    /// unread badge for `user`.
    pub fn seen_of(&self, member: &str, name: &str) -> u64 {
        self.lock()
            .channels
            .get(name)
            .and_then(|ch| ch.seen.get(member).copied())
            .unwrap_or(0)
    }

    /// Display-row snapshot: (watch_id, detail, tail text of the log).
    pub fn row_snapshot(
        &self,
        name: &str,
    ) -> Option<(Option<crate::watch::WatchId>, String, String)> {
        const TAIL: usize = 50;
        let inner = self.lock();
        let ch = inner.channels.get(name)?;
        let detail = match ch.log.last() {
            Some(last) => format!(
                "{} msgs · latest {}: {}",
                ch.seq,
                last.from,
                crate::tool::agent::excerpt(&last.text)
            ),
            None => "0 msgs".to_string(),
        };
        let skipped = ch.log.len().saturating_sub(TAIL);
        let mut lines: Vec<String> = Vec::new();
        if skipped > 0 {
            lines.push(format!("… ({skipped} earlier msgs skipped)"));
        }
        lines.extend(
            ch.log
                .iter()
                .skip(skipped)
                .map(|m| format!("{}. {}: {}", m.seq, m.from, m.text)),
        );
        Some((ch.watch_id, detail, lines.join("\n")))
    }

    /// Single-channel snapshot (TUI room header).
    pub fn info(&self, name: &str) -> Option<ChannelStatus> {
        let inner = self.lock();
        inner.channels.get(name).map(|ch| ChannelStatus {
            name: name.to_string(),
            members: ch.members.clone(),
            mode: ch.mode,
            seq: ch.seq,
            frozen: ch.frozen,
        })
    }

    /// Full message log (TUI room rendering; cloned, the caller polls per frame).
    pub fn log_of(&self, name: &str) -> Vec<ChannelMessage> {
        self.lock()
            .channels
            .get(name)
            .map(|ch| ch.log.clone())
            .unwrap_or_default()
    }

    pub fn list(&self) -> Vec<ChannelStatus> {
        let inner = self.lock();
        let mut out: Vec<ChannelStatus> = inner
            .channels
            .iter()
            .map(|(name, ch)| ChannelStatus {
                name: name.clone(),
                members: ch.members.clone(),
                mode: ch.mode,
                seq: ch.seq,
                frozen: ch.frozen,
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn has_hub_mail(&self) -> bool {
        !self.lock().hub_mail.is_empty()
    }

    /// Drain channel messages pending injection into the main agent (batch-injected at turn boundaries).
    pub fn drain_hub_mail(&self) -> Vec<String> {
        std::mem::take(&mut self.lock().hub_mail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Arc<ChannelRegistry> {
        ChannelRegistry::new(ChannelLimits::default())
    }

    fn sent(outcome: PostOutcome) -> (u64, Vec<(String, ChannelMessage)>) {
        match outcome {
            PostOutcome::Sent { seq, deliveries } => (seq, deliveries),
            PostOutcome::Stale { .. } => panic!("should land"),
        }
    }

    #[test]
    fn create_invite_kick_and_list() {
        let reg = registry();
        reg.create("table", vec!["a".into(), "b".into()], ChannelMode::Free)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            reg.create("table", vec![], ChannelMode::Free).is_err(),
            "duplicate name"
        );
        assert!(
            reg.create("", vec![], ChannelMode::Free).is_err(),
            "empty name"
        );
        let st = &reg.list()[0];
        assert_eq!(
            st.members,
            vec!["main", "user", "a", "b"],
            "hub and user auto-join and lead the list"
        );
        reg.invite("table", "c").unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.invite("table", "c").is_err(), "duplicate invite");
        reg.kick("table", "b").unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.kick("table", "b").is_err(), "not present");
        assert!(reg.kick("table", "main").is_err(), "hub cannot be removed");
        assert!(reg.kick("table", "user").is_err(), "user cannot be removed");
        assert_eq!(reg.list()[0].members, vec!["main", "user", "a", "c"]);
        reg.remove_member_everywhere("a");
        assert_eq!(reg.list()[0].members, vec!["main", "user", "c"]);
        // Single-channel snapshot and full-log accessors.
        assert_eq!(
            reg.info("table").unwrap_or_else(|| panic!("has one")).seq,
            0
        );
        assert!(reg.info("nope").is_none());
        assert!(reg.log_of("table").is_empty());
    }

    #[test]
    fn post_fans_out_excluding_sender_and_hub() {
        let reg = registry();
        reg.create(
            "t",
            vec!["a".into(), "b".into(), "c".into()],
            ChannelMode::Free,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let (seq, deliveries) = sent(
            reg.post("a", "t", "hello everyone")
                .unwrap_or_else(|e| panic!("{e}")),
        );
        assert_eq!(seq, 1);
        let names: Vec<&str> = deliveries.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(names, vec!["b", "c"], "not delivered to the sender or hub");
        assert!(
            deliveries
                .iter()
                .all(|(_, m)| m.from == "a" && m.text == "hello everyone")
        );
        // Hub is a member: messages go to hub_mail; the hub's own posts don't.
        assert!(reg.has_hub_mail());
        let mail = reg.drain_hub_mail();
        assert_eq!(mail, vec!["[#t msg #1] a: hello everyone"]);
        let _ = sent(
            reg.post("main", "t", "quiet")
                .unwrap_or_else(|e| panic!("{e}")),
        );
        assert!(!reg.has_hub_mail(), "hub's own posts do not flow back");
        // user (a human) is a natural member: can post, hub hears it, doesn't consume the per_agent budget.
        let (_, deliveries) = sent(
            reg.post("user", "t", "everyone stop")
                .unwrap_or_else(|e| panic!("{e}")),
        );
        assert_eq!(
            deliveries
                .iter()
                .map(|(m, _)| m.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"],
            "user's post wakes all agent members"
        );
        assert!(reg.drain_hub_mail()[0].contains("user: everyone stop"));
        // Non-member / unknown channel error.
        assert!(reg.post("ghost", "t", "x").is_err());
        assert!(reg.post("a", "nope", "x").is_err());
    }

    #[test]
    fn serial_bounces_stale_sender_with_increments() {
        let reg = registry();
        reg.create("count", vec!["a".into(), "b".into()], ChannelMode::Serial)
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(
            reg.post("a", "count", "1")
                .unwrap_or_else(|e| panic!("{e}")),
        );
        // b hasn't seen a's "1" (seen=0 < seq=1) → bounce back with increments.
        match reg
            .post("b", "count", "1")
            .unwrap_or_else(|e| panic!("{e}"))
        {
            PostOutcome::Stale { missed } => {
                assert_eq!(missed.len(), 1);
                assert_eq!(missed[0].from, "a");
                assert_eq!(missed[0].text, "1");
            }
            PostOutcome::Sent { .. } => panic!("should bounce back"),
        }
        // Bounce counts as read: the resend commits (the model says "2" instead).
        let (seq, _) = sent(
            reg.post("b", "count", "2")
                .unwrap_or_else(|e| panic!("{e}")),
        );
        assert_eq!(seq, 2, "retry lands; ordering emerges");
        // mark_seen: after inbox injection, a's cursor advances, no bounce.
        reg.mark_seen("a", "count", 2);
        let (seq, _) = sent(
            reg.post("a", "count", "3")
                .unwrap_or_else(|e| panic!("{e}")),
        );
        assert_eq!(seq, 3);
        // Free mode doesn't check.
        reg.create(
            "brainstorm",
            vec!["a".into(), "b".into()],
            ChannelMode::Free,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(
            reg.post("a", "brainstorm", "idea one")
                .unwrap_or_else(|e| panic!("{e}")),
        );
        let _ = sent(
            reg.post("b", "brainstorm", "idea two")
                .unwrap_or_else(|e| panic!("{e}")),
        );
    }

    #[test]
    fn late_joiner_starts_from_current_head() {
        let reg = registry();
        reg.create("t", vec!["a".into()], ChannelMode::Serial)
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(
            reg.post("a", "t", "old news")
                .unwrap_or_else(|e| panic!("{e}")),
        );
        reg.invite("t", "late").unwrap_or_else(|e| panic!("{e}"));
        // Late joiner's seen = head at join time: no backlog bounce, can post immediately.
        let (seq, _) = sent(
            reg.post("late", "t", "I'm here")
                .unwrap_or_else(|e| panic!("{e}")),
        );
        assert_eq!(seq, 2);
    }

    #[test]
    fn per_channel_message_limit_overrides_registry() {
        let reg = ChannelRegistry::new(ChannelLimits {
            channel_total: 100,
            per_agent: 100,
        });
        reg.create("t", vec!["a".into()], ChannelMode::Free)
            .unwrap_or_else(|e| panic!("{e}"));
        // Channel-level override is 1: the second message freezes it.
        reg.set_message_limit("t", 1)
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(reg.post("a", "t", "1").unwrap_or_else(|e| panic!("{e}")));
        let err = reg.post("a", "t", "2").unwrap_err();
        assert!(err.contains("frozen"), "{err}");
        assert!(reg.list()[0].frozen);
        // 0 is rejected; unknown channel errors.
        assert!(reg.set_message_limit("t", 0).is_err());
        assert!(reg.set_message_limit("nope", 5).is_err());
    }

    #[test]
    fn budgets_freeze_channel_and_notify_hub_once() {
        let reg = ChannelRegistry::new(ChannelLimits {
            channel_total: 2,
            per_agent: 2,
        });
        reg.create("t", vec!["a".into(), "b".into()], ChannelMode::Free)
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(reg.post("a", "t", "1").unwrap_or_else(|e| panic!("{e}")));
        let _ = sent(reg.post("a", "t", "2").unwrap_or_else(|e| panic!("{e}")));
        // a hits the per_agent cap.
        let err = reg.post("a", "t", "3").unwrap_err();
        assert!(err.contains("cap 2"), "{err}");
        // b triggers the channel total cap: freeze + hub gets one warning.
        let _ = reg.drain_hub_mail();
        let err = reg.post("b", "t", "x").unwrap_err();
        assert!(err.contains("frozen"), "{err}");
        assert!(reg.list()[0].frozen);
        let mail = reg.drain_hub_mail();
        assert_eq!(mail.len(), 1, "{mail:?}");
        assert!(mail[0].contains("now frozen"));
        // Posting after freeze: rejected, no repeated notification.
        let err = reg.post("b", "t", "y").unwrap_err();
        assert!(err.contains("is frozen"), "{err}");
        assert!(!reg.has_hub_mail());
    }

    #[test]
    fn share_hooks_track_create_invite_kick_post() {
        let root = std::env::temp_dir().join(format!("bingo-ch-{}-share", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = crate::share::ShareStore::load_or_create(&root.join("s.json"))
            .unwrap_or_else(|e| panic!("{e}"));
        let reg = ChannelRegistry::new(ChannelLimits::default());
        reg.attach_share(store.clone());

        // create → channel metadata (mode + members).
        reg.create("t", vec!["a".into()], ChannelMode::Free)
            .unwrap_or_else(|e| panic!("{e}"));
        let doc = store.snapshot();
        assert_eq!(doc.channels.len(), 1);
        assert_eq!(doc.channels[0].mode, "free");
        assert_eq!(doc.channels[0].members, vec!["main", "user", "a"]);
        assert!(doc.channels[0].messages.is_empty());

        // invite/kick → member updates (messages kept).
        reg.invite("t", "b").unwrap_or_else(|e| panic!("{e}"));
        reg.kick("t", "a").unwrap_or_else(|e| panic!("{e}"));
        let doc = store.snapshot();
        assert_eq!(doc.channels[0].members, vec!["main", "user", "b"]);

        // post Sent → message appended.
        let (seq, _) = sent(reg.post("b", "t", "hi").unwrap_or_else(|e| panic!("{e}")));
        assert_eq!(seq, 1);
        let doc = store.snapshot();
        assert_eq!(doc.channels[0].messages.len(), 1);
        assert_eq!(doc.channels[0].messages[0].from, "b");
        assert_eq!(doc.channels[0].messages[0].text, "hi");
        // Disk roundtrip: reloading yields identical data.
        store.persist();
        let reloaded = crate::share::ShareStore::load_or_create(&root.join("s.json"))
            .unwrap_or_else(|e| panic!("{e}"));
        let doc = reloaded.snapshot();
        assert_eq!(doc.channels[0].messages[0].seq, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn row_snapshot_summarizes_log() {
        let reg = registry();
        reg.create("t", vec!["a".into()], ChannelMode::Free)
            .unwrap_or_else(|e| panic!("{e}"));
        let (_, detail, payload) = reg
            .row_snapshot("t")
            .unwrap_or_else(|| panic!("has channel"));
        assert_eq!(detail, "0 msgs");
        assert!(payload.is_empty());
        let _ = sent(
            reg.post("a", "t", "first line")
                .unwrap_or_else(|e| panic!("{e}")),
        );
        let (_, detail, payload) = reg
            .row_snapshot("t")
            .unwrap_or_else(|| panic!("has channel"));
        assert!(
            detail.contains("1 msgs") && detail.contains("a: first line"),
            "{detail}"
        );
        assert_eq!(payload, "1. a: first line");
        assert!(reg.row_snapshot("nope").is_none());
    }
}
