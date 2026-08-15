//! Agent channels (D29 step two, experimental feature `experimental.agentChannels`).
//!
//! **Vocabulary** (D95): what this module calls a *channel* is what the UI and
//! the docs call a **room** — the only group-chat primitive there is. The
//! domain keeps its own name (renaming a persisted schema, a tool, a settings
//! key and a watch kind to say the same thing would be churn, not clarity);
//! everything a user or an agent reads says "room".
//!
//! A room's members are an **arbitrary subset of the team**. Neither the user
//! nor the main agent is seated automatically: agents may form rooms among
//! themselves, and a room the user is not in is one they can find and read but
//! not speak in until they join. Who gets seated on creation is the tool
//! layer's policy (it stamps the creator); this module only records the set.
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
pub const MAIN_NAME: &str = "main";
/// Reserved member name of the user (a human) in channels: speaks under this
/// identity in a room and is shown as the sender of their own messages. Exempt
/// from budgets, like `main`. Since D95 the user is an *ordinary* member in
/// every other respect: not seated automatically, and free to leave a room they
/// joined — a roster the user could never leave was a roster, not a membership.
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

/// What a log entry is. A room's log holds two kinds of thing and they are not
/// the same thing said twice: somebody spoke, or the roster changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageKind {
    /// A member said this.
    #[default]
    Said,
    /// A membership change: `from` joined or left the room. Nobody typed it, so
    /// it renders as a dim system line rather than as speech, it is not
    /// delivered into anybody's context, and it never counts as something a
    /// serial sender had to have read before speaking.
    Membership,
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
    /// Speech or a roster change (D95). Defaulted rather than required: share
    /// documents written before D95 hold only speech, and reading one must not
    /// fail over a field that did not exist when it was written.
    #[serde(default)]
    pub kind: MessageKind,
}

/// The two roster changes, as they read in a room.
pub const JOINED: &str = "joined";
pub const LEFT: &str = "left";

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
    /// Committed: deliver to these members (excluding the sender and main — main goes through main_mail).
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
    /// The main agent's inbox, pending injection into its context (formatted text).
    /// Room relays and direct messages share it (D98) so there is exactly one
    /// drain-and-inject seam into the host turn loop.
    main_mail: Vec<String>,
    /// A direct message in `main_mail` asked for the attention channel (D98).
    /// Independent of the mail itself: the turn that drains the mail and the
    /// surface that rings the bell are different readers on different clocks,
    /// and a bell owed must survive the drain that beat it.
    main_mail_urgent: bool,
    limits: ChannelLimits,
}

/// Session-level channel registry (Session holds the Arc; shared by child sessions).
pub struct ChannelRegistry {
    inner: Mutex<Inner>,
    /// Share persistence (Option semantics: behavior is unchanged when not attached; once attached, create/invite/kick/post sync snapshots).
    share: Mutex<Option<Arc<crate::share::ShareStore>>>,
}

fn format_main_line(channel: &str, msg: &ChannelMessage) -> String {
    format!("[#{channel} msg #{}] {}: {}", msg.seq, msg.from, msg.text)
}

/// Opening of the line a direct message to the main agent arrives under (D98):
/// `[message from @scout]`, on its own line above the text.
///
/// The shape is [`crate::tool::agent::DM_FROM_USER_MARKER`]'s, with the sender
/// named — the one thing that marker never had to carry, because the human is
/// the only human. `main` hears from many agents, so its marker names which.
/// [`crate::tui::buffer::line_source`] is the single parser of this shape.
pub const MAIN_MESSAGE_PREFIX: &str = "[message from @";

/// One direct message as it enters the main agent's context.
pub fn format_main_message(from: &str, text: &str) -> String {
    format!("{MAIN_MESSAGE_PREFIX}{from}]\n{text}")
}

/// Write a roster change into the room's record and hand back the entry.
///
/// It takes a sequence number because it *is* part of the room's total order —
/// a reader must be able to tell whether a member spoke before or after they
/// arrived. It is deliberately not delivered anywhere: waking every member
/// because a roster changed is the flooding D94 removed, and an agent that
/// wants the roster asks for it. Nor does it make anybody stale — the serial
/// check reads speech only, so a join can never bounce a post already in
/// flight.
fn record_membership(channel: &mut Channel, member: &str, what: &str) -> ChannelMessage {
    channel.seq += 1;
    let event = ChannelMessage {
        seq: channel.seq,
        from: member.to_string(),
        text: what.to_string(),
        at: now_unix(),
        kind: MessageKind::Membership,
    };
    channel.log.push(event.clone());
    event
}

impl ChannelRegistry {
    pub fn new(limits: ChannelLimits) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                channels: HashMap::new(),
                main_mail: Vec::new(),
                main_mail_urgent: false,
                limits,
            }),
            share: Mutex::new(None),
        })
    }

    /// Replace share persistence and ensure existing channels can accept future messages.
    pub fn attach_share(&self, store: Arc<crate::share::ShareStore>) {
        let inner = self.lock();
        for (name, channel) in &inner.channels {
            store.upsert_channel_meta(name, channel.mode, channel.members.clone());
        }
        *self.share.lock().unwrap_or_else(|e| e.into_inner()) = Some(store.clone());
        store.persist();
    }

    pub fn align_with_share(&self, store: &crate::share::ShareStore) {
        let doc = store.snapshot();
        let mut inner = self.lock();
        for saved in doc.channels {
            let Some(channel) = inner.channels.get_mut(&saved.name) else {
                continue;
            };
            if saved.messages.last().map_or(0, |message| message.seq) > channel.seq {
                channel.log = saved.messages;
                channel.seq = channel.log.last().map_or(0, |message| message.seq);
                channel.sent.clear();
                channel.seen.retain(|_, seen| *seen <= channel.seq);
            }
        }
    }

    pub fn detach_share(&self) {
        *self.share.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    #[cfg(test)]
    pub fn has_share(&self) -> bool {
        self.share
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
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

    /// Create a room with exactly these members — an arbitrary subset of the
    /// team, which need contain neither the user nor the main agent (D95).
    ///
    /// Nobody is seated behind the caller's back. Auto-seating `main` and
    /// `user` made every room the user's room by construction, which is the one
    /// thing the room model says a room is not: agents form rooms among
    /// themselves, and the user reaches those by finding them in the directory
    /// and joining. Who *should* be seated on creation is policy, and policy
    /// lives at the tool layer, where the creator's identity is known
    /// ([`crate::tool::channel`] stamps it) and where member existence and
    /// depth are already validated.
    pub fn create(
        &self,
        name: &str,
        members: Vec<String>,
        mode: ChannelMode,
    ) -> Result<(), String> {
        let name = name.trim_start_matches('#');
        if name.is_empty() {
            return Err("room name must not be empty".to_string());
        }
        {
            let mut inner = self.lock();
            if inner.channels.contains_key(name) {
                return Err(format!("room #{name} already exists"));
            }
            let mut all: Vec<String> = Vec::new();
            for m in members {
                if !m.is_empty() && !all.contains(&m) {
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

    /// Is this name currently seated in the room? The one question the display
    /// side asks: a room the user is in is a conversation of theirs (bar,
    /// switcher, composer), a room they are not in is a place they can read.
    pub fn is_member(&self, name: &str, member: &str) -> bool {
        self.lock()
            .channels
            .get(name)
            .is_some_and(|ch| ch.members.iter().any(|m| m == member))
    }

    /// Every room this member is seated in, by name, sorted. The directory
    /// prints it beside the member; nothing else needs it.
    pub fn rooms_of(&self, member: &str) -> Vec<String> {
        let inner = self.lock();
        let mut out: Vec<String> = inner
            .channels
            .iter()
            .filter(|(_, ch)| ch.members.iter().any(|m| m == member))
            .map(|(name, _)| name.clone())
            .collect();
        out.sort();
        out
    }

    /// Seat a member and write the join into the room's record.
    pub fn invite(&self, name: &str, member: &str) -> Result<(), String> {
        {
            let mut inner = self.lock();
            let Some(ch) = inner.channels.get_mut(name) else {
                return Err(format!("no room #{name}"));
            };
            if ch.members.iter().any(|m| m == member) {
                return Err(format!("{member} is already in #{name}"));
            }
            ch.members.push(member.to_string());
            // Late joiners don't get backlog replay: they start "listening" from the current
            // head (seen set to the current seq, so the serial check won't bounce on pre-join history).
            let seq = ch.seq;
            ch.seen.insert(member.to_string(), seq);
            let event = record_membership(ch, member, JOINED);
            self.sync_channel_message(name, &event);
        }
        self.sync_channel_meta(name);
        Ok(())
    }

    /// Unseat a member and write the departure into the room's record.
    ///
    /// The user is removable like anybody else (that is what leaving a room
    /// is); `main` is not, because main's relay path is seated through it.
    pub fn kick(&self, name: &str, member: &str) -> Result<(), String> {
        if member == MAIN_NAME {
            return Err(format!(
                "{member} is a reserved member and cannot be removed from a room"
            ));
        }
        {
            let mut inner = self.lock();
            let Some(ch) = inner.channels.get_mut(name) else {
                return Err(format!("no room #{name}"));
            };
            let before = ch.members.len();
            ch.members.retain(|m| m != member);
            if ch.members.len() == before {
                return Err(format!("{member} is not in #{name}"));
            }
            let event = record_membership(ch, member, LEFT);
            self.sync_channel_message(name, &event);
        }
        self.sync_channel_meta(name);
        Ok(())
    }

    /// Remove an instance from all channels on deletion (called by the tool layer on AgentControl delete).
    pub fn remove_member_everywhere(&self, member: &str) {
        let changed = {
            let mut inner = self.lock();
            let mut changed = Vec::new();
            for (name, channel) in &mut inner.channels {
                let before = channel.members.len();
                channel.members.retain(|candidate| candidate != member);
                if channel.members.len() != before {
                    let event = record_membership(channel, member, LEFT);
                    changed.push((name.clone(), event));
                }
            }
            changed
        };
        for (name, event) in changed {
            self.sync_channel_message(&name, &event);
            self.sync_channel_meta(&name);
        }
    }

    /// Post a message. The runtime only does three things: stamping (from is taken by the
    /// caller from the session instance name; the model can't specify it), serial staleness
    /// check, and the budget gate; what to say / whether to resend is entirely up to the model.
    pub fn post(&self, from: &str, name: &str, text: &str) -> Result<PostOutcome, String> {
        let mut inner = self.lock();
        let limits = inner.limits;
        let main_line;
        let outcome = {
            let Some(ch) = inner.channels.get_mut(name) else {
                return Err(format!("no room #{name}"));
            };
            if !ch.members.iter().any(|m| m == from) {
                return Err(format!(
                    "{from} is not a member of #{name} — join the room before speaking in it"
                ));
            }
            // Channel-level cap: team override wins, otherwise registry-level.
            let channel_total = ch.message_limit.unwrap_or(limits.channel_total);
            if ch.frozen {
                return Err(format!(
                    "#{name} is frozen (hit the {channel_total} total message cap); no more posts"
                ));
            }
            // Serial commit check: fall behind → bounce back + increments (the bounced
            // content enters the context, counted as read). Speech only: a roster
            // change is not something a sender had to have read before speaking,
            // so a join must never bounce a post that was already being drafted.
            if ch.mode == ChannelMode::Serial {
                let seen = ch.seen.get(from).copied().unwrap_or(0);
                let missed: Vec<ChannelMessage> = ch
                    .log
                    .iter()
                    .filter(|m| m.seq > seen && m.kind == MessageKind::Said)
                    .cloned()
                    .collect();
                if !missed.is_empty() {
                    ch.seen.insert(from.to_string(), ch.seq);
                    return Ok(PostOutcome::Stale { missed });
                }
            }
            let sent = ch.sent.get(from).copied().unwrap_or(0);
            if from != MAIN_NAME && from != USER_NAME && sent >= limits.per_agent {
                return Err(format!(
                    "your posts in #{name} hit the per-agent cap {} (budget gate)",
                    limits.per_agent
                ));
            }
            if ch.seq >= channel_total {
                ch.frozen = true;
                inner.main_mail.push(format!(
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
                kind: MessageKind::Said,
            };
            ch.log.push(msg.clone());
            self.sync_channel_message(name, &msg);
            ch.seen.insert(from.to_string(), ch.seq);
            *ch.sent.entry(from.to_string()).or_insert(0) += 1;
            let deliveries: Vec<(String, ChannelMessage)> = ch
                .members
                .iter()
                .filter(|m| {
                    m.as_str() != from && m.as_str() != MAIN_NAME && m.as_str() != USER_NAME
                })
                .map(|m| (m.clone(), msg.clone()))
                .collect();
            main_line = if from != MAIN_NAME && ch.members.iter().any(|m| m == MAIN_NAME) {
                Some(format_main_line(name, &msg))
            } else {
                None
            };
            PostOutcome::Sent {
                seq: msg.seq,
                deliveries,
            }
        };
        if let Some(line) = main_line {
            inner.main_mail.push(line);
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

    /// Display-row snapshot: (watch_id, detail, tail text of the log).
    pub fn row_snapshot(
        &self,
        name: &str,
    ) -> Option<(Option<crate::watch::WatchId>, String, String)> {
        const TAIL: usize = 50;
        let inner = self.lock();
        let ch = inner.channels.get(name)?;
        // "latest" means the latest thing anybody *said*. A room whose last
        // entry is a join has not gone quiet, and a row reading `latest coder:
        // joined` would say it had.
        let detail = match ch.log.iter().rev().find(|m| m.kind == MessageKind::Said) {
            Some(last) => format!(
                "{} msgs · latest {}: {}",
                ch.seq,
                last.from,
                crate::tool::agent::excerpt(&last.text)
            ),
            None => format!("{} msgs", ch.seq),
        };
        let skipped = ch.log.len().saturating_sub(TAIL);
        let mut lines: Vec<String> = Vec::new();
        if skipped > 0 {
            lines.push(format!("… ({skipped} earlier msgs skipped)"));
        }
        lines.extend(ch.log.iter().skip(skipped).map(|m| match m.kind {
            MessageKind::Said => format!("{}. {}: {}", m.seq, m.from, m.text),
            MessageKind::Membership => format!("· {} {} ·", m.from, m.text),
        }));
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

    pub fn has_main_mail(&self) -> bool {
        !self.lock().main_mail.is_empty()
    }

    /// How much is waiting. The digest debounce watches this rather than the
    /// bare "is there any": a burst is exactly a count that keeps changing, and
    /// the quiet window restarts every time it does.
    pub fn main_mail_len(&self) -> usize {
        self.lock().main_mail.len()
    }

    /// Drain channel messages pending injection into the main agent (batch-injected at turn boundaries).
    pub fn drain_main_mail(&self) -> Vec<String> {
        std::mem::take(&mut self.lock().main_mail)
    }

    /// Land a direct message for the main agent (D98's `SendMessage(to: "main")`).
    ///
    /// It rides the room relays' store because the main agent has one inbox and
    /// one place it is injected from; what tells the two apart is the marker on
    /// the line, which is also what lets a reader attribute it.
    pub fn deliver_to_main(&self, from: &str, text: &str, urgent: bool) {
        let mut inner = self.lock();
        inner.main_mail.push(format_main_message(from, text));
        inner.main_mail_urgent |= urgent;
    }

    /// Take the pending attention request, if any. Reading it clears it: the
    /// bell rings once per message that asked for it.
    pub fn take_main_mail_urgent(&self) -> bool {
        std::mem::take(&mut self.lock().main_mail_urgent)
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

    /// A room's roster is exactly what it was created with — an arbitrary
    /// subset of the team, with nobody seated behind the caller's back (D95).
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
            vec!["a", "b"],
            "neither the user nor main is seated unless asked for"
        );
        assert!(
            !reg.is_member("table", USER_NAME),
            "so this is a room the user is not in"
        );
        reg.invite("table", "c").unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.invite("table", "c").is_err(), "duplicate invite");
        reg.kick("table", "b").unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.kick("table", "b").is_err(), "not present");
        assert!(reg.kick("table", "main").is_err(), "main cannot be removed");
        assert_eq!(reg.list()[0].members, vec!["a", "c"]);
        // The user joins and leaves like anybody else: that is what a
        // membership is, and a member who cannot leave is a fixture.
        reg.invite("table", USER_NAME)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.is_member("table", USER_NAME));
        assert_eq!(reg.rooms_of(USER_NAME), vec!["table"]);
        reg.kick("table", USER_NAME)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.rooms_of(USER_NAME).is_empty());
        reg.remove_member_everywhere("a");
        assert_eq!(reg.list()[0].members, vec!["c"]);
        // Single-room snapshot and full-log accessors. The log is not empty:
        // every one of those roster changes is in it.
        assert!(reg.info("table").unwrap_or_else(|| panic!("has one")).seq > 0);
        assert!(reg.info("nope").is_none());
        assert!(
            reg.log_of("table")
                .iter()
                .all(|m| m.kind == MessageKind::Membership),
            "nobody has said anything yet"
        );
    }

    #[test]
    fn post_fans_out_excluding_sender_and_main() {
        let reg = registry();
        reg.create(
            "t",
            vec![
                MAIN_NAME.into(),
                USER_NAME.into(),
                "a".into(),
                "b".into(),
                "c".into(),
            ],
            ChannelMode::Free,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let (seq, deliveries) = sent(
            reg.post("a", "t", "hello everyone")
                .unwrap_or_else(|e| panic!("{e}")),
        );
        assert_eq!(seq, 1);
        let names: Vec<&str> = deliveries.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(names, vec!["b", "c"], "not delivered to the sender or main");
        assert!(
            deliveries
                .iter()
                .all(|(_, m)| m.from == "a" && m.text == "hello everyone")
        );
        // Main is a member: messages go to main_mail; main's own posts don't.
        assert!(reg.has_main_mail());
        let mail = reg.drain_main_mail();
        assert_eq!(mail, vec!["[#t msg #1] a: hello everyone"]);
        let _ = sent(
            reg.post("main", "t", "quiet")
                .unwrap_or_else(|e| panic!("{e}")),
        );
        assert!(!reg.has_main_mail(), "main's own posts do not flow back");
        // user (a human) is a natural member: can post, main hears it, doesn't consume the per_agent budget.
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
        assert!(reg.drain_main_mail()[0].contains("user: everyone stop"));
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
        assert_eq!(seq, 3, "the join took a place in the room's order too");
    }

    /// A roster change is in the record but is not something anyone had to have
    /// read before speaking: joining a serial room must not bounce every member
    /// who was already drafting.
    #[test]
    fn a_join_never_makes_anybody_stale() {
        let reg = registry();
        reg.create("t", vec!["a".into(), "b".into()], ChannelMode::Serial)
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(reg.post("a", "t", "one").unwrap_or_else(|e| panic!("{e}")));
        reg.mark_seen("b", "t", 1);
        reg.invite("t", "c").unwrap_or_else(|e| panic!("{e}"));
        reg.kick("t", "c").unwrap_or_else(|e| panic!("{e}"));
        // b is up to date on everything *said*; two roster changes since then
        // change nothing about that.
        let (seq, _) = sent(reg.post("b", "t", "two").unwrap_or_else(|e| panic!("{e}")));
        assert_eq!(seq, 4);
        // And the membership entries are not delivered to anybody: only the
        // post is.
        match reg.post("a", "t", "three") {
            Ok(PostOutcome::Stale { missed }) => {
                assert_eq!(missed.len(), 1, "only speech is missed: {missed:?}");
                assert_eq!(missed[0].text, "two");
            }
            other => panic!("a is behind by one message, got {other:?}"),
        }
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
    fn budgets_freeze_channel_and_notify_main_once() {
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
        // b triggers the channel total cap: freeze + main gets one warning.
        let _ = reg.drain_main_mail();
        let err = reg.post("b", "t", "x").unwrap_err();
        assert!(err.contains("frozen"), "{err}");
        assert!(reg.list()[0].frozen);
        let mail = reg.drain_main_mail();
        assert_eq!(mail.len(), 1, "{mail:?}");
        assert!(mail[0].contains("now frozen"));
        // Posting after freeze: rejected, no repeated notification.
        let err = reg.post("b", "t", "y").unwrap_err();
        assert!(err.contains("is frozen"), "{err}");
        assert!(!reg.has_main_mail());
    }

    #[test]
    fn share_hooks_track_create_invite_kick_post() {
        let root = std::env::temp_dir().join(format!("bingo-ch-{}-share", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = crate::share::ShareStore::load_or_create(&root.join("s.json"))
            .unwrap_or_else(|e| panic!("{e}"));
        let reg = ChannelRegistry::new(ChannelLimits::default());
        reg.attach_share(store.clone());

        // create → room metadata (mode + members).
        reg.create("t", vec!["main".into(), "a".into()], ChannelMode::Free)
            .unwrap_or_else(|e| panic!("{e}"));
        let doc = store.snapshot();
        assert_eq!(doc.channels.len(), 1);
        assert_eq!(doc.channels[0].mode, "free");
        assert_eq!(doc.channels[0].members, vec!["main", "a"]);
        assert!(doc.channels[0].messages.is_empty());

        // invite/kick → member updates, and the roster change itself is part of
        // the record that gets persisted (D95).
        reg.invite("t", "b").unwrap_or_else(|e| panic!("{e}"));
        reg.kick("t", "a").unwrap_or_else(|e| panic!("{e}"));
        let doc = store.snapshot();
        assert_eq!(doc.channels[0].members, vec!["main", "b"]);
        assert_eq!(
            doc.channels[0]
                .messages
                .iter()
                .map(|m| (m.from.as_str(), m.text.as_str()))
                .collect::<Vec<_>>(),
            vec![("b", JOINED), ("a", LEFT)]
        );

        // post Sent → message appended after them.
        let (seq, _) = sent(reg.post("b", "t", "hi").unwrap_or_else(|e| panic!("{e}")));
        assert_eq!(seq, 3);
        let doc = store.snapshot();
        assert_eq!(doc.channels[0].messages.len(), 3);
        assert_eq!(doc.channels[0].messages[2].from, "b");
        assert_eq!(doc.channels[0].messages[2].text, "hi");
        // Disk roundtrip: reloading yields identical data, kinds included.
        store.persist();
        let reloaded = crate::share::ShareStore::load_or_create(&root.join("s.json"))
            .unwrap_or_else(|e| panic!("{e}"));
        let doc = reloaded.snapshot();
        assert_eq!(doc.channels[0].messages[2].seq, 3);
        assert_eq!(doc.channels[0].messages[0].kind, MessageKind::Membership);
        assert_eq!(doc.channels[0].messages[2].kind, MessageKind::Said);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A share document written before D95 carries no `kind`, and reading one
    /// must not fail over a field that did not exist: it is all speech.
    #[test]
    fn a_pre_d95_share_document_reads_as_speech() {
        let json = r#"{"seq":1,"from":"a","text":"hello","at":7}"#;
        let msg: ChannelMessage =
            serde_json::from_str(json).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(msg.kind, MessageKind::Said);
        assert_eq!(msg.at, 7);
    }

    #[test]
    fn removing_a_member_everywhere_persists_channel_metadata() {
        let root = std::env::temp_dir().join(format!(
            "bingo-ch-{}-remove-member-share",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let path = root.join("s.json");
        let store = crate::share::ShareStore::load_or_create(&path)
            .unwrap_or_else(|error| panic!("{error}"));
        let reg = ChannelRegistry::new(ChannelLimits::default());
        reg.attach_share(store);
        reg.create(
            "t",
            vec!["main".into(), "a".into(), "b".into()],
            ChannelMode::Free,
        )
        .unwrap_or_else(|error| panic!("{error}"));

        reg.remove_member_everywhere("a");

        let reloaded = crate::share::ShareStore::load_or_create(&path)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(reloaded.snapshot().channels[0].members, vec!["main", "b"]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn attaching_new_share_store_preserves_destination_history() {
        let root =
            std::env::temp_dir().join(format!("bingo-ch-{}-share-rebind", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let reg = ChannelRegistry::new(ChannelLimits::default());
        reg.create("t", vec!["a".into()], ChannelMode::Free)
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(
            reg.post("a", "t", "source session")
                .unwrap_or_else(|e| panic!("{e}")),
        );
        let store = crate::share::ShareStore::load_or_create(&root.join("s.json"))
            .unwrap_or_else(|e| panic!("{e}"));
        store.upsert_channel_meta("t", ChannelMode::Free, vec!["main".into(), "a".into()]);
        store.append_channel_message(
            "t",
            ChannelMessage {
                seq: 10,
                from: "a".into(),
                text: "destination session".into(),
                at: 1,
                kind: MessageKind::Said,
            },
        );

        reg.align_with_share(&store);
        reg.attach_share(store.clone());
        let _ = sent(
            reg.post("a", "t", "after rebind")
                .unwrap_or_else(|e| panic!("{e}")),
        );

        let doc = store.snapshot();
        assert_eq!(doc.channels.len(), 1);
        assert_eq!(doc.channels[0].members, vec!["a"]);
        assert_eq!(doc.channels[0].messages.len(), 2);
        assert_eq!(doc.channels[0].messages[0].text, "destination session");
        assert_eq!(doc.channels[0].messages[1].seq, 11);
        assert_eq!(doc.channels[0].messages[1].text, "after rebind");
        let _ = std::fs::remove_dir_all(root);
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
