//! The perspective page's projection (D96): one agent's communications, split
//! into the threads it actually had.
//!
//! **What this is.** For any agent X, a read-only dossier: X's direct
//! conversations grouped by counterpart, the rooms X is in, the intake X was
//! handed, and a merged timeline of everything. It is the audit layer the
//! privacy stance names — X's DM with somebody who is not the user is visible
//! *here*, and only here, while the user's own `@X` pair view stays pure.
//!
//! **Why the split is work rather than a filter.** The sender is not a field.
//! `InboxItem::Direct` carries a real `from`, but `absorb_inbox` renders the
//! batch into one flat prompt string and the name is gone: what survives is a
//! set of literal markers, which [`crate::tui::buffer::line_source`] is the
//! single parser for. Recovering who said what is therefore a walk, and [`walk`]
//! is it.
//!
//! **And the walk has two readers (D99).** [`dossier`] keeps every lane the walk
//! files. [`pair_lane`] keeps exactly one — the user's — and that is what
//! [`crate::tui::buffer::dm_posts`] renders, so `@X` is the pure pair the model
//! says it is and this page is where everything else in X's life is read. The
//! two cannot disagree about attribution, because there is one walk.
//!
//! **And two protagonists (D100).** Main has a page too, over its own session
//! transcript, and the unmarked default *flips*: in a subagent's history
//! unmarked user-role prose is the hub speaking, because the hub is the sender
//! `direct_text` leaves unmarked; in main's own history it is the human at the
//! keyboard, because nothing else writes plain prose there. Nor is there a spawn
//! task, so the first-message-is-intake rule does not apply. Both facts are
//! [`Protagonist`], which the walk carries instead of the two constants it used
//! to hardcode. Every already-recognised shape keeps its home.
//!
//! **What the markers can and cannot say** (the D96 attribution inventory):
//!
//! | Shape | Composed at | Attributed to |
//! |---|---|---|
//! | `[DM from user]` heading a line | `tool::agent::direct_text` | the user |
//! | `[Message from user, sent while you were working]` block | `steer::SteerItem::block_text` | the user |
//! | `[follow-up instruction] …` | `direct_text`, batched | the protagonist's default counterpart |
//! | unmarked prose | `direct_text`, single | the default: the hub in a subagent's record, the user in main's |
//! | `[#room msg #N] who: …` | `absorb_inbox` | the room (timeline only; the room's own log is authoritative) |
//! | `[follow-up N/M] …` | `absorb_inbox` | intake — a chase, nobody wrote it |
//! | `[SYSTEM NOTIFICATION - TASK REMINDER]` | `query::maybe_inject_task_reminder` | intake |
//! | `<task-notifications>` | `query` | intake |
//! | the first user message | the `Agent` tool's prompt | intake — the task that created X (subagents only: main was not spawned) |
//! | interrupt / compaction / stop-hook / max-tokens | `query`, `compact` | nobody: timeline only |
//!
//! **The one thing the domain cannot express today.** `SendMessage` is
//! assembled at every depth since D98, but `check_target` narrows by caller:
//! main reaches any instance, a subagent reaches `main` and the rooms it is in.
//! So agent→agent *direct* messages still do not exist; agents reach each other
//! through rooms. The counterpart lane is keyed by name rather than by an enum
//! precisely so that the day the addressing rules open, this projection needs no
//! change to show it. (The stale half of this paragraph — `SendMessage`
//! hardcoding `main` at depth 0 — was left behind by D98 and is corrected here.)

use std::collections::BTreeMap;

use crate::api::types::{ContentBlock, Message, Role as ApiRole};
use crate::channels::{ChannelMessage, HUB_NAME, USER_NAME};
use crate::tui::buffer::{
    LineSource, Post, PostKind, THINKING_ROW, channel_posts, line_source, tool_call_line,
};

/// Which thread a lane is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneId {
    /// Everything, in order — the complete record of X's own transcript.
    Timeline,
    /// A pair conversation with one counterpart: `user` and `main` on a
    /// subagent's page, `user` and every agent that wrote in on main's, and one
    /// day another agent on a subagent's.
    Dm(String),
    /// A room X speaks in. Its posts are the *room's* log, not X's history.
    Room(String),
    /// What was handed to X rather than said to it: the task that created it,
    /// task reminders, notifications, the chases for its silence.
    Intake,
}

impl LaneId {
    /// The lane's name in the index, from X's point of view.
    pub fn label(&self) -> String {
        match self {
            Self::Timeline => "timeline".to_string(),
            Self::Dm(who) => format!("@{who}"),
            Self::Room(name) => format!("#{name}"),
            Self::Intake => "intake".to_string(),
        }
    }

    /// The rule a thread opens under. Every one of them says `read-only`, in the
    /// observer vocabulary D95 established — the page is a dossier, and the
    /// framing is a fact about the view rather than about the host.
    pub fn title(&self, agent: &str) -> String {
        match self {
            Self::Timeline => format!("@{agent} · timeline · read-only"),
            Self::Dm(who) => format!("@{agent} ↔ @{who} · read-only"),
            Self::Room(name) => format!("#{name} · @{agent}'s view · read-only"),
            Self::Intake => format!("@{agent} · intake · read-only"),
        }
    }
}

/// One thread on the page, built at snapshot time.
#[derive(Debug, Clone)]
pub struct Lane {
    pub id: LaneId,
    /// The thread, in order, including X's process rows (the protagonist rule).
    pub posts: Vec<Post>,
    /// Unix seconds of the most recent post that carried a clock; 0 when none
    /// did (a compaction clears an instance's stamps outright).
    pub last_at: u64,
}

impl Lane {
    /// How many *messages* the lane holds. Process rows are X's work, not
    /// things anybody said, so they do not count — an index that read
    /// `@main (47)` because a turn made forty-five tool calls would be
    /// measuring the wrong thing.
    pub fn messages(&self) -> usize {
        self.posts
            .iter()
            .filter(|p| p.kind == PostKind::Said)
            .count()
    }

    fn new(id: LaneId, posts: Vec<Post>) -> Self {
        let last_at = posts.iter().map(|p| p.at).max().unwrap_or(0);
        Self { id, posts, last_at }
    }
}

/// One agent's communications, grouped.
#[derive(Debug, Clone)]
pub struct Dossier {
    pub agent: String,
    /// Timeline first, then DMs and rooms by last activity, then intake. Empty
    /// lanes are dropped: a page is a record of what happened, and a row that
    /// says `#parser (0)` is furniture.
    pub lanes: Vec<Lane>,
}

impl Dossier {
    /// Nobody ever wrote to this agent and it never wrote to anybody.
    pub fn is_empty(&self) -> bool {
        self.lanes.is_empty()
    }
}

/// Where one piece of a user-role message belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// A counterpart's DM lane. Also becomes the *active* lane, which is what
    /// X's following turns attach to.
    Dm(String),
    Intake,
    /// Real, but attributable to nobody: it lands in the timeline and nowhere
    /// else, which is the rule that keeps a thread honest.
    TimelineOnly,
}

/// What one of X's own rows *was*, for a reader that needs more than the line.
///
/// The line ([`tool_call_line`], [`THINKING_ROW`]) is all a thread pager needs.
/// The pair view (D99) renders X's work the way the console renders main's —
/// collapsed activity groups — and the classifier that builds those groups
/// reads the call, not the sentence it printed.
#[derive(Debug, Clone)]
pub(crate) enum Work {
    Tool {
        name: String,
        input: serde_json::Value,
    },
    Thinking,
}

/// Whose record is being walked, and therefore what its unmarked shapes mean.
///
/// The markers are the same for everybody; the *defaults* are not. A subagent's
/// history is written by `absorb_inbox`, which leaves exactly one sender —
/// the hub — unmarked, and opens with the prompt the `Agent` tool spawned it
/// with. Main's history is its session transcript: the unmarked prose in it is
/// the human typing into the console, and there is no spawn prompt because
/// nobody dispatched main. Two facts, carried rather than hardcoded, so one walk
/// serves both pages.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Protagonist<'a> {
    /// The agent whose record this is: every one of its own rows is `from` this.
    pub name: &'a str,
    /// Who unmarked user-role prose belongs to — and, labelled, `[follow-up
    /// instruction]`, which is the same sender with a batch label on it.
    pub default: &'a str,
    /// The first user text is the task that created this instance, and therefore
    /// intake rather than conversation. False for main, which was never spawned.
    pub spawned: bool,
}

impl<'a> Protagonist<'a> {
    /// A subagent: the hub is its unmarked voice, and its history opens with the
    /// task it was dispatched with.
    fn agent(name: &'a str) -> Self {
        Self {
            name,
            default: HUB_NAME,
            spawned: true,
        }
    }

    /// Main, over its own session transcript.
    fn main() -> Self {
        Self {
            name: HUB_NAME,
            default: USER_NAME,
            spawned: false,
        }
    }

    /// Which one a name is. The main agent's member name is reserved
    /// (`channels::HUB_NAME`), so no instance can answer to it and this cannot
    /// mistake a teammate for the console.
    pub(crate) fn of(name: &'a str) -> Self {
        if name == HUB_NAME {
            Self::main()
        } else {
            Self::agent(name)
        }
    }
}

/// One piece of an agent's record, in the order the record holds it.
#[derive(Debug, Clone)]
pub(crate) struct Filed {
    pub target: Target,
    pub post: Post,
    /// Set only on X's own work rows.
    pub work: Option<Work>,
}

/// One agent's history → every post in it, in order, each already filed.
///
/// **The single recognition walk (D99).** The perspective page files every item
/// into its lane; the user's `@X` pair view keeps the one lane it is in
/// ([`pair_lane`]). Both therefore agree about who said what by construction,
/// rather than by two parsers that happen to read the same markers today.
pub(crate) fn walk(who: Protagonist<'_>, history: &[Message], stamps: &[u64]) -> Vec<Filed> {
    let agent = who.name;
    let mut out: Vec<Filed> = Vec::new();
    // Which lane X's own turns belong to: the counterpart it last heard from.
    // Where interleaving makes exact reply-attribution impossible this is a
    // best effort by construction — the timeline is the lane that is complete.
    let mut active: Option<String> = None;
    let mut seen_user_text = false;
    let turn = |active: &Option<String>, post: Post, work: Option<Work>| Filed {
        target: match active {
            Some(name) => Target::Dm(name.clone()),
            None => Target::TimelineOnly,
        },
        post,
        work,
    };
    for (i, msg) in history.iter().enumerate() {
        let at = stamps.get(i).copied().unwrap_or(0);
        for block in &msg.content {
            match (msg.role, block) {
                (ApiRole::User, ContentBlock::Text { text }) => {
                    let first = who.spawned && !seen_user_text;
                    seen_user_text = true;
                    for (target, post) in split_user_text(text, at, first, who) {
                        if let Target::Dm(name) = &target {
                            active = Some(name.clone());
                        }
                        out.push(Filed {
                            target,
                            post,
                            work: None,
                        });
                    }
                }
                (ApiRole::Assistant, ContentBlock::Text { text }) => out.push(turn(
                    &active,
                    Post {
                        from: agent.to_string(),
                        you: false,
                        at,
                        text: text.clone(),
                        kind: PostKind::Said,
                    },
                    None,
                )),
                (ApiRole::Assistant, ContentBlock::ToolUse { name, input, .. }) => out.push(turn(
                    &active,
                    Post {
                        from: agent.to_string(),
                        you: false,
                        at: 0,
                        text: tool_call_line(name, input),
                        kind: PostKind::Process,
                    },
                    Some(Work::Tool {
                        name: name.clone(),
                        input: input.clone(),
                    }),
                )),
                (ApiRole::Assistant, ContentBlock::Thinking { .. }) => out.push(turn(
                    &active,
                    Post {
                        from: agent.to_string(),
                        you: false,
                        at: 0,
                        text: THINKING_ROW.to_string(),
                        kind: PostKind::Process,
                    },
                    Some(Work::Thinking),
                )),
                _ => {}
            }
        }
    }
    out
}

/// One post of the user↔agent pair lane, plus the one thing a filter loses.
pub(crate) struct PairPost {
    pub post: Post,
    pub work: Option<Work>,
    /// Whether the previous item of the *full* walk is the previous item here.
    ///
    /// The pair view merges X's consecutive rows into one message, the way the
    /// console holds a turn. What must not merge is two runs with something
    /// between them — a room relay, an instruction from main, a chase — because
    /// that something is exactly what ended the first run, even though it
    /// renders nowhere in this lane.
    pub contiguous: bool,
}

/// The user's side of an agent's record: what the user said, what X answered
/// them, and the work X did for those turns (D99).
///
/// Everything else — main's instructions, room relays, `[message from @X]`
/// mail, chases, reminders, and the task that created the instance — belongs to
/// a lane that is not this one, and lives on the observation page.
pub(crate) fn pair_lane(agent: &str, history: &[Message], stamps: &[u64]) -> Vec<PairPost> {
    let mut out: Vec<PairPost> = Vec::new();
    let mut previous: Option<usize> = None;
    for (i, filed) in walk(Protagonist::of(agent), history, stamps)
        .into_iter()
        .enumerate()
    {
        if !matches!(&filed.target, Target::Dm(name) if name == USER_NAME) {
            continue;
        }
        out.push(PairPost {
            post: filed.post,
            work: filed.work,
            contiguous: previous == Some(i.wrapping_sub(1)),
        });
        previous = Some(i);
    }
    out
}

/// Scaffolding that no counterpart wrote: the runtime talking to itself.
///
/// These are recognised so they cannot fall through to the unmarked-prose rule
/// and be misfiled as the hub speaking — the failure mode that would put words
/// in somebody's mouth.
fn runtime_only(text: &str) -> bool {
    let trimmed = text.trim();
    crate::query::is_interrupt_marker(trimmed)
        || trimmed == crate::query::MAX_TOKENS_RESUME_PROMPT
        || trimmed.starts_with(crate::transcript::COMPACT_SUMMARY_PREFIX)
        || trimmed.starts_with("(Stop hook blocked continuation)")
}

/// A note nobody said, for the timeline's completeness.
fn note(from: &str, at: u64, text: String) -> Post {
    Post {
        from: from.to_string(),
        you: false,
        at,
        text,
        kind: PostKind::Note,
    }
}

/// One user-role text block → the pieces it is made of, each with its home.
///
/// `first` marks the very first user text in the history: the prompt the `Agent`
/// tool started X with. It is unmarked prose and would otherwise read as the
/// hub speaking, which is nearly true and still wrong — it is the task, and the
/// task is intake. It is never set for main, which nobody dispatched (D100), so
/// the first thing the user ever typed into the console stays a message.
fn split_user_text(text: &str, at: u64, first: bool, who: Protagonist<'_>) -> Vec<(Target, Post)> {
    let mut out: Vec<(Target, Post)> = Vec::new();
    // The main agent's inbox block (D98) is an envelope, not a message: what is
    // in it are room relays and direct messages, each already wearing the marker
    // that says whose they are. Unwrap and read the lines — collapsing the whole
    // block to one timeline note, as the old `<channel-messages>` shape was, is
    // what would lose the sender the marker exists to carry.
    if let Some(body) = text
        .trim()
        .strip_prefix(crate::query::MAIL_BLOCK_OPEN)
        .and_then(|rest| rest.trim_end().strip_suffix(crate::query::MAIL_BLOCK_CLOSE))
    {
        return split_user_text(body.trim(), at, false, who);
    }
    // The pre-D98 envelope. Histories recorded before the rename still carry
    // it, and a reader that forgot the old shape would file the whole block as
    // the hub speaking. The lines inside wear their own markers, so unwrapping
    // attributes them the same way the new envelope's are.
    if let Some(body) = text
        .trim()
        .strip_prefix("<channel-messages>")
        .and_then(|rest| rest.trim_end().strip_suffix("</channel-messages>"))
    {
        return split_user_text(body.trim(), at, false, who);
    }
    if runtime_only(text) {
        out.push((
            Target::TimelineOnly,
            note("", at, one_line_summary(text.trim())),
        ));
        return out;
    }
    if let Some(body) = text.strip_prefix(crate::steer::STEER_MARKER) {
        // The user typed it while X was working. A real message, from a real
        // person, that arrived beside a tool result.
        out.push((
            Target::Dm(USER_NAME.to_string()),
            said(USER_NAME, at, body.trim_start_matches('\n').to_string()),
        ));
        return out;
    }
    if text.trim_start().starts_with("<task-notifications>") {
        out.push((
            Target::Intake,
            note("", at, "system note · task notifications".to_string()),
        ));
        return out;
    }
    if first && line_source(text.lines().next().unwrap_or("")).is_none() {
        out.push((Target::Intake, said("", at, text.to_string())));
        return out;
    }

    // Unmarked prose belongs to the protagonist's default counterpart until a
    // marker says otherwise: the hub in a subagent's record (it is the sender
    // `direct_text` leaves unmarked), the user in main's (nothing else writes
    // plain prose into a session transcript).
    let mut current = Target::Dm(who.default.to_string());
    let mut plain: Vec<&str> = Vec::new();
    let flush = |plain: &mut Vec<&str>, current: &Target, out: &mut Vec<(Target, Post)>| {
        let joined = plain.join("\n");
        plain.clear();
        if joined.trim().is_empty() {
            return;
        }
        let who = match current {
            Target::Dm(name) => name.clone(),
            _ => String::new(),
        };
        out.push((current.clone(), said(&who, at, joined)));
    };
    for line in text.lines() {
        match line_source(line) {
            Some(LineSource::TaskReminder) => {
                flush(&mut plain, &current, &mut out);
                out.push((
                    Target::Intake,
                    note("", at, "system note · task tools".to_string()),
                ));
                return out;
            }
            Some(LineSource::User) => {
                flush(&mut plain, &current, &mut out);
                current = Target::Dm(USER_NAME.to_string());
            }
            // The same sender as unmarked prose, wearing a batch label because
            // the batch made its boundaries ambiguous — so it files where the
            // unmarked default files, and not at a second hardcoded name.
            Some(LineSource::HubBatched { text }) => {
                flush(&mut plain, &current, &mut out);
                current = Target::Dm(who.default.to_string());
                out.push((current.clone(), said(who.default, at, text)));
            }
            // An agent wrote to the protagonist directly (D98). Like the user's
            // marker it is a header: the lane switches and the prose under it is
            // that agent's, so a page can finally say who said what to main.
            Some(LineSource::Agent { name }) => {
                flush(&mut plain, &current, &mut out);
                current = Target::Dm(name);
            }
            Some(LineSource::Room { channel, body }) => {
                // The room's own log is the authoritative copy, so the relay is
                // recorded in the timeline and left out of the room thread —
                // counting it twice would make a lane disagree with itself.
                flush(&mut plain, &current, &mut out);
                out.push((
                    Target::TimelineOnly,
                    note("", at, format!("#{channel} · {body}")),
                ));
            }
            Some(LineSource::Chase) => {
                flush(&mut plain, &current, &mut out);
                out.push((
                    Target::Intake,
                    note("", at, "follow-up · waiting for a reply".to_string()),
                ));
            }
            None => plain.push(line),
        }
    }
    flush(&mut plain, &current, &mut out);
    out
}

fn said(from: &str, at: u64, text: String) -> Post {
    Post {
        from: from.to_string(),
        you: from == USER_NAME,
        at,
        text,
        kind: PostKind::Said,
    }
}

/// A runtime block collapsed to the one line it reads as.
fn one_line_summary(text: &str) -> String {
    match text.lines().next() {
        Some(first) if text.lines().count() > 1 => format!("{first} …"),
        Some(first) => first.to_string(),
        None => String::new(),
    }
}

/// Build one agent's dossier.
///
/// `agent` is any participant, `main` included (D100): the projection reads
/// [`Protagonist::of`] to decide what its unmarked shapes mean, and everything
/// downstream — lanes, ordering, counts, titles — is the same page for both.
///
/// `rooms` is the room name plus that room's whole log, which the caller reads
/// from the channel registry — the projection stays pure, so a test can hand it
/// a history and a log and get a page back.
///
/// **Snapshot, not a feed.** Everything is built once, at the moment the page
/// opens (the D82 transcript precedent); a message that arrives afterwards does
/// not mutate the view, and reopening is how you refresh.
pub fn dossier(
    agent: &str,
    history: &[Message],
    stamps: &[u64],
    rooms: &[(String, Vec<ChannelMessage>)],
) -> Dossier {
    let mut timeline: Vec<Post> = Vec::new();
    let mut dms: BTreeMap<String, Vec<Post>> = BTreeMap::new();
    let mut intake: Vec<Post> = Vec::new();

    for filed in walk(Protagonist::of(agent), history, stamps) {
        timeline.push(filed.post.clone());
        match filed.target {
            Target::Dm(name) => dms.entry(name).or_default().push(filed.post),
            Target::Intake => intake.push(filed.post),
            Target::TimelineOnly => {}
        }
    }

    let mut lanes = vec![Lane::new(LaneId::Timeline, timeline)];
    let mut pairs: Vec<Lane> = dms
        .into_iter()
        .filter(|(_, posts)| !posts.is_empty())
        .map(|(name, posts)| Lane::new(LaneId::Dm(name), posts))
        .collect();
    pairs.sort_by(|a, b| {
        b.last_at
            .cmp(&a.last_at)
            .then_with(|| a.id.label().cmp(&b.id.label()))
    });
    lanes.extend(pairs);

    let mut room_lanes: Vec<Lane> = rooms
        .iter()
        .map(|(name, log)| Lane::new(LaneId::Room(name.clone()), channel_posts(log, agent)))
        .filter(|lane| !lane.posts.is_empty())
        .collect();
    room_lanes.sort_by(|a, b| {
        b.last_at
            .cmp(&a.last_at)
            .then_with(|| a.id.label().cmp(&b.id.label()))
    });
    lanes.extend(room_lanes);

    if !intake.is_empty() {
        lanes.push(Lane::new(LaneId::Intake, intake));
    }
    // The timeline is dropped when it is the only thing there is: a page whose
    // single row is "timeline (0)" says nothing an empty page does not.
    if lanes.len() == 1 && lanes[0].posts.is_empty() {
        lanes.clear();
    }
    Dossier {
        agent: agent.to_string(),
        lanes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::MessageKind;

    fn user(text: &str) -> Message {
        Message::user_text(text)
    }

    fn assistant(blocks: Vec<ContentBlock>) -> Message {
        Message {
            role: ApiRole::Assistant,
            content: blocks,
        }
    }

    fn text(t: &str) -> ContentBlock {
        ContentBlock::Text {
            text: t.to_string(),
        }
    }

    fn tool(name: &str, input: serde_json::Value) -> ContentBlock {
        ContentBlock::ToolUse {
            id: "toolu_1".to_string(),
            name: name.to_string(),
            input,
        }
    }

    /// The lane with this identity. Tests ask for one by name; production
    /// walks the list it just built, so this stays where its readers are.
    fn lane_of<'a>(page: &'a Dossier, id: &LaneId) -> Option<&'a Lane> {
        page.lanes.iter().find(|lane| &lane.id == id)
    }

    fn said_texts(lane: &Lane) -> Vec<&str> {
        lane.posts
            .iter()
            .filter(|p| p.kind == PostKind::Said)
            .map(|p| p.text.as_str())
            .collect()
    }

    /// The attribution catalog, pinned: every shape the runtime can put in an
    /// agent's history, filed where it belongs.
    #[test]
    fn a_page_groups_every_counterpart_the_markers_can_name() {
        let history = vec![
            // The task that created it: unmarked prose, and still not the hub
            // making conversation.
            user("map the parser module"),
            assistant(vec![text("on it")]),
            // The hub, single and therefore unmarked.
            user("also check the lexer"),
            assistant(vec![text("will do")]),
            // The user, marked.
            user(&format!(
                "{}\nare you nearly done?",
                crate::tool::agent::DM_FROM_USER_MARKER
            )),
            assistant(vec![text("nearly")]),
            // A batch: the hub labelled, the user marked, a room relayed, a
            // chase, all in one absorbed prompt.
            user(&format!(
                "[follow-up instruction] and the printer\n{}\nping\n[#parser msg #3] scout: found it\n[follow-up 1/3] The hub sent you message #2 and has had no reply",
                crate::tool::agent::DM_FROM_USER_MARKER
            )),
            // Scaffolding nobody wrote.
            user(crate::query::INTERRUPT_MARKER),
            user(&format!(
                "{}\nThe task tools haven't been used recently.",
                crate::query::TASK_REMINDER_MARKER
            )),
        ];
        let stamps = vec![10, 20, 30, 40, 50, 60, 70, 80, 90];
        let page = dossier("scout", &history, &stamps, &[]);

        let hub = lane_of(&page, &LaneId::Dm(HUB_NAME.to_string())).expect("a hub lane");
        assert_eq!(
            said_texts(hub),
            vec!["also check the lexer", "will do", "and the printer"],
            "the hub's unmarked single and its labelled batch line, plus the reply it drew"
        );

        let human = lane_of(&page, &LaneId::Dm(USER_NAME.to_string())).expect("a user lane");
        assert_eq!(
            said_texts(human),
            vec!["are you nearly done?", "nearly", "ping"],
            "only what the user actually wrote, plus the reply it drew"
        );

        let intake = lane_of(&page, &LaneId::Intake).expect("an intake lane");
        let intake_text: Vec<&str> = intake.posts.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(
            intake_text,
            vec![
                "map the parser module",
                "follow-up · waiting for a reply",
                "system note · task tools"
            ],
            "the task that created it, the chase, and the reminder"
        );

        // The room relay is in the timeline and in no thread: the room's own log
        // is the authoritative copy.
        let timeline = lane_of(&page, &LaneId::Timeline).expect("a timeline");
        assert!(
            timeline
                .posts
                .iter()
                .any(|p| p.text == "#parser · scout: found it"),
            "the relay is recorded"
        );
        assert!(
            lane_of(&page, &LaneId::Room("parser".to_string())).is_none(),
            "a relay line does not invent a room lane"
        );
    }

    /// The interrupt marker, the compaction summary and the rest of the
    /// runtime's own talk reach the timeline and stop there — a thread that
    /// showed them would be putting words in somebody's mouth.
    #[test]
    fn scaffolding_nobody_wrote_stays_out_of_every_thread() {
        let history = vec![
            user("start"),
            user(crate::query::INTERRUPT_MARKER),
            user(&format!(
                "{}\nearlier, they discussed the parser",
                crate::transcript::COMPACT_SUMMARY_PREFIX
            )),
            user(crate::query::MAX_TOKENS_RESUME_PROMPT),
        ];
        let page = dossier("scout", &history, &[1, 2, 3, 4], &[]);
        for lane in &page.lanes {
            if lane.id == LaneId::Timeline {
                continue;
            }
            for post in &lane.posts {
                assert!(
                    !post.text.contains("interrupted by user")
                        && !post.text.contains("summary of the earlier")
                        && !post.text.contains("Output token limit"),
                    "{:?} leaked runtime scaffolding: {:?}",
                    lane.id,
                    post.text
                );
            }
        }
        let timeline = lane_of(&page, &LaneId::Timeline).expect("a timeline");
        assert_eq!(
            timeline.posts.len(),
            4,
            "the timeline keeps all four, complete"
        );
    }

    /// The protagonist rule: X's thinking and tool calls show in the thread it
    /// was working in, whoever the counterpart is.
    #[test]
    fn a_thread_shows_the_agent_s_own_process() {
        let history = vec![
            user("task"),
            user(&format!(
                "{}\nwhat does the lexer do?",
                crate::tool::agent::DM_FROM_USER_MARKER
            )),
            assistant(vec![
                ContentBlock::Thinking {
                    thinking: "hmm".to_string(),
                    signature: String::new(),
                },
                tool("Bash", serde_json::json!({"command": "cat lexer.rs"})),
                text("it tokenizes"),
            ]),
        ];
        let page = dossier("scout", &history, &[1, 2, 3], &[]);
        let human = lane_of(&page, &LaneId::Dm(USER_NAME.to_string())).expect("a user lane");
        let kinds: Vec<PostKind> = human.posts.iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds,
            vec![
                PostKind::Said,
                PostKind::Process,
                PostKind::Process,
                PostKind::Said
            ],
            "the question, the reasoning, the tool call, the answer"
        );
        assert_eq!(
            human.messages(),
            2,
            "process rows are work, not messages, so the count says 2"
        );
        assert!(
            human.posts.iter().any(|p| p.text == THINKING_ROW),
            "the same collapsed reasoning row the DM view shows"
        );
    }

    /// D98: an agent writing to main arrives under a marker that names it, and
    /// that is what lets main's own page file the message in the sender's lane
    /// rather than dropping the whole inbox block into the timeline as one note.
    /// The envelope is scaffolding; what is inside it was said by someone.
    #[test]
    fn a_direct_message_to_main_lands_in_its_sender_s_lane() {
        let inbox = format!(
            "{}\n{}\n[#build msg #4] zoe: the tests pass\n{}",
            crate::query::MAIL_BLOCK_OPEN,
            crate::channels::format_main_message("scout", "the migration is done"),
            crate::query::MAIL_BLOCK_CLOSE
        );
        let history = vec![user("run the release"), user(&inbox)];
        let page = dossier(HUB_NAME, &history, &[1, 2], &[]);
        let lane = lane_of(&page, &LaneId::Dm("scout".to_string())).expect("a lane for the sender");
        assert_eq!(said_texts(lane), vec!["the migration is done"]);
        let timeline = lane_of(&page, &LaneId::Timeline).expect("a timeline");
        assert!(
            timeline
                .posts
                .iter()
                .any(|p| p.text == "#build · zoe: the tests pass"),
            "and a room relay in the same block is still the room's, recorded once"
        );
    }

    /// Main's own page (D100). The record is its session transcript, and the
    /// unmarked default flips: the prose in it is the human at the keyboard, not
    /// the hub talking to itself. Nothing dispatched main either, so the first
    /// message is the first thing the user ever said — a message, not intake.
    #[test]
    fn mains_page_reads_unmarked_prose_as_the_user() {
        let history = vec![
            // The first thing typed into the console. In a subagent's record
            // this shape is the spawn task; here there is no spawn.
            user("ship the release"),
            assistant(vec![text("on it")]),
            // An agent wrote to main (D98), inside main's own inbox envelope.
            user(&format!(
                "{}\n{}\n[#build msg #4] zoe: the tests pass\n{}",
                crate::query::MAIL_BLOCK_OPEN,
                crate::channels::format_main_message("scout", "the migration is done"),
                crate::query::MAIL_BLOCK_CLOSE
            )),
            assistant(vec![text("thanks")]),
            // Dispatch notifications are handed to main, not said to it.
            user("<task-notifications>\nscout finished\n</task-notifications>"),
        ];
        let page = dossier(HUB_NAME, &history, &[10, 20, 30, 40, 50], &[]);

        let human = lane_of(&page, &LaneId::Dm(USER_NAME.to_string())).expect("a user lane");
        assert_eq!(
            said_texts(human),
            vec!["ship the release", "on it"],
            "unmarked prose in main's record is the user, and the first line is not intake"
        );
        assert!(
            lane_of(&page, &LaneId::Dm(HUB_NAME.to_string())).is_none(),
            "and main is never its own counterpart"
        );

        let scout =
            lane_of(&page, &LaneId::Dm("scout".to_string())).expect("a lane for the sender");
        assert_eq!(
            said_texts(scout),
            vec!["the migration is done", "thanks"],
            "mail lands in its sender's lane, and the reply it drew with it"
        );

        let intake = lane_of(&page, &LaneId::Intake).expect("an intake lane");
        assert_eq!(
            intake
                .posts
                .iter()
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>(),
            vec!["system note · task notifications"],
            "the notifications are intake and nothing else is"
        );

        let timeline = lane_of(&page, &LaneId::Timeline).expect("a timeline");
        assert!(
            timeline
                .posts
                .iter()
                .any(|p| p.text == "#build · zoe: the tests pass"),
            "a room relay in the same block stays the room's, recorded once"
        );
    }

    /// The flip is a property of the protagonist, not of the shapes: main's page
    /// still reads every marker the way a subagent's does, the legacy envelope
    /// included, and a subagent's page still files unmarked prose to the hub
    /// (which is what every test above this one asserts, unchanged).
    #[test]
    fn the_flipped_default_does_not_move_any_marker() {
        let history = vec![
            user("<channel-messages>\n[#build msg #2] zoe: still failing\n</channel-messages>"),
            user(&format!(
                "{}\nnot the hub",
                crate::tool::agent::DM_FROM_USER_MARKER
            )),
            user(&format!(
                "{}\nThe task tools haven't been used recently.",
                crate::query::TASK_REMINDER_MARKER
            )),
            user(crate::query::INTERRUPT_MARKER),
        ];
        let page = dossier(HUB_NAME, &history, &[1, 2, 3, 4], &[]);
        let timeline = lane_of(&page, &LaneId::Timeline).expect("a timeline");
        assert!(
            timeline
                .posts
                .iter()
                .any(|p| p.text == "#build · zoe: still failing"),
            "the pre-D98 envelope is unwrapped on main's page too"
        );
        assert_eq!(
            said_texts(lane_of(&page, &LaneId::Dm(USER_NAME.to_string())).expect("a user lane")),
            vec!["not the hub"],
            "an explicit user marker files where it always did"
        );
        assert!(
            lane_of(&page, &LaneId::Intake)
                .expect("an intake lane")
                .posts
                .iter()
                .any(|p| p.text == "system note · task tools"),
            "the reminder is intake for main as well"
        );
        for lane in &page.lanes {
            if lane.id == LaneId::Timeline {
                continue;
            }
            assert!(
                !lane.posts.iter().any(|p| p.text.contains("interrupted")),
                "{:?} took the runtime's own words",
                lane.id
            );
        }
    }

    /// A history recorded before D98 wraps its inbox in `<channel-messages>`.
    /// The old envelope reads the same as the new one: its lines wear their
    /// own markers, and forgetting the shape would file the block as the hub
    /// speaking.
    #[test]
    fn the_pre_d98_envelope_is_still_read() {
        let inbox = "<channel-messages>\n[#build msg #2] zoe: still failing\n</channel-messages>";
        let history = vec![user("task"), user(inbox)];
        let page = dossier("scout", &history, &[1, 2], &[]);
        let timeline = lane_of(&page, &LaneId::Timeline).expect("a timeline");
        assert!(
            timeline
                .posts
                .iter()
                .any(|p| p.text == "#build · zoe: still failing"),
            "the relay inside is attributed, not misfiled as hub prose"
        );
        assert!(
            lane_of(&page, &LaneId::Dm(HUB_NAME.to_string())).is_none(),
            "and nothing in the legacy block reads as the hub speaking"
        );
    }

    /// A room thread is the room's conversation with X in it — not X's
    /// monologue. A thread that showed only the protagonist would not be a
    /// thread.
    #[test]
    fn a_room_thread_is_the_whole_room_with_the_agent_in_it() {
        let log = vec![
            ChannelMessage {
                seq: 1,
                from: "ui".to_string(),
                text: "who owns the lexer?".to_string(),
                at: 100,
                kind: MessageKind::Said,
            },
            ChannelMessage {
                seq: 2,
                from: "scout".to_string(),
                text: "I do".to_string(),
                at: 110,
                kind: MessageKind::Said,
            },
            ChannelMessage {
                seq: 3,
                from: "qa".to_string(),
                text: "joined".to_string(),
                at: 120,
                kind: MessageKind::Membership,
            },
        ];
        let page = dossier(
            "scout",
            &[user("task")],
            &[1],
            &[("parser".to_string(), log)],
        );
        let room = lane_of(&page, &LaneId::Room("parser".to_string())).expect("a room lane");
        assert_eq!(
            said_texts(&room.clone()),
            vec!["who owns the lexer?", "I do"],
            "everybody's speech, not only the protagonist's"
        );
        let mine: Vec<bool> = room
            .posts
            .iter()
            .filter(|p| p.kind == PostKind::Said)
            .map(|p| p.you)
            .collect();
        assert_eq!(
            mine,
            vec![false, true],
            "the agent's own rows are the ones marked, so the renderer can emphasize them"
        );
        assert!(
            room.posts.iter().any(|p| p.kind == PostKind::Note),
            "the membership line is part of the room's record"
        );
        assert_eq!(room.last_at, 120);
    }

    /// Everything in a history-derived thread is in the timeline, in the same
    /// order. (Room lanes are the room's own log and are deliberately not
    /// contained in it — X's page is complete about X, not about rooms.)
    #[test]
    fn the_timeline_is_a_superset_of_the_threads_it_split() {
        let history = vec![
            user("task"),
            user("hub says hi"),
            assistant(vec![text("hello")]),
            user(&format!(
                "{}\nuser says hi",
                crate::tool::agent::DM_FROM_USER_MARKER
            )),
            assistant(vec![text("hi there")]),
        ];
        let page = dossier("scout", &history, &[1, 2, 3, 4, 5], &[]);
        let timeline: Vec<&str> = lane_of(&page, &LaneId::Timeline)
            .expect("a timeline")
            .posts
            .iter()
            .map(|p| p.text.as_str())
            .collect();
        for lane in &page.lanes {
            if matches!(lane.id, LaneId::Timeline | LaneId::Room(_)) {
                continue;
            }
            let mut at = 0;
            for post in &lane.posts {
                let found = timeline[at..]
                    .iter()
                    .position(|t| *t == post.text.as_str())
                    .map(|p| p + at);
                let Some(found) = found else {
                    panic!("{:?} has {:?}, the timeline does not", lane.id, post.text);
                };
                at = found + 1;
            }
        }
    }

    /// The index leads with what happened last.
    #[test]
    fn lanes_are_ordered_by_last_activity() {
        let history = vec![
            user("task"),
            user("hub early"),
            user(&format!(
                "{}\nuser late",
                crate::tool::agent::DM_FROM_USER_MARKER
            )),
        ];
        let page = dossier("scout", &history, &[1, 2, 900], &[]);
        let order: Vec<String> = page.lanes.iter().map(|l| l.id.label()).collect();
        assert_eq!(
            order,
            vec!["timeline", "@user", "@main", "intake"],
            "timeline first, counterparts by recency, intake last"
        );
    }

    /// A lane's count is what the index prints beside it, and it counts the
    /// same posts the thread renders as messages.
    #[test]
    fn a_lane_s_count_is_its_thread_s_messages() {
        let history = vec![
            user("task"),
            user("one"),
            assistant(vec![
                tool("Bash", serde_json::json!({"command": "ls"})),
                text("two"),
            ]),
        ];
        let page = dossier("scout", &history, &[1, 2, 3], &[]);
        for lane in &page.lanes {
            assert_eq!(
                lane.messages(),
                lane.posts
                    .iter()
                    .filter(|p| p.kind == PostKind::Said)
                    .count(),
                "{:?}",
                lane.id
            );
        }
    }

    /// An agent nobody ever wrote to has nothing to show, and says so by being
    /// empty rather than by listing empty lanes.
    #[test]
    fn an_agent_with_no_history_has_an_empty_page() {
        let page = dossier("scout", &[], &[], &[]);
        assert!(page.is_empty());
    }

    // ---- the pair lane (D99) ---------------------------------------------

    fn from_user(text: &str) -> Message {
        user(&format!(
            "{}\n{text}",
            crate::tool::agent::DM_FROM_USER_MARKER
        ))
    }

    fn pair_texts(history: &[Message]) -> Vec<String> {
        pair_lane("scout", history, &[])
            .into_iter()
            .map(|item| item.post.text)
            .collect()
    }

    /// The `@agent` view is the user's lane and nothing else. Everything the
    /// old flat view mixed into it — the task the instance was created with,
    /// main's instructions, a room relay, another agent's mail, a chase, the
    /// task reminder — belongs to a lane that is not this one.
    #[test]
    fn the_pair_lane_keeps_the_user_and_drops_everybody_else() {
        let history = vec![
            user("map the parser module"),
            assistant(vec![text("on it")]),
            user(
                format!(
                    "[#build msg #4] qa: the suite is red\n{}\nwhat did you find?",
                    crate::tool::agent::DM_FROM_USER_MARKER
                )
                .as_str(),
            ),
            assistant(vec![text("a missing case")]),
            user("[follow-up instruction] also check the lexer"),
            assistant(vec![text("checked the lexer")]),
            user("[follow-up 2/3] still waiting"),
            user("[message from @qa]\nthe suite is red"),
            user(&format!("{} something", crate::query::TASK_REMINDER_MARKER)),
        ];
        assert_eq!(
            pair_texts(&history),
            vec!["what did you find?", "a missing case"],
            "the spawn prompt, the room relay, main's instruction, the chase, \
             the mail and the reminder all live on the page"
        );
    }

    /// Attribution is the perspective's own rule: a turn attaches to the
    /// counterpart it last heard from. So a reply main drew stays out of the
    /// user's lane and a reply the user drew stays in — the same walk answers
    /// both, which is why they cannot disagree.
    #[test]
    fn a_reply_belongs_to_whoever_drew_it() {
        let history = vec![
            from_user("what did you find?"),
            assistant(vec![text("for you")]),
            user("look again"),
            assistant(vec![text("for main")]),
            from_user("and now?"),
            assistant(vec![text("for you again")]),
        ];
        assert_eq!(
            pair_texts(&history),
            vec!["what did you find?", "for you", "and now?", "for you again"]
        );
    }

    /// The agent's work rides with the turn it belongs to (the protagonist
    /// rule), and the walk keeps the call itself so the pair view can hand it
    /// to the console's collapse classifier.
    #[test]
    fn the_pair_lane_carries_the_work_of_its_own_turns() {
        let history = vec![
            from_user("find the leak"),
            assistant(vec![
                text("looking"),
                tool("Grep", serde_json::json!({"pattern": "leak"})),
            ]),
        ];
        let lane = pair_lane("scout", &history, &[]);
        let work: Vec<&Post> = lane
            .iter()
            .map(|item| &item.post)
            .filter(|p| p.kind == PostKind::Process)
            .collect();
        assert_eq!(work.len(), 1, "{lane:?}", lane = pair_texts(&history));
        assert!(
            matches!(
                lane.iter().find_map(|item| item.work.as_ref()),
                Some(Work::Tool { name, .. }) if name == "Grep"
            ),
            "the call survives the projection, not only the line it printed"
        );
    }

    /// Contiguity is measured in the *full* walk, so anything that stood
    /// between two of the agent's rows breaks the run even where this lane
    /// never shows it. That is what keeps a replay append-only: every
    /// continuation is triggered by something, and that something is a break.
    #[test]
    fn a_run_breaks_on_what_the_lane_does_not_show() {
        let history = vec![
            from_user("go"),
            assistant(vec![text("first")]),
            user("[follow-up 2/3] still waiting"),
            assistant(vec![text("second")]),
        ];
        let lane = pair_lane("scout", &history, &[]);
        let contiguous: Vec<bool> = lane.iter().map(|item| item.contiguous).collect();
        assert_eq!(
            contiguous,
            vec![false, true, false],
            "the chase is invisible here and still ends the run"
        );
    }
}
