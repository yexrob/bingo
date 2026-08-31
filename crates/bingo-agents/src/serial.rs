//! The serial room (ADR-0025): a post must follow everything its author could
//! have seen. Two ledgers are derived at the moment of the call and compared —
//! the room's own posts against what the caller has read — and a post written
//! behind the room's head is handed back with what it missed instead of
//! landing.
//!
//! Both ledgers are folds of journals, so nothing is stored beside them and a
//! restart re-derives exactly what the process before it had. Neither reads
//! anything but `Origin` and journal order: what a room is remains the rooms
//! plugin's business.
//!
//! "Could have seen" is the whole of the rule, so a post that landed before
//! the caller's session existed is not counted: it was never fanned out to a
//! session that was not there, and no author can be behind on it. What landed
//! afterwards and was not read is what bounces — including a fan-out lost to a
//! process that was down, which the bounce itself repairs.

use bingo_sdk::{
    ContentPart, Item, ItemBody, ItemId, SessionState, SessionSummary, ToolContext, ToolOutput,
};

use crate::{names, watch};

/// One post, as a room's journal has it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Post {
    author: String,
    text: String,
}

/// What the room says about a post about to be written.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Verdict {
    /// Everything the caller could have read, it has read.
    Land,
    /// These landed since, in order, and the post was not written against
    /// them.
    Behind { seen: usize, missed: Vec<Post> },
}

/// The bounce a stale post gets, or nothing when it may land. A room this
/// process cannot read judges nobody: the discipline is only ever what the
/// journals say it is.
pub async fn bounce(cx: &ToolContext, room: &SessionSummary, speaker: &str) -> Option<ToolOutput> {
    let title = names::name_of(room).to_string();
    let there = watch::follow(&cx.host, &room.id).await.ok()?;
    let here = watch::follow(&cx.host, &cx.session).await.ok()?;
    let awaited = awaited(&there.snapshot, &here.snapshot, speaker);
    let seen = seen(&here.snapshot, &cx.item, &title);
    match verdict(awaited, seen) {
        Verdict::Land => None,
        Verdict::Behind { seen, missed } => Some(ToolOutput::error(said(&title, seen, &missed))),
    }
}

/// The posts of a room the caller was there to hear: everyone else's, from
/// the moment its own session opened.
fn awaited(room: &SessionState, caller: &SessionState, speaker: &str) -> Vec<Post> {
    room.items
        .iter()
        .filter(|item| item.completed_at.unwrap_or(item.started_at) >= caller.summary.created_at)
        .filter_map(post_of)
        .filter(|post| post.author != speaker)
        .collect()
}

/// What a room holds is what was said into it: a user item, and nothing else.
fn post_of(item: &Item) -> Option<Post> {
    let ItemBody::User { parts, origin } = &item.body else {
        return None;
    };
    Some(Post {
        author: origin
            .principal
            .clone()
            .unwrap_or_else(|| names::PARENT.to_string()),
        text: parts.iter().filter_map(ContentPart::as_text).collect(),
    })
}

/// How many of the room's posts the caller has read: the ones absorbed into
/// its own journal, or the ones a bounce already quoted at it, whichever is
/// the further on (ADR-0025 §3).
fn seen(caller: &SessionState, cut: &ItemId, room: &str) -> usize {
    let read = before(caller, cut);
    absorbed(read, room).max(quoted(read, room))
}

/// The caller's journal as the model that made this call saw it. What a
/// barrier absorbed after the model spoke is not what it wrote against, and
/// the item the call was issued from is where the two part.
fn before<'a>(caller: &'a SessionState, cut: &ItemId) -> &'a [Item] {
    match caller.items.iter().position(|item| &item.id == cut) {
        Some(at) => &caller.items[..at],
        None => &caller.items,
    }
}

/// The room's posts in the caller's own journal. A nudge carries no principal
/// and is nobody's post, so it never counts (ADR-0025 §3).
fn absorbed(items: &[Item], room: &str) -> usize {
    items
        .iter()
        .filter(|item| match &item.body {
            ItemBody::User { origin, .. } => {
                origin.conversation.as_deref() == Some(room) && origin.principal.is_some()
            }
            _ => false,
        })
        .count()
}

/// The furthest a bounce has already read this room out to the caller. A
/// journaled bounce is a reading of the room through the tool-result lane, so
/// it counts as seen and the very next attempt is unlocked.
fn quoted(items: &[Item], room: &str) -> usize {
    items
        .iter()
        .filter_map(|item| match &item.body {
            ItemBody::ToolCall {
                output: Some(output),
                ..
            } => head_of(room, &text_of(output)),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

fn text_of(output: &ToolOutput) -> String {
    output
        .parts
        .iter()
        .filter_map(ContentPart::as_text)
        .collect()
}

fn verdict(awaited: Vec<Post>, seen: usize) -> Verdict {
    if seen >= awaited.len() {
        return Verdict::Land;
    }
    Verdict::Behind {
        seen,
        missed: awaited[seen..].to_vec(),
    }
}

/// The first line of a bounce, which is also the ledger a later call reads
/// back out of it: one shape, written and parsed in one place.
fn ledger(room: &str, seen: usize, head: usize) -> String {
    format!("{room}: not sent — you had read {seen} of its {head} posts; these landed first:")
}

/// The head a bounce for this room recorded, or nothing when the text is not
/// one of this room's bounces.
fn head_of(room: &str, text: &str) -> Option<usize> {
    let opening = format!("{room}: not sent — you had read ");
    let rest = text.lines().next()?.strip_prefix(&opening)?;
    let (_, tail) = rest.split_once(" of its ")?;
    tail.split_once(' ')?.0.parse().ok()
}

/// What the caller is handed instead of a receipt: the posts it missed,
/// verbatim, and what to do about them.
fn said(room: &str, seen: usize, missed: &[Post]) -> String {
    let quotes: Vec<String> = missed
        .iter()
        .map(|post| format!("  {}: {}", post.author, post.text.trim()))
        .collect();
    format!(
        "{}\n\n{}\n\nYou have them now. Post again if what you wrote still applies.",
        ledger(room, seen, seen + missed.len()),
        quotes.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fleet, Recorder, summary, tool_context};
    use bingo_sdk::{ItemStatus, Origin, Tool, TurnId};
    use jiff::Timestamp;
    use serde_json::json;

    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(second).expect("a timestamp")
    }

    fn post(author: &str, text: &str) -> Post {
        Post {
            author: author.into(),
            text: text.into(),
        }
    }

    /// A session state with these items in its journal, opened at `second`.
    fn journal(name: &str, second: i64, items: Vec<Item>) -> SessionState {
        let mut summary = summary("ses_x", Some(name), None);
        summary.created_at = at(second);
        let mut state = SessionState::new(summary);
        state.items = items;
        state
    }

    fn item(body: ItemBody) -> Item {
        Item {
            id: ItemId::mint(),
            turn: Some(TurnId::from_raw("trn_1")),
            round: 0,
            status: ItemStatus::Completed,
            started_at: at(0),
            completed_at: Some(at(0)),
            intent: None,
            body,
            meta: Default::default(),
        }
    }

    /// Something said into a session: a room's post, a nudge, or a message.
    fn heard(text: &str, room: Option<&str>, who: Option<&str>, second: i64) -> Item {
        let mut item = item(ItemBody::User {
            parts: vec![ContentPart::text(text)],
            origin: Origin {
                surface: "room".into(),
                principal: who.map(str::to_string),
                conversation: room.map(str::to_string),
            },
        });
        item.completed_at = Some(at(second));
        item
    }

    fn result(text: &str) -> Item {
        item(ItemBody::ToolCall {
            call_id: "c1".into(),
            name: "SendMessage".into(),
            input: json!({}),
            output: Some(ToolOutput::error(text)),
            progress: None,
            duration_ms: None,
        })
    }

    #[test]
    fn a_caller_level_with_the_room_lands_and_one_behind_it_does_not() {
        let head = vec![post("scout", "the build is green")];
        assert_eq!(verdict(head.clone(), 1), Verdict::Land);
        assert_eq!(verdict(Vec::new(), 0), Verdict::Land);
        assert_eq!(
            verdict(head.clone(), 0),
            Verdict::Behind {
                seen: 0,
                missed: head
            }
        );
    }

    /// The caller's own posts are not something it can be behind on, and
    /// neither is what landed before its session existed.
    #[test]
    fn what_the_caller_could_have_heard_and_what_it_could_not() {
        let room = journal(
            "#design",
            0,
            vec![
                heard("before your time", None, Some("scout"), 1),
                heard("mine", None, Some("builder"), 5),
                heard("yours to read", None, Some("scout"), 6),
            ],
        );
        let caller = journal("builder", 5, Vec::new());
        assert_eq!(
            awaited(&room, &caller, "builder"),
            [post("scout", "yours to read")]
        );
    }

    /// A post nobody signed came from the session the room hangs under, and
    /// what a room does not say it does not hold.
    #[test]
    fn a_post_is_a_user_item_and_its_author_is_who_signed_it() {
        let room = journal(
            "#design",
            0,
            vec![
                heard("unsigned", None, None, 1),
                item(ItemBody::Assistant {
                    text: "a room answers nobody".into(),
                }),
            ],
        );
        let caller = journal("scout", 0, Vec::new());
        assert_eq!(
            awaited(&room, &caller, "scout"),
            [post(names::PARENT, "unsigned")]
        );
    }

    #[test]
    fn a_room_s_posts_count_and_a_nudge_never_does() {
        let items = [
            heard("a post", Some("#design"), Some("scout"), 1),
            heard("a nudge", Some("#design"), None, 2),
            heard("another room", Some("#standup"), Some("scout"), 3),
            heard("a direct message", None, Some("scout"), 4),
        ];
        assert_eq!(absorbed(&items, "#design"), 1);
    }

    /// The cut: what a barrier absorbed after the model spoke was not what it
    /// wrote against, so it is not counted as read.
    #[test]
    fn only_what_was_journaled_before_the_calling_item_is_read() {
        let call = result("the call this tool is running under");
        let cut = call.id.clone();
        let caller = journal(
            "scout",
            0,
            vec![
                heard("read before the call", Some("#design"), Some("builder"), 1),
                call,
                heard("absorbed after it", Some("#design"), Some("builder"), 2),
            ],
        );
        assert_eq!(seen(&caller, &cut, "#design"), 1);
        assert_eq!(
            seen(&caller, &ItemId::from_raw("itm_gone"), "#design"),
            2,
            "a cut the journal does not hold cuts nothing"
        );
    }

    #[test]
    fn a_bounce_quotes_the_missed_posts_and_reads_back_as_a_ledger() {
        let missed = [
            post("scout", "the build is green"),
            post("parent", "and I filed it"),
        ];
        let text = said("#design", 2, &missed);
        assert!(text.contains("  scout: the build is green"), "{text}");
        assert!(text.contains("  parent: and I filed it"), "{text}");
        assert_eq!(head_of("#design", &text), Some(4));
        assert_eq!(head_of("#standup", &text), None, "another room's bounce");
        assert_eq!(head_of("#design", "Posted to #design."), None);
    }

    #[test]
    fn a_journaled_bounce_counts_as_read_and_unlocks_the_next_attempt() {
        let missed = [post("builder", "mind this")];
        let caller = journal("scout", 0, vec![result(&said("#design", 0, &missed))]);
        let seen = seen(&caller, &ItemId::from_raw("itm_call"), "#design");
        assert_eq!(seen, 1);
        assert_eq!(verdict(missed.to_vec(), seen), Verdict::Land);
    }

    /// End to end through the tool, on a fleet whose room holds a post the
    /// caller never heard: the author is never behind on its own post, and
    /// the one that was not there is.
    #[tokio::test]
    async fn a_stale_post_bounces_and_lands_nothing() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let room = fleet.room(&root, "#design");
        let scout = fleet.child(&root, "scout");
        fleet.post(&room, "the build is green", Some("scout"));
        let host = Recorder::new(&fleet);

        let out = posted(&scout, host.clone()).await;
        assert!(!out.is_error, "the author is not behind on its own post");
        assert_eq!(host.delivered().len(), 1);

        let out = posted(&root, host.clone()).await;
        assert!(out.is_error, "the root never heard the scout's post");
        let text = out.parts[0].as_text().unwrap_or_default();
        assert!(text.contains("scout: the build is green"), "{text}");
        assert_eq!(host.delivered().len(), 1, "a bounced post never landed");
    }

    async fn posted(caller: &bingo_sdk::SessionId, host: std::sync::Arc<Recorder>) -> ToolOutput {
        crate::MessageTool
            .call(
                json!({ "to": "#design", "text": "stand-up in five" }),
                &tool_context(caller, host),
            )
            .await
            .expect("a post this crate can judge")
    }
}
