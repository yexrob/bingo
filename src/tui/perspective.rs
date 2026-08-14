//! The perspective page's projection (D96): one agent's communications, split
//! into the threads it actually had.
//!
//! **What this is.** For any agent X, a read-only dossier: X's direct
//! conversations grouped by counterpart, the rooms X is in, the intake X was
//! handed, and a merged timeline of everything. It is the audit layer the
//! privacy stance names — X's DM with somebody who is not the user is visible
//! *here*, and only here, while the user's own `@X` pair view stays pure.
//!
//! **Why the split is work rather than a filter.** [`crate::tui::buffer::dm_posts`]
//! does not filter by counterpart at all: it renders the whole of X's history
//! with every user-role message attributed to the user, because a pair view has
//! two parties and the bubble already says which one spoke. A perspective page
//! has as many parties as ever wrote to X, so it has to recover the sender —
//! and the sender is not a field. `InboxItem::Direct` carries a real `from`,
//! but `absorb_inbox` renders it into one flat prompt string and the name is
//! gone: what survives is a set of literal markers, which
//! [`crate::tui::buffer::line_source`] is the single parser for.
//!
//! **What the markers can and cannot say** (the D96 attribution inventory):
//!
//! | Shape | Composed at | Attributed to |
//! |---|---|---|
//! | `[DM from user]` heading a line | `tool::agent::direct_text` | the user |
//! | `[Message from user, sent while you were working]` block | `steer::SteerItem::block_text` | the user |
//! | `[follow-up instruction] …` | `direct_text`, batched | the hub |
//! | unmarked prose | `direct_text`, single | the hub — it is the one sender left unmarked |
//! | `[#room msg #N] who: …` | `absorb_inbox` | the room (timeline only; the room's own log is authoritative) |
//! | `[follow-up N/M] …` | `absorb_inbox` | intake — a chase, nobody wrote it |
//! | `[SYSTEM NOTIFICATION - TASK REMINDER]` | `query::maybe_inject_task_reminder` | intake |
//! | `<task-notifications>` | `query` | intake |
//! | the first user message | the `Agent` tool's prompt | intake — the task that created X |
//! | interrupt / compaction / stop-hook / max-tokens | `query`, `compact` | nobody: timeline only |
//!
//! **The one thing the domain cannot express today.** Every production caller of
//! `AgentRegistry::deliver` passes either `main` or `user`, and `SendMessage` —
//! which hardcodes `main` — is assembled for depth 0 only. So agent→agent
//! *direct* messages do not exist yet; agents reach each other through rooms.
//! The counterpart lane is keyed by name rather than by an enum precisely so
//! that the day `deliver` carries a real sender, this projection needs no
//! change to show it.

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
    /// A pair conversation with one counterpart (`user`, `main`, and one day
    /// another agent).
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
enum Target {
    /// A counterpart's DM lane. Also becomes the *active* lane, which is what
    /// X's following turns attach to.
    Dm(String),
    Intake,
    /// Real, but attributable to nobody: it lands in the timeline and nowhere
    /// else, which is the rule that keeps a thread honest.
    TimelineOnly,
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
        || trimmed.starts_with("<channel-messages>")
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
/// task is intake.
fn split_user_text(text: &str, at: u64, first: bool) -> Vec<(Target, Post)> {
    let mut out: Vec<(Target, Post)> = Vec::new();
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

    // The hub is the sender `direct_text` leaves unmarked, so prose belongs to
    // it until a marker says otherwise.
    let mut current = Target::Dm(HUB_NAME.to_string());
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
            Some(LineSource::HubBatched { text }) => {
                flush(&mut plain, &current, &mut out);
                current = Target::Dm(HUB_NAME.to_string());
                out.push((current.clone(), said(HUB_NAME, at, text)));
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
    // Which lane X's own turns belong to: the counterpart it last heard from.
    // Where interleaving makes exact reply-attribution impossible this is a
    // best effort by construction — the timeline is the lane that is complete.
    let mut active: Option<String> = None;
    let mut seen_user_text = false;

    let file = |target: &Target,
                post: Post,
                dms: &mut BTreeMap<String, Vec<Post>>,
                intake: &mut Vec<Post>| {
        match target {
            Target::Dm(name) => dms.entry(name.clone()).or_default().push(post),
            Target::Intake => intake.push(post),
            Target::TimelineOnly => {}
        }
    };

    for (i, msg) in history.iter().enumerate() {
        let at = stamps.get(i).copied().unwrap_or(0);
        for block in &msg.content {
            match (msg.role, block) {
                (ApiRole::User, ContentBlock::Text { text }) => {
                    let first = !seen_user_text;
                    seen_user_text = true;
                    for (target, post) in split_user_text(text, at, first) {
                        timeline.push(post.clone());
                        if let Target::Dm(name) = &target {
                            active = Some(name.clone());
                        }
                        file(&target, post, &mut dms, &mut intake);
                    }
                }
                (ApiRole::Assistant, ContentBlock::Text { text }) => {
                    let post = Post {
                        from: agent.to_string(),
                        you: false,
                        at,
                        text: text.clone(),
                        kind: PostKind::Said,
                    };
                    timeline.push(post.clone());
                    if let Some(name) = &active {
                        dms.entry(name.clone()).or_default().push(post);
                    }
                }
                (ApiRole::Assistant, ContentBlock::ToolUse { name, input, .. }) => {
                    let post = Post {
                        from: agent.to_string(),
                        you: false,
                        at: 0,
                        text: tool_call_line(name, input),
                        kind: PostKind::Process,
                    };
                    timeline.push(post.clone());
                    if let Some(lane) = &active {
                        dms.entry(lane.clone()).or_default().push(post);
                    }
                    // A notice is X speaking to the user, in the one tool that
                    // can. It is a message in the user's lane as well as a step
                    // of the work in the lane X was working in.
                    if name == "notify_user"
                        && let Some(text) = input.get("text").and_then(|t| t.as_str())
                    {
                        dms.entry(USER_NAME.to_string()).or_default().push(said(
                            agent,
                            at,
                            text.to_string(),
                        ));
                    }
                }
                (ApiRole::Assistant, ContentBlock::Thinking { .. }) => {
                    let post = Post {
                        from: agent.to_string(),
                        you: false,
                        at: 0,
                        text: THINKING_ROW.to_string(),
                        kind: PostKind::Process,
                    };
                    timeline.push(post.clone());
                    if let Some(lane) = &active {
                        dms.entry(lane.clone()).or_default().push(post);
                    }
                }
                _ => {}
            }
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

    /// A notice is X speaking to the user: a message in their lane, and a step
    /// of the work in the lane X was working in.
    #[test]
    fn a_notice_is_a_message_in_the_user_s_lane() {
        let history = vec![
            user("task"),
            user("go on"),
            assistant(vec![tool(
                "notify_user",
                serde_json::json!({"text": "the migration finished", "level": "info"}),
            )]),
        ];
        let page = dossier("scout", &history, &[1, 2, 3], &[]);
        let human = lane_of(&page, &LaneId::Dm(USER_NAME.to_string())).expect("a user lane");
        assert_eq!(said_texts(human), vec!["the migration finished"]);
        let hub = lane_of(&page, &LaneId::Dm(HUB_NAME.to_string())).expect("a hub lane");
        assert!(
            hub.posts.iter().any(|p| p.kind == PostKind::Process),
            "the call is still a step of the work in the lane it happened in"
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
}
