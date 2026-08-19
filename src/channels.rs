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
//! 3. Wake-up follows delivery, unconditionally (v7): a non-empty inbox wakes
//!    its holder, a running one absorbs at its next tool boundary, and an empty
//!    one never wakes, so a quiet room costs nothing. What a member *owes* is
//!    the `@`'s business and the prompt's — silence after waking is a zero-cost
//!    absorbing state, which is why reading can be free of judgement;
//! 4. Sender stamping by the runtime (from comes from the session instance name and
//!    cannot be forged) + a budget gate (freezes the channel on overrun and notifies
//!    the main agent instead of silently burning money).
//!
//! This module is pure state (no watch/agents dependencies); delivery wake-ups and
//! display-row updates are orchestrated by the tool layer (`tool::channel`). The main
//! agent's member name is always `main`.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use crate::app::answer::Answer;
use crate::app::controller::Control;

use serde::{Deserialize, Serialize};

/// Reserved member name of the main agent in channels.
pub const MAIN_NAME: &str = "main";
/// Reserved member name of the user (a human) in channels: speaks under this
/// identity in a room and is shown as the sender of their own messages. Exempt
/// from budgets, like `main`. Since D95 the user is an *ordinary* member in
/// every other respect: not seated automatically, and free to leave a room they
/// joined — a roster the user could never leave was a roster, not a membership.
pub const USER_NAME: &str = "user";
/// Reserved mention token that reaches every member of a room at once
/// (`@all`); never handed out as an instance name, or the token would be
/// ambiguous.
pub const ALL_NAME: &str = "all";

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

/// Whether a room post says a name at somebody (D99).
///
/// Case-insensitive with word boundaries on both sides of the token: an agent
/// writing `@User`, `@USER,` or `(@user)` is addressing the person and reaches
/// them, while `@username` and `mail@user.example` are not and do not. The
/// literal-`@user` test this replaced meant a badge that depended on the model
/// getting the case right.
pub fn names(text: &str, name: &str) -> bool {
    let haystack = text.to_lowercase();
    let needle = format!("@{}", name.to_lowercase());
    let mut from = 0;
    while let Some(offset) = haystack.get(from..).and_then(|rest| rest.find(&needle)) {
        let start = from + offset;
        let end = start + needle.len();
        let before_ok = haystack[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !part_of_a_word(c));
        let after_ok = haystack[end..]
            .chars()
            .next()
            .is_none_or(|c| !part_of_a_word(c));
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

/// What counts as the inside of a name for mention purposes — one predicate
/// shared by [`names`] and [`mention_tokens`], so a text that names a member
/// by one reading always names them by the other.
fn part_of_a_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// Every `@token` in a post, lowercased and deduplicated, with [`names`]'s
/// word boundaries: the sender's mention list, which the room resolves
/// against its roster (and `@all`).
pub fn mention_tokens(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut tokens: Vec<String> = Vec::new();
    let mut prev: Option<char> = None;
    for (i, c) in lower.char_indices() {
        if c == '@' && prev.is_none_or(|p| !part_of_a_word(p)) {
            let token: String = lower[i + c.len_utf8()..]
                .chars()
                .take_while(|c| part_of_a_word(*c))
                .collect();
            if !token.is_empty() && !tokens.contains(&token) {
                tokens.push(token);
            }
        }
        prev = Some(c);
    }
    tokens
}

/// Budgets: freeze on overrun (read from settings.experimental; defaults 500/50).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// One member's copy of a committed post, with whether the post named them.
#[derive(Debug)]
pub struct RoomDelivery {
    pub member: String,
    pub msg: ChannelMessage,
    /// `@member` or `@all` appeared in the text — the needs-you-now bit (v6):
    /// a mentioned copy wakes its holder at once, an unmentioned one keeps to
    /// the batch clock.
    pub mentioned: bool,
}

/// Result of post.
#[derive(Debug)]
pub enum PostOutcome {
    /// Committed: deliver to these members (excluding the sender and main — main goes through main_mail).
    Sent {
        seq: u64,
        deliveries: Vec<RoomDelivery>,
        /// `@tokens` that resolved to nobody in this room (not a member, not
        /// `all`): surfaced to the sender in the tool result rather than
        /// failing the post, so a typo is caught in the same turn.
        unknown_mentions: Vec<String>,
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

/// One `@`, and whether it has been answered (v7 batch 3).
///
/// The `@` is the room's only obligation (R1), and until this it was the only
/// obligation nothing recorded: a direct message has carried a delivery record
/// since D44, while a mention in a room ran bare. So a member that read a
/// question and said nothing was indistinguishable from one that had not read it
/// yet, from one still working on the answer, and from one whose turn had died —
/// four situations, one appearance, and only one of them fine (D124 was the
/// fourth, and it took a screenshot to find).
///
/// Opened by the `@` and closed by the named member's next post to the same
/// room. "Substantive" is not judged: **speaking is the answer**, because the
/// note already says an acknowledgement is not one (R2) and a runtime that
/// second-guessed the wording would be making the judgement call v7 exists to
/// remove.
#[derive(Debug, Clone)]
pub struct Mention {
    /// The message that carried the `@`.
    pub seq: u64,
    /// Who spent it.
    pub from: String,
    /// Who was named — [`ALL_NAME`] for `@all`, which the room owes *one*
    /// covered answer rather than one answer each (R4).
    pub to: String,
    /// When it was posted, in the unix seconds every `at` here is measured in.
    pub at: u64,
    /// The post that closed it, if one has.
    pub answered: Option<u64>,
}

impl Mention {
    /// Whether this member's next post settles it: the one named, or anybody
    /// but the asker when the room at large was.
    fn settled_by(&self, speaker: &str) -> bool {
        if self.to == ALL_NAME {
            speaker != self.from
        } else {
            self.to == speaker
        }
    }
}

/// What a member's row can say about one room: how far it has read, and what it
/// still owes. Both halves already existed and neither was ever shown — the
/// cursor had exactly one reader (the serial staleness check) and the mention
/// had none.
#[derive(Debug, Clone)]
pub struct MemberStanding {
    pub room: String,
    /// Highest sequence this member has taken into a turn.
    pub read_to: u64,
    /// The oldest `@` it has not answered.
    pub owes: Option<Mention>,
}

struct Channel {
    members: Vec<String>,
    mode: ChannelMode,
    seq: u64,
    log: Vec<ChannelMessage>,
    /// Every `@` posted here, open and closed (v7 batch 3). Kept whole rather
    /// than pruned on close: the closed ones are the record of who answered
    /// what, which is the same question the open ones ask.
    mentions: Vec<Mention>,
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

/// One room as a reader sees it: everything a surface draws, and nothing the
/// state machine keeps for itself.
///
/// Held behind an `Arc` in [`ChannelView`] so republishing one room's change
/// does not copy every other room's log.
#[derive(Debug)]
struct RoomView {
    members: Vec<String>,
    mode: ChannelMode,
    seq: u64,
    frozen: bool,
    log: Vec<ChannelMessage>,
    mentions: Vec<Mention>,
    seen: HashMap<String, u64>,
    watch_id: Option<crate::watch::WatchId>,
}

/// The replacement snapshot the registry publishes after every change.
///
/// Rooms are drawn every frame — the roster, the room page, the background
/// dialog — and a round trip to the actor per frame would put the frame rate in
/// front of the session's own work.
#[derive(Debug, Default)]
pub struct ChannelView {
    rooms: std::collections::BTreeMap<String, Arc<RoomView>>,
    /// How much is waiting in the main agent's inbox. The mail itself is a byte
    /// contract with the model and is taken by a question, never read from here.
    main_mail: usize,
    /// Whether a share document is attached. Read by the tests that assert the
    /// hooks fire; nothing on the screen asks.
    #[cfg_attr(not(test), allow(dead_code))]
    shared: bool,
}

/// One room as the application event layer names it: everything a
/// `RoomResource` carries except the server-minted identifier.
pub(crate) struct RoomFacts {
    pub name: String,
    pub mode: ChannelMode,
    pub members: Vec<String>,
    pub message_count: u64,
    pub last_seq: u64,
}

impl ChannelRegistry {
    /// What every room stands at, for the actor to turn into events.
    pub(crate) fn facts(&self) -> Vec<RoomFacts> {
        let mut out: Vec<RoomFacts> = self
            .inner
            .channels
            .iter()
            .map(|(name, ch)| RoomFacts {
                name: name.clone(),
                mode: ch.mode,
                members: ch.members.clone(),
                message_count: ch.log.len() as u64,
                last_seq: ch.seq,
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Everything one room recorded after `seq`, in log order.
    ///
    /// The actor turns these into conversation items, and the diff is what makes
    /// one path serve both cases: a post lands one entry behind the last one the
    /// actor was told about, and a resumed session's replayed log lands its whole
    /// history behind seq zero (B4's sidecar).
    pub(crate) fn since(&self, room: &str, seq: u64) -> Vec<ChannelMessage> {
        self.inner
            .channels
            .get(room)
            .map(|ch| ch.log.iter().filter(|msg| msg.seq > seq).cloned().collect())
            .unwrap_or_default()
    }

    /// Every open mention in every room, as `(room, mention)`.
    pub(crate) fn open_mentions(&self) -> Vec<(String, Mention)> {
        let mut out: Vec<(String, Mention)> = self
            .inner
            .channels
            .iter()
            .flat_map(|(name, ch)| {
                ch.mentions
                    .iter()
                    .filter(|owed| owed.answered.is_none())
                    .map(move |owed| (name.clone(), owed.clone()))
            })
            .collect();
        out.sort_by(|a, b| (a.0.as_str(), a.1.seq).cmp(&(b.0.as_str(), b.1.seq)));
        out
    }
}

/// An answer held back until the view carrying its effect has been published.
type Answered = Box<dyn FnOnce()>;

fn answered<T: 'static>(reply: oneshot::Sender<T>, value: T) -> Option<Answered> {
    Some(Box::new(move || {
        let _ = reply.send(value);
    }))
}

/// Which rooms a change touched, so republishing can keep the rest.
enum Touched {
    /// The mail changed, or nothing did: every room's snapshot still stands.
    NoRoom,
    One(String),
    Every,
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
    /// What the transcript owes the screen about that inbox (D106), kept beside
    /// it rather than inside it: [`Inner::main_mail`] is a byte contract with
    /// the model — the marker on each line is what
    /// [`crate::app::projection::line_source`] reads back — and a renderer that had
    /// to un-format it would be reading the model's mail over its shoulder.
    ///
    /// One entry per direct message that landed for `main`, drained by whatever
    /// draws the flow. Room relays are deliberately absent: see
    /// [`ChannelRegistry::drain_main_arrivals`].
    main_arrivals: VecDeque<MainArrival>,
    limits: ChannelLimits,
}

impl Inner {}

/// A message that arrived for the main agent, as the transcript needs it: who
/// sent it, what they said, and the one-line preview they wrote for it. The
/// flow prints one line of this and keeps the body for the `ctrl+o` transcript.
///
/// **The summary rides the mirror and not the mail** (D108). CC's field travels
/// in the envelope of its *teammate* runtime, as a `summary="…"` attribute its
/// renderer parses back out of the recipient's own prompt text
/// (`utils/teammateMailbox.ts:386`); its **subagent** runtime — the one v4
/// replicates — passes only the message and drops the summary before the
/// recipient sees it (`SendMessageTool.ts:810-814`). bingo matches the
/// subagent: [`Inner::main_mail`] is byte-identical to what it was, so the
/// model reads exactly what was said to it and the preview is a fact about the
/// screen alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainArrival {
    pub from: String,
    pub text: String,
    /// The sender's own 5-10 word preview, when it wrote one. `None` is the
    /// ordinary case and the renderer's fallback — the message's first line —
    /// is CC's own (`SendMessageTool.ts:765`).
    pub summary: Option<String>,
}

/// Cap on the undrained arrivals mirror. Nothing drains it outside the TUI —
/// `-p` runs deliver mail with no flow to print it into — so it is bounded and
/// drops the oldest, exactly as the feed buffers in [`crate::watch`] do.
const MAX_MAIN_ARRIVALS: usize = 256;

/// The channel registry's state, owned by the session actor.
///
/// Nothing here is behind a lock, because nothing else can reach it: every
/// change arrives as a [`ChannelMsg`] on the actor's one inbox and is applied by
/// the actor's one thread.
pub struct ChannelRegistry {
    inner: Inner,
    /// Messages main was handed since the actor last looked. Drained by the
    /// actor, which turns each into an item in main's conversation at the moment
    /// it arrived (D135) — the room half of `AgentRegistry::drain_delivered`.
    delivered: Vec<MainDelivery>,
    /// Share persistence (Option semantics: behavior is unchanged when not attached; once attached, create/invite/kick/post sync snapshots).
    share: Option<Arc<crate::share::ShareStore>>,
    /// Where the attached document's disk writes happen — never here.
    saver: Option<crate::share::ShareSaver>,
    /// The room sidecar, if this session has one. Append-only, written on its
    /// own thread, and replayed on resume (Amendment #6).
    sidecar: Option<crate::app::roomlog::Writer>,
    view: tokio::sync::watch::Sender<Arc<ChannelView>>,
}

/// What the actor is told about the rooms, and what it is asked.
///
/// Reports carry no reply channel and nobody waits on them; questions carry the
/// channel their answer returns on. The split is `engine::events`', and the
/// reason a value-returning call is a question rather than a read of the
/// published view is that its caller acts on the answer — a post that bounced,
/// a room that already exists — and a stale view would let it act on a world
/// that has moved.
pub enum ChannelMsg {
    /// Start writing this session's rooms to a sidecar, and write down what is
    /// already here.
    AttachSidecar(std::path::PathBuf),
    /// Rebuild the rooms a sidecar remembers.
    RestoreRooms(Box<crate::app::roomlog::Replay>),
    AttachShare(Arc<crate::share::ShareStore>),
    AlignWithShare(Arc<crate::share::ShareStore>),
    DetachShare,
    Create {
        name: String,
        members: Vec<String>,
        mode: ChannelMode,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SetMessageLimit {
        name: String,
        limit: u64,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SetWatch {
        name: String,
        id: crate::watch::WatchId,
    },
    Invite {
        name: String,
        member: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Kick {
        name: String,
        member: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RemoveMemberEverywhere {
        member: String,
    },
    Post {
        from: String,
        name: String,
        text: String,
        reply: oneshot::Sender<Result<PostOutcome, String>>,
    },
    OpenMention {
        name: String,
        seq: u64,
        to: String,
        reply: oneshot::Sender<Option<Mention>>,
    },
    MarkSeen {
        member: String,
        name: String,
        seq: u64,
    },
    DrainMainMail {
        reply: oneshot::Sender<Vec<String>>,
    },
    DeliverToMain {
        from: String,
        text: String,
        summary: Option<String>,
        urgent: bool,
    },
    DrainMainArrivals {
        reply: oneshot::Sender<Vec<MainArrival>>,
    },
    TakeMainMailUrgent {
        reply: oneshot::Sender<bool>,
    },
    /// Answered once the attached share document has reached the disk. The
    /// tests that assert on the file ask; the session never does.
    #[cfg(test)]
    FlushShare {
        reply: oneshot::Sender<()>,
    },
}

fn format_main_line(channel: &str, msg: &ChannelMessage) -> String {
    format!("[#{channel} msg #{}] {}: {}", msg.seq, msg.from, msg.text)
}

/// Opening of the line a direct message *from an agent* arrives under (D98):
/// `[message from @scout]`, on its own line above the text.
///
/// The shape is [`crate::tool::agent::DM_FROM_USER_MARKER`]'s, with the sender
/// named — the one thing that marker never had to carry, because the human is
/// the only human. A receiver hears from many agents, so this marker names
/// which. [`crate::app::projection::line_source`] is the single parser of the shape.
///
/// D137 gave it its second producer. It was main's alone while main was the
/// only thing an agent could write to; now any instance can be written to by
/// any other, and the receiver has the same question main had — *who is this
/// from* — answered by the same line, parsed by the same parser, filed by the
/// same walk. What the marker means is "an agent wrote to you", not "you are
/// main", which is why it is no longer named for the receiver.
pub const AGENT_MESSAGE_PREFIX: &str = "[message from @";

/// One direct message as it enters an agent's context.
pub fn format_agent_message(from: &str, text: &str) -> String {
    format!("{AGENT_MESSAGE_PREFIX}{from}]\n{text}")
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
/// Re-derive a room's `@` ledger from its log.
///
/// The rules are `post`'s, applied in sequence order: close the debts this line
/// settles, then open the ones it names. Replaying them rather than storing them
/// keeps one authority for what a `@` owes, however the log arrived.
fn rebuild_mentions(channel: &mut Channel) {
    let entries: Vec<ChannelMessage> = channel
        .log
        .iter()
        .filter(|entry| entry.kind == MessageKind::Said)
        .cloned()
        .collect();
    for entry in entries {
        for owed in channel.mentions.iter_mut() {
            if owed.answered.is_none() && owed.seq < entry.seq && owed.settled_by(&entry.from) {
                owed.answered = Some(entry.seq);
            }
        }
        let tokens = mention_tokens(&entry.text);
        let at_all = tokens.iter().any(|token| token == ALL_NAME);
        let named: Vec<String> = if at_all {
            vec![ALL_NAME.to_string()]
        } else {
            channel
                .members
                .iter()
                .filter(|member| {
                    member.as_str() != entry.from
                        && tokens.iter().any(|token| *token == member.to_lowercase())
                })
                .cloned()
                .collect()
        };
        for to in named {
            channel.mentions.push(Mention {
                seq: entry.seq,
                from: entry.from.clone(),
                to,
                at: entry.at,
                answered: None,
            });
        }
    }
}

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

/// Build the registry and the handle everything reaches it by.
pub(crate) fn attach(
    control: mpsc::UnboundedSender<Control>,
    limits: ChannelLimits,
) -> (ChannelRegistry, ChannelHandle) {
    let (view, reader) = tokio::sync::watch::channel(Arc::new(ChannelView::default()));
    let registry = ChannelRegistry {
        delivered: Vec::new(),
        inner: Inner {
            channels: HashMap::new(),
            main_mail: Vec::new(),
            main_mail_urgent: false,
            main_arrivals: VecDeque::new(),
            limits,
        },
        share: None,
        saver: None,
        sidecar: None,
        view,
    };
    let handle = ChannelHandle {
        control,
        view: reader,
    };
    (registry, handle)
}

impl ChannelRegistry {
    /// Apply one message, republish what a reader sees, and only then answer.
    ///
    /// The order of the last two steps is the contract a caller depends on: a
    /// question is answered **after** the view carrying its effect is published,
    /// so a caller that posts and then reads the room can never read the room as
    /// it was before its own post.
    pub(crate) fn handle(&mut self, message: ChannelMsg) {
        let (touched, answer) = self.apply(message);
        self.publish(touched);
        if let Some(answer) = answer {
            answer();
        }
    }

    fn apply(&mut self, message: ChannelMsg) -> (Touched, Option<Answered>) {
        match message {
            ChannelMsg::AttachSidecar(path) => {
                self.attach_sidecar(path);
                (Touched::NoRoom, None)
            }
            ChannelMsg::RestoreRooms(replay) => {
                self.restore(&replay);
                (Touched::Every, None)
            }
            ChannelMsg::AttachShare(store) => {
                self.attach_share(store);
                (Touched::NoRoom, None)
            }
            ChannelMsg::AlignWithShare(store) => {
                self.align_with_share(&store);
                (Touched::Every, None)
            }
            ChannelMsg::DetachShare => {
                self.share = None;
                self.saver = None;
                (Touched::NoRoom, None)
            }
            ChannelMsg::Create {
                name,
                members,
                mode,
                reply,
            } => {
                let created = self.create(&name, members, mode);
                (Touched::Every, answered(reply, created))
            }
            ChannelMsg::SetMessageLimit { name, limit, reply } => {
                let set = self.set_message_limit(&name, limit);
                (Touched::NoRoom, answered(reply, set))
            }
            ChannelMsg::SetWatch { name, id } => {
                if let Some(ch) = self.inner.channels.get_mut(&name) {
                    ch.watch_id = Some(id);
                }
                (Touched::One(name), None)
            }
            ChannelMsg::Invite {
                name,
                member,
                reply,
            } => {
                let invited = self.invite(&name, &member);
                (Touched::One(name), answered(reply, invited))
            }
            ChannelMsg::Kick {
                name,
                member,
                reply,
            } => {
                let kicked = self.kick(&name, &member);
                (Touched::One(name), answered(reply, kicked))
            }
            ChannelMsg::RemoveMemberEverywhere { member } => {
                self.remove_member_everywhere(&member);
                (Touched::Every, None)
            }
            ChannelMsg::Post {
                from,
                name,
                text,
                reply,
            } => {
                let posted = self.post(&from, &name, &text);
                (Touched::One(name), answered(reply, posted))
            }
            ChannelMsg::OpenMention {
                name,
                seq,
                to,
                reply,
            } => {
                let owed = self.open_mention(&name, seq, &to);
                (Touched::NoRoom, answered(reply, owed))
            }
            ChannelMsg::MarkSeen { member, name, seq } => {
                self.mark_seen(&member, &name, seq);
                (Touched::One(name), None)
            }
            ChannelMsg::DrainMainMail { reply } => {
                let mail = std::mem::take(&mut self.inner.main_mail);
                (Touched::NoRoom, answered(reply, mail))
            }
            ChannelMsg::DeliverToMain {
                from,
                text,
                summary,
                urgent,
            } => {
                self.deliver_to_main(&from, &text, summary.as_deref(), urgent);
                (Touched::NoRoom, None)
            }
            ChannelMsg::DrainMainArrivals { reply } => {
                let arrivals = self.inner.main_arrivals.drain(..).collect();
                (Touched::NoRoom, answered(reply, arrivals))
            }
            ChannelMsg::TakeMainMailUrgent { reply } => {
                let urgent = std::mem::take(&mut self.inner.main_mail_urgent);
                (Touched::NoRoom, answered(reply, urgent))
            }
            #[cfg(test)]
            ChannelMsg::FlushShare { reply } => {
                match &self.saver {
                    // Handed over, not waited on: the writer answers when the
                    // bytes are down.
                    Some(saver) => saver.flush(reply),
                    None => {
                        let _ = reply.send(());
                    }
                }
                (Touched::NoRoom, None)
            }
        }
    }

    /// Republish the reader's snapshot, keeping the `Arc` of every room the
    /// change did not touch: a post in one room must not copy the logs of all
    /// the others.
    fn publish(&self, touched: Touched) {
        let previous = self.view.borrow().clone();
        let mut rooms = std::collections::BTreeMap::new();
        for (name, ch) in &self.inner.channels {
            let unchanged = match &touched {
                Touched::NoRoom => true,
                Touched::One(one) => one != name,
                Touched::Every => false,
            };
            let reuse = unchanged
                .then(|| previous.rooms.get(name).cloned())
                .flatten();
            rooms.insert(
                name.clone(),
                reuse.unwrap_or_else(|| {
                    Arc::new(RoomView {
                        members: ch.members.clone(),
                        mode: ch.mode,
                        seq: ch.seq,
                        frozen: ch.frozen,
                        log: ch.log.clone(),
                        mentions: ch.mentions.clone(),
                        seen: ch.seen.clone(),
                        watch_id: ch.watch_id,
                    })
                }),
            );
        }
        let _ = self.view.send(Arc::new(ChannelView {
            rooms,
            main_mail: self.inner.main_mail.len(),
            shared: self.share.is_some(),
        }));
    }

    /// Replace share persistence and ensure existing channels can accept future messages.
    fn attach_share(&mut self, store: Arc<crate::share::ShareStore>) {
        for (name, channel) in &self.inner.channels {
            store.upsert_channel_meta(name, channel.mode, channel.members.clone());
        }
        self.saver = Some(crate::share::ShareSaver::spawn(store.clone()));
        self.share = Some(store);
        self.save_share();
    }

    fn align_with_share(&mut self, store: &crate::share::ShareStore) {
        let doc = store.snapshot();
        for saved in doc.channels {
            let Some(channel) = self.inner.channels.get_mut(&saved.name) else {
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

    // -- the room sidecar ---------------------------------------------------

    /// Start writing this session's rooms down, and write what is already here.
    fn attach_sidecar(&mut self, path: std::path::PathBuf) {
        match crate::app::roomlog::Writer::open(path) {
            Ok(writer) => {
                self.sidecar = Some(writer);
                let names: Vec<String> = self.inner.channels.keys().cloned().collect();
                for name in names {
                    self.log_room(&name);
                    let entries: Vec<ChannelMessage> = self
                        .inner
                        .channels
                        .get(&name)
                        .map(|ch| ch.log.clone())
                        .unwrap_or_default();
                    for entry in entries {
                        self.log_post(&name, &entry);
                    }
                }
            }
            // A sidecar is an enhancement, not a contract: a session whose rooms
            // cannot be written down still has rooms.
            Err(_) => self.sidecar = None,
        }
    }

    fn log_room(&self, name: &str) {
        let Some(sidecar) = &self.sidecar else {
            return;
        };
        let Some(ch) = self.inner.channels.get(name) else {
            return;
        };
        sidecar.append(vec![crate::app::roomlog::Record::Room {
            room: name.to_string(),
            mode: ch.mode.label().to_string(),
            members: ch.members.clone(),
            frozen: ch.frozen,
            message_limit: ch.message_limit,
        }]);
    }

    fn log_post(&self, name: &str, message: &ChannelMessage) {
        let Some(sidecar) = &self.sidecar else {
            return;
        };
        sidecar.append(vec![crate::app::roomlog::Record::Post {
            room: name.to_string(),
            seq: message.seq,
            from: message.from.clone(),
            text: message.text.clone(),
            at_unix: message.at,
            said: message.kind == MessageKind::Said,
        }]);
    }

    fn log_member(&self, name: &str, member: &str) {
        let Some(sidecar) = &self.sidecar else {
            return;
        };
        let Some(ch) = self.inner.channels.get(name) else {
            return;
        };
        sidecar.append(vec![crate::app::roomlog::Record::Member {
            room: name.to_string(),
            member: member.to_string(),
            seen: ch.seen.get(member).copied().unwrap_or(0),
            sent: ch.sent.get(member).copied().unwrap_or(0),
        }]);
    }

    /// Write down how far the *user* has read one room.
    pub(crate) fn log_read(&self, name: &str, seq: u64) {
        let Some(sidecar) = &self.sidecar else {
            return;
        };
        sidecar.append(vec![crate::app::roomlog::Record::Read {
            room: name.to_string(),
            seq,
        }]);
    }

    /// Rebuild the rooms a sidecar remembers.
    ///
    /// Only rooms that are not already here: a resume replays into a registry
    /// the team may have already repopulated, and the live room is the one that
    /// is running. Mentions are re-derived rather than replayed, so what a `@`
    /// owes has one authority however the log got here.
    fn restore(&mut self, replay: &crate::app::roomlog::Replay) {
        for state in &replay.rooms {
            if self.inner.channels.contains_key(&state.name) {
                continue;
            }
            let mut ch = Channel {
                members: state.members.clone(),
                mode: ChannelMode::parse(&state.mode).unwrap_or(ChannelMode::Free),
                seq: state.log.iter().map(|entry| entry.seq).max().unwrap_or(0),
                log: state
                    .log
                    .iter()
                    .map(|entry| ChannelMessage {
                        seq: entry.seq,
                        from: entry.from.clone(),
                        text: entry.text.clone(),
                        at: entry.at_unix,
                        kind: if entry.said {
                            MessageKind::Said
                        } else {
                            MessageKind::Membership
                        },
                    })
                    .collect(),
                mentions: Vec::new(),
                seen: state
                    .members_seen
                    .iter()
                    .map(|(member, seen, _)| (member.clone(), *seen))
                    .collect(),
                sent: state
                    .members_seen
                    .iter()
                    .map(|(member, _, sent)| (member.clone(), *sent))
                    .collect(),
                frozen: state.frozen,
                message_limit: state.message_limit,
                watch_id: None,
            };
            rebuild_mentions(&mut ch);
            self.inner.channels.insert(state.name.clone(), ch);
        }
    }

    /// Ask the attached document to reach the disk. Never writes it here: the
    /// actor is the one ordering point and a file is not something it may wait on.
    fn save_share(&self) {
        if let Some(saver) = &self.saver {
            saver.save();
        }
    }

    /// Write a channel's latest metadata (mode + members) into the share document (no-op without a store).
    fn sync_channel_meta(&self, name: &str) {
        self.log_room(name);
        let Some(store) = self.share.as_ref() else {
            return;
        };
        let Some(ch) = self.inner.channels.get(name) else {
            return;
        };
        store.upsert_channel_meta(name, ch.mode, ch.members.clone());
        self.save_share();
    }

    /// Append a landed channel message to the share document (no-op without a store).
    fn sync_channel_message(&self, name: &str, msg: &ChannelMessage) {
        self.log_post(name, msg);
        let Some(store) = self.share.as_ref() else {
            return;
        };
        store.append_channel_message(name, msg.clone());
        self.save_share();
    }

    /// Create a room with exactly these members — an arbitrary subset of the
    /// team, which need contain neither the user nor the main agent (D95).
    ///
    /// Nobody is seated behind the caller's back. Auto-seating `main` and
    /// `user` made every room the user's room by construction, which is the one
    /// thing the room model says a room is not: agents form rooms among
    /// themselves, and the user reaches those by finding them in the
    /// background dialog's Rooms section and joining. Who *should* be seated on creation is policy, and policy
    /// lives at the tool layer, where the creator's identity is known
    /// ([`crate::tool::channel`] stamps it) and where member existence and
    /// depth are already validated.
    fn create(
        &mut self,
        name: &str,
        members: Vec<String>,
        mode: ChannelMode,
    ) -> Result<(), String> {
        let name = name.trim_start_matches('#');
        if name.is_empty() {
            return Err("room name must not be empty".to_string());
        }
        {
            if self.inner.channels.contains_key(name) {
                return Err(format!("room #{name} already exists"));
            }
            let mut all: Vec<String> = Vec::new();
            for m in members {
                if !m.is_empty() && !all.contains(&m) {
                    all.push(m);
                }
            }
            self.inner.channels.insert(
                name.to_string(),
                Channel {
                    members: all,
                    mode,
                    seq: 0,
                    log: Vec::new(),
                    mentions: Vec::new(),
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
    fn set_message_limit(&mut self, name: &str, limit: u64) -> Result<(), String> {
        if limit == 0 {
            return Err("messageLimit must be a positive integer".to_string());
        }
        let Some(ch) = self.inner.channels.get_mut(name) else {
            return Err(format!("no channel #{name}"));
        };
        ch.message_limit = Some(limit);
        Ok(())
    }

    /// Seat a member and write the join into the room's record.
    fn invite(&mut self, name: &str, member: &str) -> Result<(), String> {
        {
            let Some(ch) = self.inner.channels.get_mut(name) else {
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
    fn kick(&mut self, name: &str, member: &str) -> Result<(), String> {
        if member == MAIN_NAME {
            return Err(format!(
                "{member} is a reserved member and cannot be removed from a room"
            ));
        }
        {
            let Some(ch) = self.inner.channels.get_mut(name) else {
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
    fn remove_member_everywhere(&mut self, member: &str) {
        let changed = {
            let mut changed = Vec::new();
            for (name, channel) in &mut self.inner.channels {
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
    fn post(&mut self, from: &str, name: &str, text: &str) -> Result<PostOutcome, String> {
        let limits = self.inner.limits;
        let main_line;
        let mut frozen_notice = None;
        let landed;
        let outcome = {
            let Some(ch) = self.inner.channels.get_mut(name) else {
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
                // A runtime warning, not a room relay: lands directly, gate or
                // no gate (v6) — a frozen budget is main's business at once.
                frozen_notice = Some(format!(
                    "⚠ channel #{name} hit the {channel_total} total message cap and is now frozen (further posts will be rejected)",
                ));
            }
            if let Some(notice) = frozen_notice {
                self.inner.main_mail.push(notice);
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
            landed = Some(msg.clone());
            ch.seen.insert(from.to_string(), ch.seq);
            *ch.sent.entry(from.to_string()).or_insert(0) += 1;
            // Settle before opening: speaking is what answers an `@`, and this
            // post is speech. Closing first also keeps a member that answers and
            // asks in one breath from settling its own new question.
            for owed in ch.mentions.iter_mut() {
                if owed.answered.is_none() && owed.seq < msg.seq && owed.settled_by(from) {
                    owed.answered = Some(msg.seq);
                }
            }
            let tokens = mention_tokens(text);
            let at_all = tokens.iter().any(|t| t == ALL_NAME);
            let main_mentioned = at_all || tokens.iter().any(|t| t == MAIN_NAME);
            // One debt per `@`, and `@all` is one debt against the room rather
            // than one per member: R4 owes it a single covered answer.
            let named: Vec<String> = if at_all {
                vec![ALL_NAME.to_string()]
            } else {
                ch.members
                    .iter()
                    .filter(|m| m.as_str() != from && tokens.iter().any(|t| *t == m.to_lowercase()))
                    .cloned()
                    .collect()
            };
            for to in named {
                ch.mentions.push(Mention {
                    seq: msg.seq,
                    from: from.to_string(),
                    to,
                    at: msg.at,
                    answered: None,
                });
            }
            let deliveries: Vec<RoomDelivery> = ch
                .members
                .iter()
                .filter(|m| {
                    m.as_str() != from && m.as_str() != MAIN_NAME && m.as_str() != USER_NAME
                })
                .map(|m| RoomDelivery {
                    member: m.clone(),
                    msg: msg.clone(),
                    mentioned: at_all || tokens.iter().any(|t| *t == m.to_lowercase()),
                })
                .collect();
            let unknown_mentions: Vec<String> = tokens
                .into_iter()
                .filter(|t| t != ALL_NAME && !ch.members.iter().any(|m| m.to_lowercase() == *t))
                .collect();
            main_line = if from != MAIN_NAME && ch.members.iter().any(|m| m == MAIN_NAME) {
                Some((format_main_line(name, &msg), main_mentioned))
            } else {
                None
            };
            PostOutcome::Sent {
                seq: msg.seq,
                deliveries,
                unknown_mentions,
            }
        };
        // v7: main is a member, and a member's inbox is not gated — the line
        // goes to `main_mail` the moment it is posted, exactly as it goes to
        // everyone else's inbox. What main *owes* is the `@`'s business
        // (`MAIN_CHANNEL_NOTE`), never the delivery's.
        if let Some(msg) = landed {
            self.sync_channel_message(name, &msg);
            self.log_member(name, from);
        }
        if let Some((line, _mentioned)) = main_line {
            self.inner.main_mail.push(line);
        }
        Ok(outcome)
    }

    /// One `@`, if it is still open — the chase's whole question, asked of the
    /// same record the roster reads, so a nudge and a row can never disagree.
    fn open_mention(&self, name: &str, seq: u64, to: &str) -> Option<Mention> {
        self.inner
            .channels
            .get(name)?
            .mentions
            .iter()
            .find(|m| m.seq == seq && m.to == to && m.answered.is_none())
            .cloned()
    }
    /// Mark the member's inbox as digested up to seq (its running turn was
    /// injected with channel messages up to seq).
    fn mark_seen(&mut self, member: &str, name: &str, seq: u64) {
        let mut moved = false;
        if let Some(ch) = self.inner.channels.get_mut(name) {
            let cursor = ch.seen.entry(member.to_string()).or_insert(0);
            if *cursor < seq {
                *cursor = seq;
                moved = true;
            }
        }
        if moved {
            // A resumed member that had forgotten how far it had read would
            // bounce on the whole replayed log, which is exactly the flood the
            // serial rule exists to prevent.
            self.log_member(name, member);
        }
    }

    /// Land a direct message for the main agent (D98's `SendMessage(to: "main")`).
    fn deliver_to_main(&mut self, from: &str, text: &str, summary: Option<&str>, urgent: bool) {
        self.inner.main_mail.push(format_agent_message(from, text));
        self.inner.main_mail_urgent |= urgent;
        self.inner.main_arrivals.push_back(MainArrival {
            from: from.to_string(),
            text: text.to_string(),
            summary: summary.map(str::to_string),
        });
        while self.inner.main_arrivals.len() > MAX_MAIN_ARRIVALS {
            self.inner.main_arrivals.pop_front();
        }
        self.delivered.push(MainDelivery {
            from: from.to_string(),
            text: text.to_string(),
        });
    }

    /// The messages main was handed since the last look.
    pub(crate) fn drain_delivered(&mut self) -> Vec<MainDelivery> {
        std::mem::take(&mut self.delivered)
    }

    /// How much is waiting in main's inbox, and whether any of it asked for the
    /// attention channel.
    ///
    /// The urgent flag is **read, not taken**: the terminal front end still takes
    /// it to ring its bell, and two takers would mean whichever looked first
    /// silenced the other. The core's own reading is the debounce's, and the
    /// batch latch is what stops it acting on the same flag twice.
    pub(crate) fn main_mail_stand(&self) -> (usize, bool) {
        (self.inner.main_mail.len(), self.inner.main_mail_urgent)
    }
}

/// One message main was handed, as the actor reads it back.
pub(crate) struct MainDelivery {
    pub from: String,
    pub text: String,
}

/// How everything outside the actor reaches the rooms.
///
/// The method names and their meanings are the ones the registry has always had.
/// What changed is who owns the state and how a call gets there: a report is an
/// ordered send nobody waits on, a question is a send plus the answer coming
/// back — taken with `.await` or, at the synchronous seams the terminal front
/// end still has, with [`Answer::now`] — and a listing is a borrow of the
/// published snapshot.
#[derive(Clone)]
pub struct ChannelHandle {
    control: mpsc::UnboundedSender<Control>,
    view: tokio::sync::watch::Receiver<Arc<ChannelView>>,
}

impl ChannelHandle {
    fn report(&self, message: ChannelMsg) {
        let _ = self.control.send(Control::Channels(message));
    }

    /// Ask the actor something. `gone` is the answer if the session has ended.
    fn ask<T>(&self, build: impl FnOnce(oneshot::Sender<T>) -> ChannelMsg, gone: T) -> Answer<T> {
        let (reply, answer) = oneshot::channel();
        // A failed send drops `reply`, which is how `Answer` learns the session
        // is over — so the error needs no separate path.
        let _ = self.control.send(Control::Channels(build(reply)));
        Answer::new(answer, gone)
    }

    fn room<T>(&self, name: &str, read: impl FnOnce(&RoomView) -> T) -> Option<T> {
        self.view.borrow().rooms.get(name).map(|room| read(room))
    }

    /// Replace share persistence and ensure existing channels can accept future messages.
    /// Start writing this session's rooms down (Amendment #6).
    pub fn attach_sidecar(&self, path: std::path::PathBuf) {
        self.report(ChannelMsg::AttachSidecar(path));
    }

    /// Rebuild the rooms a sidecar remembers.
    pub fn restore_rooms(&self, replay: crate::app::roomlog::Replay) {
        self.report(ChannelMsg::RestoreRooms(Box::new(replay)));
    }

    pub fn attach_share(&self, store: Arc<crate::share::ShareStore>) {
        self.report(ChannelMsg::AttachShare(store));
    }

    pub fn align_with_share(&self, store: Arc<crate::share::ShareStore>) {
        self.report(ChannelMsg::AlignWithShare(store));
    }

    pub fn detach_share(&self) {
        self.report(ChannelMsg::DetachShare);
    }

    #[cfg(test)]
    pub fn has_share(&self) -> bool {
        self.view.borrow().shared
    }

    /// Create a room with exactly these members (D95).
    pub fn create(
        &self,
        name: &str,
        members: Vec<String>,
        mode: ChannelMode,
    ) -> Answer<Result<(), String>> {
        self.ask(
            |reply| ChannelMsg::Create {
                name: name.to_string(),
                members,
                mode,
                reply,
            },
            Err(SESSION_ENDED.to_string()),
        )
    }

    /// Channel-level total message cap override (D31 team.json channel.messageLimit).
    pub fn set_message_limit(&self, name: &str, limit: u64) -> Answer<Result<(), String>> {
        self.ask(
            |reply| ChannelMsg::SetMessageLimit {
                name: name.to_string(),
                limit,
                reply,
            },
            Err(SESSION_ENDED.to_string()),
        )
    }

    pub fn set_watch(&self, name: &str, id: crate::watch::WatchId) {
        self.report(ChannelMsg::SetWatch {
            name: name.to_string(),
            id,
        });
    }

    /// Is this name currently seated in the room? The one question the display
    /// side asks: a room the user is in is a conversation of theirs (the
    /// composer's `#room` send, the accounting store), a room they are not in
    /// is a place they can read.
    pub fn is_member(&self, name: &str, member: &str) -> bool {
        self.room(name, |room| room.members.iter().any(|m| m == member))
            .unwrap_or(false)
    }

    /// Seat a member and write the join into the room's record.
    pub fn invite(&self, name: &str, member: &str) -> Answer<Result<(), String>> {
        self.ask(
            |reply| ChannelMsg::Invite {
                name: name.to_string(),
                member: member.to_string(),
                reply,
            },
            Err(SESSION_ENDED.to_string()),
        )
    }

    /// Unseat a member and write the departure into the room's record.
    pub fn kick(&self, name: &str, member: &str) -> Answer<Result<(), String>> {
        self.ask(
            |reply| ChannelMsg::Kick {
                name: name.to_string(),
                member: member.to_string(),
                reply,
            },
            Err(SESSION_ENDED.to_string()),
        )
    }

    /// Remove an instance from all channels on deletion (called by the tool layer on AgentControl delete).
    pub fn remove_member_everywhere(&self, member: &str) {
        self.report(ChannelMsg::RemoveMemberEverywhere {
            member: member.to_string(),
        });
    }

    /// Post a message. The runtime only does three things: stamping, the serial
    /// staleness check, and the budget gate.
    pub fn post(&self, from: &str, name: &str, text: &str) -> Answer<Result<PostOutcome, String>> {
        self.ask(
            |reply| ChannelMsg::Post {
                from: from.to_string(),
                name: name.to_string(),
                text: text.to_string(),
                reply,
            },
            Err(SESSION_ENDED.to_string()),
        )
    }

    /// One `@`, if it is still open — the chase's whole question, asked of the
    /// same record the roster reads, so a nudge and a row can never disagree.
    pub fn open_mention(&self, name: &str, seq: u64, to: &str) -> Answer<Option<Mention>> {
        self.ask(
            |reply| ChannelMsg::OpenMention {
                name: name.to_string(),
                seq,
                to: to.to_string(),
                reply,
            },
            None,
        )
    }

    /// Every `@` this room is still waiting on, oldest first (v7 batch 3).
    ///
    /// The room's own answer to "is anybody blocked on anybody here", which is
    /// what its roster row and its page header say.
    pub fn owed_in(&self, name: &str) -> Vec<Mention> {
        self.room(name, |room| {
            room.mentions
                .iter()
                .filter(|m| m.answered.is_none())
                .cloned()
                .collect()
        })
        .unwrap_or_default()
    }

    /// Where one member stands in every room it belongs to: how far it has
    /// read, and the oldest `@` it has not answered.
    ///
    /// Rooms it owes nothing in and has read to the end of are left out — a row
    /// with nothing to report should report nothing.
    pub fn standing_of(&self, member: &str) -> Vec<MemberStanding> {
        let view = self.view.borrow();
        let mut out: Vec<MemberStanding> = view
            .rooms
            .iter()
            .filter(|(_, room)| room.members.iter().any(|m| m == member))
            .filter_map(|(name, room)| {
                let read_to = room.seen.get(member).copied().unwrap_or(0);
                let owes = room
                    .mentions
                    .iter()
                    .find(|m| m.answered.is_none() && m.settled_by(member))
                    .cloned();
                (owes.is_some() || read_to < room.seq).then(|| MemberStanding {
                    room: name.clone(),
                    read_to,
                    owes,
                })
            })
            .collect();
        // What a member owes comes before what it has merely not read; the rooms
        // arrive in name order, so a status row cannot flicker between frames.
        out.sort_by(|a, b| {
            b.owes
                .is_some()
                .cmp(&a.owes.is_some())
                .then_with(|| a.room.cmp(&b.room))
        });
        out
    }

    /// Mark the member's inbox as digested up to seq (its running turn was injected with channel messages up to seq).
    pub fn mark_seen(&self, member: &str, name: &str, seq: u64) {
        self.report(ChannelMsg::MarkSeen {
            member: member.to_string(),
            name: name.to_string(),
            seq,
        });
    }

    /// Display-row snapshot: (watch_id, detail, tail text of the log).
    pub fn row_snapshot(
        &self,
        name: &str,
    ) -> Option<(Option<crate::watch::WatchId>, String, String)> {
        const TAIL: usize = 50;
        self.room(name, |room| {
            // "latest" means the latest thing anybody *said*. A room whose last
            // entry is a join has not gone quiet, and a row reading `latest coder:
            // joined` would say it had.
            let detail = match room.log.iter().rev().find(|m| m.kind == MessageKind::Said) {
                Some(last) => format!(
                    "{} msgs · latest {}: {}",
                    room.seq,
                    last.from,
                    crate::tool::agent::excerpt(&last.text)
                ),
                None => format!("{} msgs", room.seq),
            };
            let skipped = room.log.len().saturating_sub(TAIL);
            let mut lines: Vec<String> = Vec::new();
            if skipped > 0 {
                lines.push(format!("… ({skipped} earlier msgs skipped)"));
            }
            lines.extend(room.log.iter().skip(skipped).map(|m| match m.kind {
                MessageKind::Said => format!("{}. {}: {}", m.seq, m.from, m.text),
                MessageKind::Membership => format!("· {} {} ·", m.from, m.text),
            }));
            (room.watch_id, detail, lines.join("\n"))
        })
    }

    /// Single-channel snapshot (TUI room header).
    pub fn info(&self, name: &str) -> Option<ChannelStatus> {
        self.room(name, |room| ChannelStatus {
            name: name.to_string(),
            members: room.members.clone(),
            mode: room.mode,
            seq: room.seq,
            frozen: room.frozen,
        })
    }

    /// Full message log (TUI room rendering; cloned, the caller polls per frame).
    pub fn log_of(&self, name: &str) -> Vec<ChannelMessage> {
        self.room(name, |room| room.log.clone()).unwrap_or_default()
    }

    pub fn list(&self) -> Vec<ChannelStatus> {
        // The rooms are already in name order: the view holds them in a sorted
        // map so no reader has to impose one.
        self.view
            .borrow()
            .rooms
            .iter()
            .map(|(name, room)| ChannelStatus {
                name: name.clone(),
                members: room.members.clone(),
                mode: room.mode,
                seq: room.seq,
                frozen: room.frozen,
            })
            .collect()
    }

    pub fn has_main_mail(&self) -> bool {
        self.view.borrow().main_mail > 0
    }

    /// Drain channel messages pending injection into the main agent (batch-injected at turn boundaries).
    pub fn drain_main_mail(&self) -> Answer<Vec<String>> {
        self.ask(|reply| ChannelMsg::DrainMainMail { reply }, Vec::new())
    }

    /// Land a direct message for the main agent (D98's `SendMessage(to: "main")`).
    ///
    /// It rides the room relays' store because the main agent has one inbox and
    /// one place it is injected from; what tells the two apart is the marker on
    /// the line, which is also what lets a reader attribute it.
    pub fn deliver_to_main(&self, from: &str, text: &str, summary: Option<&str>, urgent: bool) {
        self.report(ChannelMsg::DeliverToMain {
            from: from.to_string(),
            text: text.to_string(),
            summary: summary.map(str::to_string),
            urgent,
        });
    }

    /// Take the direct messages that landed for `main` since the last look, for
    /// the surface that signals them — the sender's mail dot in the status
    /// layer (the `@scout❯ <summary>` flow line retired with D114; the
    /// `summary` field rides along unread until a preview surface wants it).
    ///
    /// **Direct messages only.** A room relay lands in the same inbox and wakes
    /// main on the same debounce, and it is deliberately not mirrored here: a
    /// room is a conversation between agents that main happens to overhear, and
    /// a line per post is exactly the flood D98's debounce exists to prevent.
    /// The room's own surfaces are its zoom and the digest main writes after
    /// reading it.
    pub fn drain_main_arrivals(&self) -> Answer<Vec<MainArrival>> {
        self.ask(|reply| ChannelMsg::DrainMainArrivals { reply }, Vec::new())
    }

    /// Take the pending attention request, if any. Reading it clears it: the
    /// bell rings once per message that asked for it.
    pub fn take_main_mail_urgent(&self) -> Answer<bool> {
        self.ask(|reply| ChannelMsg::TakeMainMailUrgent { reply }, false)
    }

    /// Wait until the attached share document has reached the disk.
    #[cfg(test)]
    pub fn flush_share(&self) -> Answer<()> {
        self.ask(|reply| ChannelMsg::FlushShare { reply }, ())
    }

    /// Wait until everything already sent has been applied. A report does not
    /// wait for its receipt, so a caller that reports and then reads the
    /// published view needs a way to say "after that".
    #[cfg(test)]
    pub async fn settle(&self) {
        crate::app::controller::settle(&self.control).await;
    }

    /// The same barrier for a synchronous test, which has no runtime to await on.
    #[cfg(test)]
    pub fn settle_now(&self) {
        crate::app::controller::settle_now(&self.control);
    }
}

/// What a question answers with once the session is over. Unreachable while any
/// handle lives, since the actor outlives them all.
const SESSION_ENDED: &str = "the session has ended";

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ChannelHandle {
        crate::app::AppCore::start(Default::default()).channels()
    }

    fn limited(channel_limits: ChannelLimits) -> ChannelHandle {
        crate::app::AppCore::start(crate::app::SessionSetup {
            channel_limits,
            ..Default::default()
        })
        .channels()
    }

    fn sent(outcome: PostOutcome) -> (u64, Vec<RoomDelivery>) {
        match outcome {
            PostOutcome::Sent {
                seq, deliveries, ..
            } => (seq, deliveries),
            PostOutcome::Stale { .. } => panic!("should land"),
        }
    }

    /// A room's roster is exactly what it was created with — an arbitrary
    /// subset of the team, with nobody seated behind the caller's back (D95).
    #[test]
    fn create_invite_kick_and_list() {
        let reg = registry();
        reg.create("table", vec!["a".into(), "b".into()], ChannelMode::Free)
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            reg.create("table", vec![], ChannelMode::Free)
                .now()
                .is_err(),
            "duplicate name"
        );
        assert!(
            reg.create("", vec![], ChannelMode::Free).now().is_err(),
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
        reg.invite("table", "c")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.invite("table", "c").now().is_err(), "duplicate invite");
        reg.kick("table", "b")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.kick("table", "b").now().is_err(), "not present");
        assert!(
            reg.kick("table", "main").now().is_err(),
            "main cannot be removed"
        );
        assert_eq!(reg.list()[0].members, vec!["a", "c"]);
        // The user joins and leaves like anybody else: that is what a
        // membership is, and a member who cannot leave is a fixture.
        reg.invite("table", USER_NAME)
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.is_member("table", USER_NAME));
        reg.kick("table", USER_NAME)
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(!reg.is_member("table", USER_NAME));
        reg.remove_member_everywhere("a");
        // A report is not waited on, so a listing right after one waits here.
        reg.settle_now();
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
        .now()
        .unwrap_or_else(|e| panic!("{e}"));
        let (seq, deliveries) = sent(
            reg.post("a", "t", "hello everyone")
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
        assert_eq!(seq, 1);
        let names: Vec<&str> = deliveries.iter().map(|d| d.member.as_str()).collect();
        assert_eq!(names, vec!["b", "c"], "not delivered to the sender or main");
        assert!(
            deliveries
                .iter()
                .all(|d| d.msg.from == "a" && d.msg.text == "hello everyone")
        );
        assert!(
            deliveries.iter().all(|d| !d.mentioned),
            "a post naming nobody carries no needs-you-now bit"
        );
        // Main is a member under the same rules (v7): the line is mailed when
        // it is posted, named or not. Main's own posts never flow back at all.
        let mail = reg.drain_main_mail().now();
        assert_eq!(mail, vec!["[#t msg #1] a: hello everyone"]);
        let _ = sent(
            reg.post("main", "t", "quiet")
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
        assert!(!reg.has_main_mail(), "main's own posts do not flow back");
        // user (a human) is a natural member: can post, main hears it, doesn't consume the per_agent budget.
        let (_, deliveries) = sent(
            reg.post("user", "t", "everyone stop")
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
        assert_eq!(
            deliveries
                .iter()
                .map(|d| d.member.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"],
            "user's post reaches every agent member's inbox"
        );
        assert!(reg.drain_main_mail().now()[0].contains("user: everyone stop"));
        // Non-member / unknown channel error.
        assert!(reg.post("ghost", "t", "x").now().is_err());
        assert!(reg.post("a", "nope", "x").now().is_err());
    }

    /// The mention scanner and the badge matcher read a text the same way:
    /// any name one finds, the other finds too, whatever the case or the
    /// punctuation around it — one word-boundary predicate, two readers.
    #[test]
    fn mention_tokens_share_names_word_boundaries() {
        let text = "@Zoe, look — @all: @zoe-2 too (mail@zoe.dev and @zoe again are old news)";
        assert_eq!(
            mention_tokens(text),
            vec!["zoe", "all", "zoe-2"],
            "lowercased, deduplicated, in order of first appearance"
        );
        assert!(mention_tokens("no sigils here").is_empty());
        for member in ["zoe", "zoe-2", "all", "dev", "ghost"] {
            assert_eq!(
                names(text, member),
                mention_tokens(text).iter().any(|t| t == member),
                "{member}: the two readings must agree"
            );
        }
    }

    /// Mentions resolve against the roster at commit time (v6): named members
    /// carry the needs-you-now bit, `@all` names everyone, and a token that
    /// reaches nobody comes back to the sender instead of vanishing.
    #[test]
    fn post_resolves_mentions_against_the_roster() {
        let reg = registry();
        reg.create(
            "t",
            vec!["a".into(), "b".into(), "c".into()],
            ChannelMode::Free,
        )
        .now()
        .unwrap_or_else(|e| panic!("{e}"));
        let (_, deliveries) = sent(
            reg.post("a", "t", "@B take over; @ghost fyi")
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
        let bit = |name: &str| {
            deliveries
                .iter()
                .find(|d| d.member == name)
                .map(|d| d.mentioned)
        };
        assert_eq!(bit("b"), Some(true), "named, case-insensitively");
        assert_eq!(bit("c"), Some(false), "present but not named");
        match reg
            .post("a", "t", "@ghost still around?")
            .now()
            .unwrap_or_else(|e| panic!("{e}"))
        {
            PostOutcome::Sent {
                unknown_mentions, ..
            } => assert_eq!(unknown_mentions, vec!["ghost"], "told, not swallowed"),
            PostOutcome::Stale { .. } => panic!("should land"),
        }
        let (_, deliveries) = sent(
            reg.post("a", "t", "@all stand-up in five")
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
        assert!(
            deliveries.iter().all(|d| d.mentioned),
            "@all names everyone"
        );
    }

    /// v7: main is a member, and a member's inbox is not gated. Every room
    /// line reaches `main_mail` when it is posted, in room order, mention or
    /// not — the pen, its bulk threshold and its age pump are gone. What main
    /// *owes* is still the `@`'s business, and so is what it says about it.
    #[test]
    fn main_hears_every_room_line_at_once() {
        let reg = registry();
        reg.create("t", vec![MAIN_NAME.into(), "a".into()], ChannelMode::Free)
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(
            reg.post("a", "t", "one")
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
        let _ = sent(
            reg.post("a", "t", "two")
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
        assert!(reg.has_main_mail(), "nothing is held back any more");
        let _ = sent(
            reg.post("a", "t", "@main decide")
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
        assert_eq!(
            reg.drain_main_mail().now(),
            vec![
                "[#t msg #1] a: one",
                "[#t msg #2] a: two",
                "[#t msg #3] a: @main decide",
            ],
            "room order, whether or not a line names main"
        );
        assert!(!reg.has_main_mail(), "and the drain empties it");
    }

    #[test]
    fn serial_bounces_stale_sender_with_increments() {
        let reg = registry();
        reg.create("count", vec!["a".into(), "b".into()], ChannelMode::Serial)
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(
            reg.post("a", "count", "1")
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
        // b hasn't seen a's "1" (seen=0 < seq=1) → bounce back with increments.
        match reg
            .post("b", "count", "1")
            .now()
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
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
        assert_eq!(seq, 2, "retry lands; ordering emerges");
        // mark_seen: after inbox injection, a's cursor advances, no bounce.
        reg.mark_seen("a", "count", 2);
        let (seq, _) = sent(
            reg.post("a", "count", "3")
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
        assert_eq!(seq, 3);
        // Free mode doesn't check.
        reg.create(
            "brainstorm",
            vec!["a".into(), "b".into()],
            ChannelMode::Free,
        )
        .now()
        .unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(
            reg.post("a", "brainstorm", "idea one")
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
        let _ = sent(
            reg.post("b", "brainstorm", "idea two")
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
    }

    #[test]
    fn late_joiner_starts_from_current_head() {
        let reg = registry();
        reg.create("t", vec!["a".into()], ChannelMode::Serial)
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(
            reg.post("a", "t", "old news")
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
        reg.invite("t", "late")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        // Late joiner's seen = head at join time: no backlog bounce, can post immediately.
        let (seq, _) = sent(
            reg.post("late", "t", "I'm here")
                .now()
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
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(
            reg.post("a", "t", "one")
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
        reg.mark_seen("b", "t", 1);
        reg.invite("t", "c").now().unwrap_or_else(|e| panic!("{e}"));
        reg.kick("t", "c").now().unwrap_or_else(|e| panic!("{e}"));
        // b is up to date on everything *said*; two roster changes since then
        // change nothing about that.
        let (seq, _) = sent(
            reg.post("b", "t", "two")
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
        assert_eq!(seq, 4);
        // And the membership entries are not delivered to anybody: only the
        // post is.
        match reg.post("a", "t", "three").now() {
            Ok(PostOutcome::Stale { missed }) => {
                assert_eq!(missed.len(), 1, "only speech is missed: {missed:?}");
                assert_eq!(missed[0].text, "two");
            }
            other => panic!("a is behind by one message, got {other:?}"),
        }
    }

    #[test]
    fn per_channel_message_limit_overrides_registry() {
        let reg = limited(ChannelLimits {
            channel_total: 100,
            per_agent: 100,
        });
        reg.create("t", vec!["a".into()], ChannelMode::Free)
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        // Channel-level override is 1: the second message freezes it.
        reg.set_message_limit("t", 1)
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(
            reg.post("a", "t", "1")
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
        let err = reg.post("a", "t", "2").now().unwrap_err();
        assert!(err.contains("frozen"), "{err}");
        assert!(reg.list()[0].frozen);
        // 0 is rejected; unknown channel errors.
        assert!(reg.set_message_limit("t", 0).now().is_err());
        assert!(reg.set_message_limit("nope", 5).now().is_err());
    }

    #[test]
    fn budgets_freeze_channel_and_notify_main_once() {
        let reg = limited(ChannelLimits {
            channel_total: 2,
            per_agent: 2,
        });
        reg.create("t", vec!["a".into(), "b".into()], ChannelMode::Free)
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(
            reg.post("a", "t", "1")
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
        let _ = sent(
            reg.post("a", "t", "2")
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
        // a hits the per_agent cap.
        let err = reg.post("a", "t", "3").now().unwrap_err();
        assert!(err.contains("cap 2"), "{err}");
        // b triggers the channel total cap: freeze + main gets one warning.
        let _ = reg.drain_main_mail().now();
        let err = reg.post("b", "t", "x").now().unwrap_err();
        assert!(err.contains("frozen"), "{err}");
        assert!(reg.list()[0].frozen);
        let mail = reg.drain_main_mail().now();
        assert_eq!(mail.len(), 1, "{mail:?}");
        assert!(mail[0].contains("now frozen"));
        // Posting after freeze: rejected, no repeated notification.
        let err = reg.post("b", "t", "y").now().unwrap_err();
        assert!(err.contains("is frozen"), "{err}");
        assert!(!reg.has_main_mail());
    }

    #[test]
    fn share_hooks_track_create_invite_kick_post() {
        let root = std::env::temp_dir().join(format!("bingo-ch-{}-share", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = crate::share::ShareStore::load_or_create(&root.join("s.json"))
            .unwrap_or_else(|e| panic!("{e}"));
        let reg = crate::app::AppCore::start(Default::default()).channels();
        reg.attach_share(store.clone());

        // create → room metadata (mode + members).
        reg.create("t", vec!["main".into(), "a".into()], ChannelMode::Free)
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        let doc = store.snapshot();
        assert_eq!(doc.channels.len(), 1);
        assert_eq!(doc.channels[0].mode, "free");
        assert_eq!(doc.channels[0].members, vec!["main", "a"]);
        assert!(doc.channels[0].messages.is_empty());

        // invite/kick → member updates, and the roster change itself is part of
        // the record that gets persisted (D95).
        reg.invite("t", "b").now().unwrap_or_else(|e| panic!("{e}"));
        reg.kick("t", "a").now().unwrap_or_else(|e| panic!("{e}"));
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
        let (seq, _) = sent(
            reg.post("b", "t", "hi")
                .now()
                .unwrap_or_else(|e| panic!("{e}")),
        );
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
        let reg = crate::app::AppCore::start(Default::default()).channels();
        reg.attach_share(store);
        reg.create(
            "t",
            vec!["main".into(), "a".into(), "b".into()],
            ChannelMode::Free,
        )
        .now()
        .unwrap_or_else(|error| panic!("{error}"));

        reg.remove_member_everywhere("a");
        reg.flush_share().now();

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
        let reg = crate::app::AppCore::start(Default::default()).channels();
        reg.create("t", vec!["a".into()], ChannelMode::Free)
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(
            reg.post("a", "t", "source session")
                .now()
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

        reg.align_with_share(store.clone());
        reg.attach_share(store.clone());
        let _ = sent(
            reg.post("a", "t", "after rebind")
                .now()
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
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        let (_, detail, payload) = reg
            .row_snapshot("t")
            .unwrap_or_else(|| panic!("has channel"));
        assert_eq!(detail, "0 msgs");
        assert!(payload.is_empty());
        let _ = sent(
            reg.post("a", "t", "first line")
                .now()
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

    // -----------------------------------------------------------------------
    // The `@` ledger (v7 batch 3)
    // -----------------------------------------------------------------------

    /// The debt the `@` opens, and the post that settles it. Before this a
    /// mention in a room ran bare: the runtime recorded who was named nowhere,
    /// so a member that read a question and said nothing looked exactly like one
    /// that had not read it yet.
    #[test]
    fn an_at_opens_a_debt_and_speaking_settles_it() {
        let reg = registry();
        reg.create("build", vec!["dev".into(), "qa".into()], ChannelMode::Free)
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        reg.post("qa", "build", "@dev is the lexer done?")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));

        let owed = reg.owed_in("build");
        assert_eq!(owed.len(), 1, "one `@`, one debt: {owed:?}");
        assert_eq!((owed[0].to.as_str(), owed[0].from.as_str()), ("dev", "qa"));

        // Somebody else speaking is not an answer to a named `@`.
        reg.post("qa", "build", "no rush")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            reg.owed_in("build").len(),
            1,
            "only the named member closes it"
        );

        reg.post("dev", "build", "not yet — two cases left")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.owed_in("build").is_empty(), "speaking is the answer");
    }

    /// R4: `@all` asks the room, not each member, so it is one debt and the
    /// first answer from anybody but the asker covers it.
    #[test]
    fn at_all_is_one_debt_the_room_owes_together() {
        let reg = registry();
        reg.create(
            "build",
            vec!["dev".into(), "qa".into(), "ops".into()],
            ChannelMode::Free,
        )
        .now()
        .unwrap_or_else(|e| panic!("{e}"));
        reg.post("dev", "build", "@all who owns the release?")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        let owed = reg.owed_in("build");
        assert_eq!(owed.len(), 1, "one debt, not one per member: {owed:?}");
        assert_eq!(owed[0].to, ALL_NAME);

        // The asker talking again does not answer its own question.
        reg.post("dev", "build", "anyone?")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(reg.owed_in("build").len(), 1);

        reg.post("ops", "build", "I do")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            reg.owed_in("build").is_empty(),
            "one covered answer is enough"
        );
    }

    /// A member that answers and asks in the same breath settles the old debt
    /// and opens the new one — the close pass runs before the open pass, so a
    /// post can never settle the question it is itself asking.
    #[test]
    fn answering_with_a_question_settles_one_and_opens_another() {
        let reg = registry();
        reg.create("build", vec!["dev".into(), "qa".into()], ChannelMode::Free)
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        reg.post("qa", "build", "@dev is it done?")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        reg.post("dev", "build", "almost — @qa which fixture?")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        let owed = reg.owed_in("build");
        assert_eq!(
            owed.len(),
            1,
            "the answer closed one and asked one: {owed:?}"
        );
        assert_eq!(owed[0].to, "qa");
    }

    /// What a member's row says, and the half that separates "has not looked
    /// yet" from "looked and said nothing" — the two situations that were
    /// indistinguishable before the cursor was ever shown.
    #[test]
    fn a_members_standing_carries_both_the_debt_and_the_cursor() {
        let reg = registry();
        reg.create("build", vec!["dev".into(), "qa".into()], ChannelMode::Free)
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        reg.post("qa", "build", "@dev the suite is red")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));

        let standing = reg.standing_of("dev");
        assert_eq!(standing.len(), 1);
        assert_eq!(standing[0].room, "build");
        assert_eq!(standing[0].read_to, 0, "not in dev's context yet");
        assert_eq!(standing[0].owes.as_ref().map(|m| m.seq), Some(1));

        reg.mark_seen("dev", "build", 1);
        reg.settle_now();
        let standing = reg.standing_of("dev");
        assert_eq!(standing[0].read_to, 1, "read it — and still owes it");
        assert!(standing[0].owes.is_some(), "reading is not answering");

        // qa asked and is owed nothing; it has read its own post, so it has
        // nothing to report at all.
        assert!(
            reg.standing_of("qa").is_empty(),
            "a member with no debt and nothing unread reports nothing"
        );
    }

    /// The chase asks the same record the roster reads, so a nudge and a row
    /// can never disagree about whether an answer is still owed.
    #[test]
    fn the_chase_and_the_row_read_one_record() {
        let reg = registry();
        reg.create("build", vec!["dev".into(), "qa".into()], ChannelMode::Free)
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        reg.post("qa", "build", "@dev status?")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.open_mention("build", 1, "dev").now().is_some());
        assert!(
            reg.open_mention("build", 1, "qa").now().is_none(),
            "the debt is the named member's, not the asker's"
        );
        reg.post("dev", "build", "green")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            reg.open_mention("build", 1, "dev").now().is_none(),
            "answered — the chase stops here"
        );
    }

    /// A name that is only being reported on is not a summons (R5), so it opens
    /// no debt: the sigil is what the ledger keys on, and prose is not.
    #[test]
    fn a_name_without_the_sigil_owes_nothing() {
        let reg = registry();
        reg.create("build", vec!["dev".into(), "qa".into()], ChannelMode::Free)
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        reg.post("qa", "build", "dev fixed the lexer this morning")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.owed_in("build").is_empty());
    }
}
