//! What a room's posts owe (ADR-0022 §1–2). `@name` opens a debt against that
//! member and the member's next post closes it — speaking is the answer, and
//! what was said is deliberately not judged. `@all` is one debt against the
//! room, closed by any other member.
//!
//! All of it is a fold of the room's own journal: the questions are already
//! there and so are the answers, so there is nothing to store beside them and
//! the next process re-derives exactly what this one had.

use bingo_sdk::{ContentPart, HostHandle, Item, ItemBody, ItemId, SessionId, SessionState};
use jiff::Timestamp;

use crate::name::{PARENT, same};
use crate::room;

/// The sigil that calls on the room rather than on anyone in it — which is
/// every member of it but whoever posted. A room that seats a member of this
/// name spends the word on the member: a real name is never shadowed.
const ALL: &str = "all";

/// Characters of a post that a nudge quotes back. Enough to tell two
/// questions apart, short enough for one line.
const HEAD: usize = 48;

/// Who a debt is against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owed {
    /// A named member: the one a nudge goes to.
    Member(String),
    /// `@all`: the room. Everyone in it is called on and the fan-out reads it
    /// so, but the sigil picked no member, so the chase picks none either.
    Room,
}

impl Owed {
    /// The member to chase, or nothing at all.
    pub fn chased(&self) -> Option<&str> {
        match self {
            Owed::Member(name) => Some(name),
            Owed::Room => None,
        }
    }

    /// How it reads in `/room` and on the card.
    pub fn said(&self) -> String {
        match self {
            Owed::Member(name) => name.clone(),
            Owed::Room => format!("@{ALL}"),
        }
    }
}

/// One `@` nobody has answered yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mention {
    pub owed_by: Owed,
    /// Who asked, as the post signed itself.
    pub asker: String,
    /// The post that opened it: what tells two debts apart, and what a nudge
    /// quotes.
    pub post: ItemId,
    pub at: Timestamp,
    pub head: String,
}

/// A post, as the fold reads one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Post {
    pub id: ItemId,
    /// A post nobody signed came from the session the room hangs under.
    pub author: String,
    pub at: Timestamp,
    pub text: String,
}

impl Post {
    /// The post a journal item is, or nothing for an item that is not one:
    /// what a room holds is what was said into it.
    pub fn of(item: &Item) -> Option<Post> {
        let ItemBody::User { parts, origin } = &item.body else {
            return None;
        };
        Some(Post {
            id: item.id.clone(),
            author: origin
                .principal
                .clone()
                .unwrap_or_else(|| PARENT.to_string()),
            at: item.completed_at.unwrap_or(item.started_at),
            text: parts.iter().filter_map(ContentPart::as_text).collect(),
        })
    }
}

/// Every debt still open after the last post, in the order they were opened.
/// Settle before open, so a post that answers and asks in one breath — `thanks
/// @scout — @reviewer what about x?` — closes its author's own debt and opens
/// the reviewer's.
pub fn mentions(posts: &[Post], members: &[String]) -> Vec<Mention> {
    let mut open: Vec<Mention> = Vec::new();
    for post in posts {
        settle(&mut open, &post.author, members);
        open.extend(opened(post, members));
    }
    open
}

/// What a post's author no longer owes. Their own debts close because they
/// spoke; the room's `@all` closes for any other member, and never for the one
/// who asked it.
fn settle(open: &mut Vec<Mention>, author: &str, members: &[String]) {
    open.retain(|mention| match &mention.owed_by {
        Owed::Member(name) => !same(name, author),
        Owed::Room => same(&mention.asker, author) || !is_member(author, members),
    });
}

/// The debts a post opens: every member it calls on but its own author — a
/// question to yourself is not one — and one against the room for `@all`.
fn opened(post: &Post, members: &[String]) -> Vec<Mention> {
    named(&post.text, members)
        .into_iter()
        .filter(|owed| owed.chased().is_none_or(|name| !same(name, &post.author)))
        .map(|owed| Mention {
            owed_by: owed,
            asker: post.author.clone(),
            post: post.id.clone(),
            at: post.at,
            head: head(&post.text),
        })
        .collect()
}

/// Who a post calls on: `@name` at a word boundary, matched case-insensitively
/// against the room's members. `mail@user` is an address rather than a call,
/// and a name nobody in the room has asks nothing of anybody. The delivery asks
/// this too (ADR-0029 §5) — one matcher, against the one roster.
pub(crate) fn named(text: &str, members: &[String]) -> Vec<Owed> {
    let chars: Vec<char> = text.chars().collect();
    let mut found: Vec<Owed> = Vec::new();
    for (i, c) in chars.iter().enumerate() {
        if *c != '@' || !calls(i, &chars) {
            continue;
        }
        let word: String = chars[i + 1..].iter().take_while(|c| in_name(**c)).collect();
        let Some(owed) = whom(word.trim_end_matches('-'), members) else {
            continue;
        };
        if !found.contains(&owed) {
            found.push(owed);
        }
    }
    found
}

/// Whether the `@` at `i` opens a name: a letter before it makes it an address.
fn calls(i: usize, chars: &[char]) -> bool {
    let Some(before) = i.checked_sub(1).and_then(|j| chars.get(j)) else {
        return true;
    };
    !(before.is_alphanumeric() || *before == '_')
}

/// Whether a character is still part of the name after an `@`.
fn in_name(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// Who the word after an `@` names: one member — spelled as the roster spells
/// it — the room, or nobody. The roster answers first, so a member actually
/// named `all` keeps its own name and the sigil is only what is left over.
fn whom(word: &str, members: &[String]) -> Option<Owed> {
    match members.iter().find(|member| same(member, word)) {
        Some(member) => Some(Owed::Member(member.clone())),
        None => same(word, ALL).then_some(Owed::Room),
    }
}

fn is_member(who: &str, members: &[String]) -> bool {
    members.iter().any(|member| same(member, who))
}

/// The first line of a post, clipped: what a nudge quotes back.
fn head(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= HEAD {
        return line.to_string();
    }
    let kept: String = line.chars().take(HEAD - 1).collect();
    format!("{kept}…")
}

/// What a room owes, as one snapshot of it has it. The posts and the
/// membership come from the same place, so neither can drift from the other.
pub fn of_state(state: &SessionState) -> Vec<Mention> {
    let posts: Vec<Post> = state.items.iter().filter_map(Post::of).collect();
    mentions(&posts, &room::members_of(state))
}

/// The same, read from the room itself. A room this process cannot read owes
/// nothing here: a debt is only ever what the journal says it is.
pub async fn of_room(host: &HostHandle, room: &SessionId) -> Vec<Mention> {
    room::read(host, room)
        .await
        .as_ref()
        .map(of_state)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members() -> Vec<String> {
        ["reviewer", "scout"].map(str::to_string).to_vec()
    }

    /// A post at second `n` of the room's life.
    fn post(n: i64, author: &str, text: &str) -> Post {
        Post {
            id: ItemId::from_raw(format!("itm_{n}")),
            author: author.into(),
            at: Timestamp::from_second(n).expect("a timestamp"),
            text: text.into(),
        }
    }

    fn owed(posts: &[Post]) -> Vec<(String, String)> {
        mentions(posts, &members())
            .iter()
            .map(|m| (m.owed_by.said(), m.asker.clone()))
            .collect()
    }

    #[test]
    fn an_at_a_member_opens_a_debt_and_the_member_s_next_post_closes_it() {
        let asked = [post(1, "parent", "@reviewer can you look?")];
        assert_eq!(owed(&asked), [("reviewer".into(), "parent".into())]);

        let answered = [
            post(1, "parent", "@reviewer can you look?"),
            post(2, "reviewer", "anything at all"),
        ];
        assert!(owed(&answered).is_empty(), "speaking is the answer");
    }

    /// The parser table, as the old tree's reads.
    #[test]
    fn a_name_is_a_word_and_only_a_member_s() {
        let table = [
            ("@reviewer", true),
            ("@Reviewer,", true),
            ("hey @REVIEWER!", true),
            ("(@reviewer)", true),
            ("@reviewer-", true),
            ("line one\n@reviewer", true),
            ("mail@reviewer", false),
            ("x_@reviewer", false),
            ("@reviewers", false),
            ("@code-reviewer", false),
            ("@nobody", false),
            ("reviewer", false),
            ("@", false),
        ];
        for (text, hits) in table {
            let opened = !named(text, &members()).is_empty();
            assert_eq!(opened, hits, "{text:?}");
        }
    }

    #[test]
    fn a_hyphenated_member_is_one_word() {
        let members = ["code-reviewer".to_string()];
        assert_eq!(
            named("@code-reviewer, please", &members),
            [Owed::Member("code-reviewer".into())]
        );
    }

    #[test]
    fn a_debt_is_owed_in_the_roster_s_spelling_however_it_was_typed() {
        let mentions = mentions(&[post(1, "parent", "@REVIEWER ping")], &members());
        assert_eq!(mentions[0].owed_by, Owed::Member("reviewer".into()));
        assert_eq!(mentions[0].owed_by.chased(), Some("reviewer"));
    }

    #[test]
    fn a_post_that_answers_and_asks_settles_before_it_opens() {
        let posts = [
            post(1, "parent", "@scout what does the log say?"),
            post(2, "scout", "thanks @scout — @reviewer what about x?"),
        ];
        assert_eq!(
            owed(&posts),
            [("reviewer".into(), "scout".into())],
            "the scout's own debt closed and it may not open one on itself"
        );
    }

    #[test]
    fn one_post_owes_once_however_often_it_says_the_name() {
        let posts = [post(1, "parent", "@reviewer @reviewer @reviewer look")];
        assert_eq!(owed(&posts).len(), 1);
    }

    #[test]
    fn at_all_is_one_debt_against_the_room_that_any_other_member_closes() {
        let asked = [post(1, "reviewer", "@all stand-up in five")];
        assert_eq!(owed(&asked), [("@all".into(), "reviewer".into())]);
        assert_eq!(
            mentions(&asked, &members())[0].owed_by.chased(),
            None,
            "the sigil named nobody, so the chase names nobody"
        );

        let again = [
            post(1, "reviewer", "@all stand-up in five"),
            post(2, "reviewer", "anyone?"),
            post(3, "parent", "in a minute"),
        ];
        assert_eq!(
            owed(&again).len(),
            1,
            "neither the one who asked nor a session that is not a member closes it"
        );

        let closed = [
            post(1, "reviewer", "@all stand-up in five"),
            post(2, "scout", "on my way"),
        ];
        assert!(owed(&closed).is_empty());
    }

    /// A room that seats somebody called `all` spends the word on them: the
    /// sigil is what is left over when no member answers to it, so nobody's
    /// name is ever taken by it.
    #[test]
    fn a_member_named_all_takes_the_word_back_from_the_sigil() {
        let seated = ["all".to_string(), "scout".to_string()];
        assert_eq!(
            named("@all stand-up in five", &seated),
            [Owed::Member("all".into())],
            "the member, and no debt against the room"
        );
        assert_eq!(
            named("@all stand-up in five", &["scout".to_string()]),
            [Owed::Room],
            "and off that roster the word is the room's again"
        );
    }

    /// The roster is asked at the fold, never baked into the post: a member
    /// seated after `@all` was said answers it like any other.
    #[test]
    fn at_all_is_answered_by_a_member_the_room_gained_after_it_was_said() {
        let asked = [post(1, "reviewer", "@all stand-up in five")];
        let joined = ["reviewer", "scout", "latecomer"]
            .map(str::to_string)
            .to_vec();
        assert_eq!(mentions(&asked, &joined).len(), 1, "still standing");

        let answered = [
            post(1, "reviewer", "@all stand-up in five"),
            post(2, "latecomer", "here"),
        ];
        assert!(mentions(&answered, &joined).is_empty());
    }

    #[test]
    fn a_member_s_post_closes_every_debt_they_owe() {
        let posts = [
            post(1, "parent", "@reviewer one"),
            post(2, "scout", "@reviewer two"),
            post(3, "reviewer", "both, then"),
        ];
        assert!(owed(&posts).is_empty());
    }

    /// ADR-0028 §3: a rostered holder is a member to the fold, and the seat's
    /// own post — a root holder's signs `parent` — closes what it owes.
    #[test]
    fn the_holder_on_the_roster_owes_and_answers_like_any_member() {
        let seated = ["scout".to_string(), PARENT.to_string()];
        let asked = [post(1, "scout", "@parent what does the build say?")];
        assert_eq!(
            mentions(&asked, &seated)[0].owed_by,
            Owed::Member(PARENT.into())
        );

        let answered = [
            post(1, "scout", "@parent what does the build say?"),
            post(2, PARENT, "it is green"),
        ];
        assert!(
            mentions(&answered, &seated).is_empty(),
            "the seat's own post closes what it owes"
        );
        assert!(
            mentions(&asked, &["scout".to_string()]).is_empty(),
            "off the roster, `@parent` opens nothing (ADR-0028 §4)"
        );
    }

    #[test]
    fn a_room_nobody_is_in_owes_nothing() {
        assert!(mentions(&[post(1, "parent", "@reviewer look")], &[]).is_empty());
    }

    #[test]
    fn a_debt_carries_the_head_of_the_post_that_opened_it() {
        let long = format!("@reviewer {}", "x".repeat(200));
        let mentions = mentions(&[post(1, "parent", &long)], &members());
        assert_eq!(mentions[0].head.chars().count(), HEAD);
        assert!(mentions[0].head.ends_with('…'));
        assert_eq!(mentions[0].at, Timestamp::from_second(1).expect("a stamp"));
        assert_eq!(mentions[0].post, ItemId::from_raw("itm_1"));
    }

    #[test]
    fn a_head_is_the_first_line_of_it() {
        assert_eq!(head("  @reviewer look  \nand again"), "@reviewer look");
        assert_eq!(head(""), "");
    }

    #[test]
    fn only_a_user_item_in_the_journal_is_a_post() {
        use crate::tests::{item, posted_item};
        assert_eq!(
            Post::of(&posted_item("@reviewer look", Some("scout")))
                .expect("a post")
                .author,
            "scout"
        );
        assert_eq!(
            Post::of(&posted_item("look", None)).expect("a post").author,
            PARENT,
            "a post nobody signed came from the session the room hangs under"
        );
        assert_eq!(
            Post::of(&item(ItemBody::Assistant {
                text: "@reviewer look".into()
            })),
            None,
            "a room answers nobody, and what it does not say it does not owe"
        );
    }
}
