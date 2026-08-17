//! The attribution projection (D96): who said what, recovered from one agent's
//! own history.
//!
//! **What this is.** For any participant X, a walk over X's transcript that
//! files each piece of it under the counterpart it belongs to — the user, main,
//! a room relay, or the intake X was handed. Nothing above it groups any more:
//! the observation page that did (D96/D100) retired with v4's table in D108,
//! and what is left is the fact its lanes were built from.
//!
//! **Why the split is work rather than a filter.** The sender is not a field.
//! `InboxItem::Direct` carries a real `from`, but `absorb_inbox` renders the
//! batch into one flat prompt string and the name is gone: what survives is a
//! set of literal markers, which [`crate::tui::buffer::line_source`] is the
//! single parser for. Recovering who said what is therefore a walk, and [`walk`]
//! is it.
//!
//! **And the walk has two readers.** The away page (v6, [`crate::tui::conv`])
//! keeps every lane the walk files — it is one agent's whole record, which is
//! what its page shows — and [`pair_lane`] keeps exactly one, the user's,
//! which is what the Said-unread accounting measures (D99). Neither can disagree
//! with the other about attribution, because there is one walk.
//!
//! **And two protagonists (D100).** Main has a record too, its own session
//! transcript, and the unmarked default *flips*: in a subagent's history
//! unmarked user-role prose is main speaking, because main is the sender
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
//! | unmarked prose | `direct_text`, single | the default: main in a subagent's record, the user in main's |
//! | a `tool_result` block | `query::tool_result_block` | nobody — it is the answer to the call above it, and rides on that row ([`Work::Tool`]) |
//! | `[#room msg #N] who: …` | `absorb_inbox` | the room (recorded, attributed to nobody; the room's own log is authoritative) |
//! | `[follow-up N/M] …` | `absorb_inbox` | intake — a chase, nobody wrote it |
//! | `[SYSTEM NOTIFICATION - TASK REMINDER]` | `query::maybe_inject_task_reminder` | intake |
//! | `<task-notifications>` | `query` | intake |
//! | the first user message | the `Agent` tool's prompt | intake — the task that created X (subagents only: main was not spawned) |
//! | interrupt / compaction / stop-hook / max-tokens | `query`, `compact` | nobody: recorded and unattributed |
//!
//! Duration is the one fact neither half of a page can recover: it never enters
//! the record. A rebuilt row shows none rather than a fabricated one.
//!
//! **The one thing the domain cannot express today.** `SendMessage` is
//! assembled at every depth since D98, but `check_target` narrows by caller:
//! main reaches any instance, a subagent reaches `main` and the rooms it is in.
//! So agent→agent *direct* messages still do not exist; agents reach each other
//! through rooms. [`Target::Dm`] is keyed by name rather than by an enum
//! precisely so that the day the addressing rules open, this projection needs no
//! change to show it.
use crate::agents::ToolAnswer;
use crate::api::types::{ContentBlock, Message, Role as ApiRole};
use crate::channels::{MAIN_NAME, USER_NAME};
use crate::tui::buffer::{LineSource, Post, PostKind, THINKING_ROW, line_source, tool_call_line};

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
/// The zoomed view (D105) renders X's work the way the console renders main's
/// — collapsed activity groups — and the classifier that builds those groups
/// reads the call, not the sentence it printed.
///
/// D130 gave both variants the payload the console's live path gets for free
/// from `UiEvent::ToolDone` and the reasoning stream. Without it an agent's page
/// showed the *actions* and never the *answers*: a failed call was drawn as a
/// finished one, and a page of reasoning folded to the bare word "Thinking".
#[derive(Debug, Clone)]
pub(crate) enum Work {
    Tool {
        name: String,
        input: serde_json::Value,
        /// The matching `tool_result`, absent only when the run was cut short
        /// before one was recorded. The same [`ToolAnswer`] a running turn
        /// reports (D132) — one type, so a page cannot draw the two halves of
        /// itself differently.
        result: Option<ToolAnswer>,
    },
    Thinking {
        text: String,
    },
}

/// A `tool_result` block's text, as the model received it.
///
/// Two shapes reach the history (`query::tool_result_block`): a plain string,
/// which is almost every call, and an array of protocol blocks when the result
/// carried images. Only the text of the second is a row's business — the base64
/// belongs to the image path, not to a fold.
fn result_output(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

/// Whose record is being walked, and therefore what its unmarked shapes mean.
///
/// The markers are the same for everybody; the *defaults* are not. A subagent's
/// history is written by `absorb_inbox`, which leaves exactly one sender —
/// main — unmarked, and opens with the prompt the `Agent` tool spawned it
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
    /// A subagent: main is its unmarked voice, and its history opens with the
    /// task it was dispatched with.
    fn agent(name: &'a str) -> Self {
        Self {
            name,
            default: MAIN_NAME,
            spawned: true,
        }
    }

    /// Main, over its own session transcript.
    fn main() -> Self {
        Self {
            name: MAIN_NAME,
            default: USER_NAME,
            spawned: false,
        }
    }

    /// Which one a name is. The main agent's member name is reserved
    /// (`channels::MAIN_NAME`), so no instance can answer to it and this cannot
    /// mistake a teammate for the console.
    pub(crate) fn of(name: &'a str) -> Self {
        if name == MAIN_NAME {
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
/// **The single recognition walk (D99).** The zoomed view keeps every item this
/// files; the user's `@X` pair lane keeps the one lane it is in
/// ([`pair_lane`]). Both therefore agree about who said what by construction,
/// rather than by two parsers that happen to read the same markers today.
///
/// Every row carries its message's stamp, work rows included (D130). They used
/// to carry none — a process row has no send time of its own — but the page
/// builder takes a run's clock from its *first* row, and a turn that opens with
/// reasoning opens with a work row: the whole block then rendered stamped zero,
/// which [`crate::tui::buffer::stamp`] draws as no clock at all.
pub(crate) fn walk(who: Protagonist<'_>, history: &[Message], stamps: &[u64]) -> Vec<Filed> {
    let agent = who.name;
    let mut out: Vec<Filed> = Vec::new();
    // Which lane X's own turns belong to: the counterpart it last heard from.
    // Where interleaving makes exact reply-attribution impossible this is a
    // best effort by construction — the timeline is the lane that is complete.
    let mut active: Option<String> = None;
    let mut seen_user_text = false;
    // Results are collected before the walk rather than during it: a call and
    // its answer sit in two different messages, and a forward pass would have to
    // patch a row it already emitted. One pre-pass keeps the walk a walk.
    let mut results: std::collections::HashMap<&str, ToolAnswer> = std::collections::HashMap::new();
    for msg in history {
        for block in &msg.content {
            if let ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = block
            {
                results.insert(
                    tool_use_id.as_str(),
                    ToolAnswer {
                        output: result_output(content),
                        is_error: *is_error,
                    },
                );
            }
        }
    }
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
                (ApiRole::Assistant, ContentBlock::ToolUse { id, name, input }) => out.push(turn(
                    &active,
                    Post {
                        from: agent.to_string(),
                        you: false,
                        at,
                        text: tool_call_line(name, input),
                        kind: PostKind::Process,
                    },
                    Some(Work::Tool {
                        name: name.clone(),
                        input: input.clone(),
                        result: results.get(id.as_str()).cloned(),
                    }),
                )),
                (ApiRole::Assistant, ContentBlock::Thinking { thinking, .. }) => out.push(turn(
                    &active,
                    Post {
                        from: agent.to_string(),
                        you: false,
                        at,
                        text: THINKING_ROW.to_string(),
                        kind: PostKind::Process,
                    },
                    Some(Work::Thinking {
                        text: thinking.clone(),
                    }),
                )),
                _ => {}
            }
        }
    }
    out
}

/// The user's side of an agent's record: what the user said, what X answered
/// them, and the work X did for those turns (D99).
///
/// Everything else — main's instructions, room relays, `[message from @X]`
/// mail, chases, reminders, and the task that created the instance — belongs to
/// a lane that is not this one, and lives on the agent's own page
/// ([`crate::tui::conv`], v6).
///
/// D99 carried a `contiguous` flag out of here so the pair view could merge X's
/// consecutive rows across the items this filter drops. D105 retired that
/// reader: the zoom folds over the *full* walk, where whatever ended a run is a
/// row you can see, so the break needs no flag to survive the filter.
pub(crate) fn pair_lane(agent: &str, history: &[Message], stamps: &[u64]) -> Vec<Post> {
    walk(Protagonist::of(agent), history, stamps)
        .into_iter()
        .filter(|filed| matches!(&filed.target, Target::Dm(name) if name == USER_NAME))
        .map(|filed| filed.post)
        .collect()
}

/// Scaffolding that no counterpart wrote: the runtime talking to itself.
///
/// These are recognised so they cannot fall through to the unmarked-prose rule
/// and be misfiled as main speaking — the failure mode that would put words
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
/// main speaking, which is nearly true and still wrong — it is the task, and the
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
    // main speaking. The lines inside wear their own markers, so unwrapping
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
    // marker says otherwise: main in a subagent's record (it is the sender
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
            Some(LineSource::MainBatched { text }) => {
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Everything the walk filed under one counterpart, said-only. The dossier
    /// used to build these groups eagerly and hand the tests a lane; the walk
    /// is where the grouping was always decided, so the tests read it there.
    fn said_to<'a>(filed: &'a [Filed], target: &Target) -> Vec<&'a str> {
        filed
            .iter()
            .filter(|item| &item.target == target && item.post.kind == PostKind::Said)
            .map(|item| item.post.text.as_str())
            .collect()
    }

    /// Everything the walk filed under one counterpart, whatever its kind —
    /// intake rows are notes rather than speech, so they are read this way.
    fn filed_to<'a>(filed: &'a [Filed], target: &Target) -> Vec<&'a str> {
        filed
            .iter()
            .filter(|item| &item.target == target)
            .map(|item| item.post.text.as_str())
            .collect()
    }

    fn dm(who: &str) -> Target {
        Target::Dm(who.to_string())
    }

    fn walked(agent: &str, history: &[Message], stamps: &[u64]) -> Vec<Filed> {
        walk(Protagonist::of(agent), history, stamps)
    }

    /// The attribution catalog, pinned: every shape the runtime can put in an
    /// agent's history, filed where it belongs.
    ///
    /// Rewritten for D108 from `a_page_groups_every_counterpart_the_markers_
    /// can_name`. The observation page retired with v4's table and `dossier`
    /// went with it, but the claim was never the page's — it is the walk's
    /// attribution, which the zoom (D105) and the Said-unread accounting both
    /// read. Every marker it asserted is asserted here, against the same
    /// history, one layer down.
    #[test]
    fn the_walk_names_every_counterpart_the_markers_can_name() {
        let history = vec![
            // The task that created it: unmarked prose, and still not main
            // making conversation.
            user("map the parser module"),
            assistant(vec![text("on it")]),
            // Main, single and therefore unmarked.
            user("also check the lexer"),
            assistant(vec![text("will do")]),
            // The user, marked.
            user(&format!(
                "{}\nare you nearly done?",
                crate::tool::agent::DM_FROM_USER_MARKER
            )),
            assistant(vec![text("nearly")]),
            // A batch: main labelled, the user marked, a room relayed, a
            // chase, all in one absorbed prompt.
            user(&format!(
                "[follow-up instruction] and the printer\n{}\nping\n[#parser msg #3] scout: found it\n[follow-up 1/3] Main sent you message #2 and has had no reply",
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
        let filed = walked("scout", &history, &stamps);

        assert_eq!(
            said_to(&filed, &dm(MAIN_NAME)),
            vec!["also check the lexer", "will do", "and the printer"],
            "main's unmarked single and its labelled batch line, plus the reply it drew"
        );

        assert_eq!(
            said_to(&filed, &dm(USER_NAME)),
            vec!["are you nearly done?", "nearly", "ping"],
            "only what the user actually wrote, plus the reply it drew"
        );

        assert_eq!(
            filed_to(&filed, &Target::Intake),
            vec![
                "map the parser module",
                "follow-up · waiting for a reply",
                "system note · task tools"
            ],
            "the task that created it, the chase, and the reminder"
        );

        // Attributable to nobody: the reply the spawn task drew (intake is not
        // a counterpart, so nothing was said back to it), the room relay (the
        // room's own log is the authoritative copy) and the interrupt.
        assert_eq!(
            filed_to(&filed, &Target::TimelineOnly),
            vec![
                "on it",
                "#parser · scout: found it",
                crate::query::INTERRUPT_MARKER
            ],
            "recorded, and attributed to nobody"
        );
    }

    /// The interrupt marker, the compaction summary and the rest of the
    /// runtime's own talk are attributable to nobody — a thread that showed
    /// them would be putting words in somebody's mouth.
    ///
    /// Rewritten for D108 from `scaffolding_nobody_wrote_stays_out_of_every_
    /// thread`: the lanes it walked were the dossier's, and `Target::
    /// TimelineOnly` is the fact underneath them.
    #[test]
    fn scaffolding_nobody_wrote_is_attributed_to_nobody() {
        let history = vec![
            user("start"),
            user(crate::query::INTERRUPT_MARKER),
            user(&format!(
                "{}\nearlier, they discussed the parser",
                crate::transcript::COMPACT_SUMMARY_PREFIX
            )),
            user(crate::query::MAX_TOKENS_RESUME_PROMPT),
        ];
        let filed = walked("scout", &history, &[1, 2, 3, 4]);
        assert_eq!(filed.len(), 4, "the record keeps all four, complete");
        for item in &filed {
            if matches!(item.target, Target::TimelineOnly) {
                continue;
            }
            assert!(
                !item.post.text.contains("interrupted by user")
                    && !item.post.text.contains("summary of the earlier")
                    && !item.post.text.contains("Output token limit"),
                "{:?} took the runtime's own words: {:?}",
                item.target,
                item.post.text
            );
        }
        assert_eq!(
            filed_to(&filed, &Target::TimelineOnly).len(),
            3,
            "the interrupt, the compaction and the token limit, all three"
        );
    }

    /// The protagonist rule: X's thinking and tool calls are filed with the
    /// counterpart it was working for, whoever that is.
    ///
    /// Rewritten for D108 from `a_thread_shows_the_agent_s_own_process`. The
    /// lane's `messages()` count went with `Lane`; what it was counting —
    /// process rows are work, not speech — is asserted here as the kinds the
    /// walk files, which is the fact the count was derived from.
    #[test]
    fn a_turn_carries_the_agents_own_process_to_its_counterpart() {
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
        let filed = walked("scout", &history, &[1, 2, 3]);
        let human: Vec<&Filed> = filed
            .iter()
            .filter(|item| item.target == dm(USER_NAME))
            .collect();
        let kinds: Vec<PostKind> = human.iter().map(|item| item.post.kind).collect();
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
            kinds.iter().filter(|k| **k == PostKind::Said).count(),
            2,
            "process rows are work, not messages"
        );
        assert!(
            human.iter().any(|item| item.post.text == THINKING_ROW),
            "the same collapsed reasoning row every reader shows"
        );
    }

    /// D98: an agent writing to main arrives under a marker that names it, and
    /// that is what lets main's own record file the message under the sender
    /// rather than dropping the whole inbox block in as one unattributed note.
    /// The envelope is scaffolding; what is inside it was said by someone.
    ///
    /// Rewritten for D108 from `a_direct_message_to_main_lands_in_its_sender_s_
    /// lane`: same history, same claim, read off the walk.
    #[test]
    fn a_direct_message_to_main_is_filed_under_its_sender() {
        let inbox = format!(
            "{}\n{}\n[#build msg #4] zoe: the tests pass\n{}",
            crate::query::MAIL_BLOCK_OPEN,
            crate::channels::format_agent_message("scout", "the migration is done"),
            crate::query::MAIL_BLOCK_CLOSE
        );
        let history = vec![user("run the release"), user(&inbox)];
        let filed = walked(MAIN_NAME, &history, &[1, 2]);
        assert_eq!(
            said_to(&filed, &dm("scout")),
            vec!["the migration is done"],
            "the mail is the sender's"
        );
        assert_eq!(
            filed_to(&filed, &Target::TimelineOnly),
            vec!["#build · zoe: the tests pass"],
            "and a room relay in the same block is still the room's, recorded once"
        );
    }

    /// Main's own record (D100). The unmarked default flips: the prose in it is
    /// the human at the keyboard, not main talking to itself. Nothing
    /// dispatched main either, so the first message is the first thing the user
    /// ever said — a message, not intake.
    ///
    /// Rewritten for D108 from `mains_page_reads_unmarked_prose_as_the_user`.
    /// The protagonist flip is `walk`'s, and this is where it is now asserted.
    #[test]
    fn mains_record_reads_unmarked_prose_as_the_user() {
        let history = vec![
            // The first thing typed into the console. In a subagent's record
            // this shape is the spawn task; here there is no spawn.
            user("ship the release"),
            assistant(vec![text("on it")]),
            // An agent wrote to main (D98), inside main's own inbox envelope.
            user(&format!(
                "{}\n{}\n[#build msg #4] zoe: the tests pass\n{}",
                crate::query::MAIL_BLOCK_OPEN,
                crate::channels::format_agent_message("scout", "the migration is done"),
                crate::query::MAIL_BLOCK_CLOSE
            )),
            assistant(vec![text("thanks")]),
            // Dispatch notifications are handed to main, not said to it.
            user("<task-notifications>\nscout finished\n</task-notifications>"),
        ];
        let filed = walked(MAIN_NAME, &history, &[10, 20, 30, 40, 50]);

        assert_eq!(
            said_to(&filed, &dm(USER_NAME)),
            vec!["ship the release", "on it"],
            "unmarked prose in main's record is the user, and the first line is not intake"
        );
        assert!(
            filed_to(&filed, &dm(MAIN_NAME)).is_empty(),
            "and main is never its own counterpart"
        );

        assert_eq!(
            said_to(&filed, &dm("scout")),
            vec!["the migration is done", "thanks"],
            "mail is filed under its sender, and the reply it drew with it"
        );

        assert_eq!(
            filed_to(&filed, &Target::Intake),
            vec!["system note · task notifications"],
            "the notifications are intake and nothing else is"
        );

        assert_eq!(
            filed_to(&filed, &Target::TimelineOnly),
            vec!["#build · zoe: the tests pass"],
            "a room relay in the same block stays the room's, recorded once"
        );
    }

    /// The flip is a property of the protagonist, not of the shapes: main's
    /// record still reads every marker the way a subagent's does, the legacy
    /// envelope included, and a subagent's record still files unmarked prose to
    /// main (which is what every test above this one asserts, unchanged).
    #[test]
    fn the_flipped_default_does_not_move_any_marker() {
        let history = vec![
            user("<channel-messages>\n[#build msg #2] zoe: still failing\n</channel-messages>"),
            user(&format!(
                "{}\nnot main",
                crate::tool::agent::DM_FROM_USER_MARKER
            )),
            user(&format!(
                "{}\nThe task tools haven't been used recently.",
                crate::query::TASK_REMINDER_MARKER
            )),
            user(crate::query::INTERRUPT_MARKER),
        ];
        let filed = walked(MAIN_NAME, &history, &[1, 2, 3, 4]);
        assert!(
            filed_to(&filed, &Target::TimelineOnly).contains(&"#build · zoe: still failing"),
            "the pre-D98 envelope is unwrapped on main's record too"
        );
        assert_eq!(
            said_to(&filed, &dm(USER_NAME)),
            vec!["not main"],
            "an explicit user marker files where it always did"
        );
        assert!(
            filed_to(&filed, &Target::Intake).contains(&"system note · task tools"),
            "the reminder is intake for main as well"
        );
        for item in &filed {
            if matches!(item.target, Target::TimelineOnly) {
                continue;
            }
            assert!(
                !item.post.text.contains("interrupted"),
                "{:?} took the runtime's own words",
                item.target
            );
        }
    }

    /// A history recorded before D98 wraps its inbox in `<channel-messages>`.
    /// The old envelope reads the same as the new one: its lines wear their
    /// own markers, and forgetting the shape would file the block as main
    /// speaking.
    #[test]
    fn the_pre_d98_envelope_is_still_read() {
        let inbox = "<channel-messages>\n[#build msg #2] zoe: still failing\n</channel-messages>";
        let history = vec![user("task"), user(inbox)];
        let filed = walked("scout", &history, &[1, 2]);
        assert_eq!(
            filed_to(&filed, &Target::TimelineOnly),
            vec!["#build · zoe: still failing"],
            "the relay inside is attributed, not misfiled as main prose"
        );
        assert!(
            filed_to(&filed, &dm(MAIN_NAME)).is_empty(),
            "and nothing in the legacy block reads as main speaking"
        );
    }

    /// Everything the walk files is in the record, once and in order — the
    /// property the dossier's timeline used to state by being a superset of the
    /// threads it split.
    ///
    /// Rewritten for D108 from `the_timeline_is_a_superset_of_the_threads_it_
    /// split`. There is one list now rather than a list and its projections, so
    /// the claim is that the targeted subsets partition it in order.
    #[test]
    fn the_record_holds_every_filed_post_once_and_in_order() {
        let history = vec![
            user("task"),
            user("main says hi"),
            assistant(vec![text("hello")]),
            user(&format!(
                "{}\nuser says hi",
                crate::tool::agent::DM_FROM_USER_MARKER
            )),
            assistant(vec![text("hi there")]),
        ];
        let filed = walked("scout", &history, &[1, 2, 3, 4, 5]);
        let whole: Vec<&str> = filed.iter().map(|i| i.post.text.as_str()).collect();
        for target in [dm(MAIN_NAME), dm(USER_NAME), Target::Intake] {
            let mut at = 0;
            for text in filed_to(&filed, &target) {
                let found = whole[at..].iter().position(|t| *t == text).map(|p| p + at);
                let Some(found) = found else {
                    panic!("{target:?} has {text:?}, the record does not");
                };
                at = found + 1;
            }
        }
        assert_eq!(
            whole.len(),
            filed_to(&filed, &dm(MAIN_NAME)).len()
                + filed_to(&filed, &dm(USER_NAME)).len()
                + filed_to(&filed, &Target::Intake).len()
                + filed_to(&filed, &Target::TimelineOnly).len(),
            "every post is filed exactly once"
        );
    }

    /// An agent nobody ever wrote to has nothing to show.
    #[test]
    fn an_agent_with_no_history_files_nothing() {
        assert!(walked("scout", &[], &[]).is_empty());
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
            .map(|item| item.text)
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
             the mail and the reminder all belong to another lane"
        );
    }

    /// Attribution is the walk's own rule: a turn attaches to the
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
    /// rule), and the walk keeps the **call** and not only the line it printed
    /// — which is what lets the zoom hand it to the console's collapse
    /// classifier (D105).
    ///
    /// Rewritten for D105: `pair_lane` used to carry the call out with the
    /// post, for the pair replay that retired. The claim it was making is a
    /// claim about the walk, so it is asserted where it lives.
    #[test]
    fn the_walk_carries_the_call_and_not_only_the_line() {
        let history = vec![
            from_user("find the leak"),
            assistant(vec![
                text("looking"),
                tool("Grep", serde_json::json!({"pattern": "leak"})),
            ]),
        ];
        let work = pair_lane("scout", &history, &[])
            .into_iter()
            .filter(|p| p.kind == PostKind::Process)
            .count();
        assert_eq!(work, 1, "the lane keeps its own turn's work");
        let filed = walk(Protagonist::of("scout"), &history, &[]);
        assert!(
            matches!(
                filed.iter().find_map(|item| item.work.as_ref()),
                Some(Work::Tool { name, .. }) if name == "Grep"
            ),
            "the call survives the projection, not only the line it printed"
        );
    }

    /// The pair lane drops what it is not about, and the walk keeps it — which
    /// is what makes the zoom's fold able to see the thing that ended a run.
    ///
    /// Rewritten for D105: the old assertion read the `contiguous` flag the
    /// lane carried so the pair replay could break a run across an item it
    /// could not show. The zoom folds over the whole walk, where that item is a
    /// row, so the flag retired and the fact it protected is asserted directly.
    #[test]
    fn a_run_breaks_on_what_the_lane_does_not_show() {
        let history = vec![
            from_user("go"),
            assistant(vec![text("first")]),
            user("[follow-up 2/3] still waiting"),
            assistant(vec![text("second")]),
        ];
        assert_eq!(
            pair_texts(&history),
            vec!["go", "first", "second"],
            "the chase is not the user's conversation and does not render here"
        );
        let filed = walk(Protagonist::of("scout"), &history, &[]);
        let between: Vec<(&Target, &str)> = filed
            .iter()
            .map(|item| (&item.target, item.post.text.as_str()))
            .collect();
        assert_eq!(
            between,
            vec![
                (&Target::Dm(USER_NAME.to_string()), "go"),
                (&Target::Dm(USER_NAME.to_string()), "first"),
                (&Target::Intake, "follow-up · waiting for a reply"),
                (&Target::Dm(USER_NAME.to_string()), "second"),
            ],
            "the chase stands between the two turns in the record the zoom reads"
        );
    }
}
