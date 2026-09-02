//! The serial room (ADR-0025): a post must follow everything its author could
//! have seen. Two ledgers are derived at the moment of the call and compared —
//! the room's own posts against how far the caller has read — and a post
//! written behind the room's head is handed back with what it missed instead
//! of landing.
//!
//! Both ledgers are folds of journals, so nothing is stored beside them and a
//! restart re-derives exactly what the process before it had. How far the
//! caller has read is the cursor the room keeps under its name (ADR-0034 §2),
//! which is the one fact "seen" comes from now that a post is written once and
//! copied nowhere. Both ledgers are read out of the room itself, so a caller
//! that has yet to hold a seat is judged by the same rule as one that does.
//!
//! "Could have seen" is the whole of the rule, so a post that landed before
//! the caller's session existed is not counted: it was never anybody's to read
//! before there was a session to read it, and no author can be behind on it.
//! What landed afterwards and was not read is what bounces.

use bingo_sdk::{
    ContentPart, Item, ItemBody, ItemId, SessionState, SessionSummary, ToolContext, ToolOutput,
};

use crate::{names, watch};

/// The plugin whose journal a cursor is read out of. This crate may not import
/// that one (ADR-0001), so the three names here are the whole of the contract
/// and the payload is read as data: a shape this does not recognise says the
/// caller has read nothing rather than guessing.
const ROOMS: &str = "bingo.rooms";
/// The kind one seat's cursor is published under, before its name.
const CURSOR: &str = "cursor:";
/// The post that payload names: the last one the seat has read.
const POST: &str = "post";

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
    let read = read(&there.snapshot, &awaited, cursor(&there.snapshot, speaker));
    let quoted = quoted(before(&here.snapshot, &cx.item), &title);
    match verdict(awaited, read.max(quoted)) {
        Verdict::Land => None,
        Verdict::Behind { seen, missed } => Some(ToolOutput::error(said(&title, seen, &missed))),
    }
}

/// The posts of a room the caller was there to hear, each with where the room
/// holds it: everyone else's, from the moment its own session opened.
fn awaited(room: &SessionState, caller: &SessionState, speaker: &str) -> Vec<(usize, Post)> {
    room.items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item.completed_at.unwrap_or(item.started_at) >= caller.summary.created_at
        })
        .filter_map(|(at, item)| Some((at, post_of(item)?)))
        .filter(|(_, post)| post.author != speaker)
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

/// How far the caller's cursor stands: the posts it awaited that the room
/// holds at or before the one the cursor names (ADR-0034 §5). A seat with no
/// cursor, or one the room does not hold, has read nothing.
fn read(room: &SessionState, awaited: &[(usize, Post)], cursor: Option<ItemId>) -> usize {
    let Some(head) = cursor.and_then(|id| room.items.iter().position(|item| item.id == id)) else {
        return 0;
    };
    awaited.iter().filter(|(at, _)| *at <= head).count()
}

/// Where one member has read this room up to, as the rooms plugin publishes it
/// in the room's own journal: a register per seat, keyed in one spelling of the
/// name because a room compares names in any case.
fn cursor(room: &SessionState, member: &str) -> Option<ItemId> {
    let published = room
        .extensions
        .get(ROOMS)?
        .get(&format!("{CURSOR}{}", member.to_lowercase()))?;
    Some(ItemId::from_raw(published.get(POST)?.as_str()?))
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

fn verdict(awaited: Vec<(usize, Post)>, seen: usize) -> Verdict {
    if seen >= awaited.len() {
        return Verdict::Land;
    }
    Verdict::Behind {
        seen,
        missed: awaited[seen..]
            .iter()
            .map(|(_, post)| post.clone())
            .collect(),
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

    /// A ledger of posts, as `awaited` hands one back: in the order the room
    /// holds them, from the top of it.
    fn ledger(posts: &[Post]) -> Vec<(usize, Post)> {
        posts.iter().cloned().enumerate().collect()
    }

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
        let head = [post("scout", "the build is green")];
        assert_eq!(verdict(ledger(&head), 1), Verdict::Land);
        assert_eq!(verdict(Vec::new(), 0), Verdict::Land);
        assert_eq!(
            verdict(ledger(&head), 0),
            Verdict::Behind {
                seen: 0,
                missed: head.to_vec()
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
            [(2, post("scout", "yours to read"))],
            "and the room still says where it holds it"
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
            [(0, post(names::PARENT, "unsigned"))]
        );
    }

    /// The whole of ADR-0034 §5: the cursor the room keeps under the caller's
    /// name says how much of what it awaited it has read, and nothing else does.
    #[test]
    fn the_caller_s_cursor_into_the_room_is_the_count_of_what_it_has_read() {
        let room = journal(
            "#design",
            0,
            vec![
                heard("the build is green", None, Some("scout"), 1),
                heard("mine", None, Some("builder"), 2),
                heard("and the tests pass", None, Some("scout"), 3),
            ],
        );
        let awaited = awaited(&room, &journal("builder", 0, Vec::new()), "builder");
        assert_eq!(awaited.len(), 2, "its own post is not one it awaits");

        let at = |n: usize| Some(room.items[n].id.clone());
        assert_eq!(read(&room, &awaited, None), 0, "a seat with no cursor");
        assert_eq!(read(&room, &awaited, at(0)), 1);
        assert_eq!(read(&room, &awaited, at(1)), 1, "its own post moved it");
        assert_eq!(read(&room, &awaited, at(2)), 2);
        assert_eq!(
            read(&room, &awaited, Some(ItemId::from_raw("itm_gone"))),
            0,
            "a cursor the room does not hold has read nothing"
        );
    }

    /// The cursor as the rooms plugin publishes it, read back by hand: the
    /// contract between the two crates, written down (ADR-0034 §2).
    #[test]
    fn a_cursor_is_a_post_id_under_the_rooms_plugin_s_own_kind() {
        let mut room = journal("#design", 0, Vec::new());
        assert_eq!(cursor(&room, "scout"), None);

        room.extensions.insert(
            ROOMS.into(),
            [("cursor:scout".to_string(), json!({ "post": "itm_7" }))]
                .into_iter()
                .collect(),
        );
        assert_eq!(cursor(&room, "scout"), Some(ItemId::from_raw("itm_7")));
        assert_eq!(
            cursor(&room, "Scout"),
            Some(ItemId::from_raw("itm_7")),
            "a room compares names in any case"
        );
        assert_eq!(cursor(&room, "builder"), None, "one seat is not another");
    }

    /// The cut: what a barrier absorbed after the model spoke was not what it
    /// wrote against, so a bounce quoted after it is not counted as read.
    #[test]
    fn only_what_was_journaled_before_the_calling_item_is_read() {
        let missed = [post("builder", "mind this")];
        let call = result("the call this tool is running under");
        let cut = call.id.clone();
        let caller = journal("scout", 0, vec![call, result(&said("#design", 0, &missed))]);
        assert_eq!(quoted(before(&caller, &cut), "#design"), 0);
        assert_eq!(
            quoted(before(&caller, &ItemId::from_raw("itm_gone")), "#design"),
            1,
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

    /// A bounce is a reading of the room through the tool-result lane, and the
    /// cursor does not move until the next turn reads it, so the quote is what
    /// unlocks the very next attempt.
    #[test]
    fn a_journaled_bounce_counts_as_read_and_unlocks_the_next_attempt() {
        let missed = [post("builder", "mind this")];
        let caller = journal("scout", 0, vec![result(&said("#design", 0, &missed))]);
        let seen = quoted(before(&caller, &ItemId::from_raw("itm_call")), "#design");
        assert_eq!(seen, 1);
        assert_eq!(verdict(ledger(&missed), seen), Verdict::Land);
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
