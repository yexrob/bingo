//! Recall: the entries that fit what the person just said, appended as a user
//! item after their turn. It lands in the transcript, so what the model saw is
//! what a person can read back, and the cache prefix in front of it stands
//! (ADR-0014 §6).

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    ContentPart, ContextContributor, ContextError, ContextPiece, ContextQuery, Item, ItemBody,
    Placement,
};

use crate::entry::Entry;
use crate::store::Library;
use crate::{rank, render};

/// Unsolicited, so it is short: three lines is a hint, ten is an argument.
const MAX: usize = 3;

const PREAMBLE: &str =
    "Experience that may fit what was just asked — ExperienceQuery reads one in full:";

/// The id the transcript shows this under.
const ID: &str = "experience:recall";

/// Ranks the active entries against the latest thing the person said.
#[derive(Debug)]
pub struct RecallContributor {
    library: Arc<Library>,
}

impl RecallContributor {
    pub fn new(library: Arc<Library>) -> Self {
        Self { library }
    }
}

#[async_trait]
impl ContextContributor for RecallContributor {
    fn id(&self) -> &str {
        ID
    }

    fn placement(&self) -> Placement {
        Placement::RoundStart
    }

    async fn contribute(&self, query: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError> {
        // Cheapest first: the text is in hand, the store costs a syscall.
        let Some(text) = unanswered(query.items) else {
            return Ok(Vec::new());
        };
        if !self.library.occupied(query.cwd) {
            return Ok(Vec::new());
        }
        let shelf = self.library.load(query.cwd);
        let active: Vec<&Entry> = shelf.active().collect();
        let mut hits = rank::best(&active, &text, true);
        hits.truncate(MAX);
        Ok(piece(&hits).into_iter().collect())
    }
}

fn piece(hits: &[&Entry]) -> Option<ContextPiece> {
    if hits.is_empty() {
        return None;
    }
    let lines = hits
        .iter()
        .map(|entry| format!("- {}", render::line(entry)))
        .collect::<Vec<_>>()
        .join("\n");
    Some(ContextPiece::User {
        parts: vec![ContentPart::text(format!("{PREAMBLE}\n{lines}"))],
        label: ID.into(),
    })
}

/// The latest thing a person said that this contributor has not already
/// answered. A turn asks its contributors at every round, and the same three
/// lines twice are noise; nothing said, nothing to recall.
fn unanswered(items: &[Item]) -> Option<String> {
    for item in items.iter().rev() {
        let ItemBody::User { parts, origin } = &item.body else {
            continue;
        };
        if origin.surface == format!("contributor:{ID}") {
            return None;
        }
        if origin.surface.starts_with("contributor:") {
            continue;
        }
        let text: String = parts
            .iter()
            .filter_map(ContentPart::as_text)
            .collect::<Vec<_>>()
            .join("\n");
        return Some(text).filter(|text| !text.trim().is_empty());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::Status;
    use crate::entry::tests::entry;
    use crate::tests::{Fixture, said};

    fn shelved(fixture: &Fixture, id: &str, trigger: &str, status: Status) {
        let entry = Entry {
            id: id.into(),
            trigger: vec![trigger.into()],
            summary: format!("what to do about {trigger}"),
            status,
            ..entry()
        };
        fixture
            .library
            .save(&fixture.cwd(), &entry)
            .expect("an entry");
    }

    async fn recalled(fixture: &Fixture, items: Vec<Item>) -> Option<String> {
        let asked = fixture.asked(items);
        let pieces = RecallContributor::new(fixture.library.clone())
            .contribute(asked.query())
            .await
            .expect("recall never fails a turn");
        pieces.into_iter().next().map(|piece| match piece {
            ContextPiece::User { parts, label } => {
                assert_eq!(label, "experience:recall");
                parts.iter().filter_map(ContentPart::as_text).collect()
            }
            ContextPiece::System(_) => panic!("recall is a user item, not a system block"),
        })
    }

    #[tokio::test]
    async fn the_entries_that_fit_the_question_land_as_a_user_item() {
        let fixture = Fixture::new();
        shelved(
            &fixture,
            "aaaa1111",
            "the sqlite migration fails",
            Status::Active,
        );
        shelved(&fixture, "bbbb2222", "the bundle is stale", Status::Active);
        let text = recalled(
            &fixture,
            vec![said("the sqlite migration failed again", "tui")],
        )
        .await
        .expect("a recall");
        assert!(text.starts_with(PREAMBLE), "{text}");
        assert!(text.contains("aaaa1111"), "{text}");
        assert!(!text.contains("bbbb2222"), "{text}");
    }

    #[tokio::test]
    async fn a_retired_entry_is_never_recalled() {
        let fixture = Fixture::new();
        shelved(
            &fixture,
            "aaaa1111",
            "the sqlite migration fails",
            Status::Retired,
        );
        assert_eq!(
            recalled(&fixture, vec![said("the sqlite migration failed", "tui")]).await,
            None
        );
    }

    #[tokio::test]
    async fn an_empty_store_and_an_empty_question_both_contribute_nothing() {
        let fixture = Fixture::new();
        assert_eq!(
            recalled(&fixture, vec![said("anything at all", "tui")]).await,
            None,
            "there is no store yet"
        );
        assert!(
            !fixture.dir().exists(),
            "an empty store was created by reading it"
        );

        shelved(&fixture, "aaaa1111", "the migration fails", Status::Active);
        assert_eq!(
            recalled(&fixture, Vec::new()).await,
            None,
            "nobody said anything"
        );
        assert_eq!(
            recalled(&fixture, vec![said("   ", "tui")]).await,
            None,
            "an empty message"
        );
    }

    /// Not one token in common, so not a word reaches the prompt. (A word the
    /// entry does share is a hit however common it is: the floor is relative,
    /// and in a one-entry library the only hit is also the best one.)
    #[tokio::test]
    async fn nothing_relevant_is_nothing_at_all() {
        let fixture = Fixture::new();
        shelved(&fixture, "aaaa1111", "the migration fails", Status::Active);
        assert_eq!(
            recalled(&fixture, vec![said("weather forecast tomorrow", "tui")]).await,
            None
        );
    }

    #[tokio::test]
    async fn it_answers_a_message_once_however_many_rounds_the_turn_runs() {
        let fixture = Fixture::new();
        shelved(&fixture, "aaaa1111", "the migration fails", Status::Active);
        let asked = vec![said("the migration fails", "tui")];
        assert!(recalled(&fixture, asked.clone()).await.is_some());

        let mut answered = asked.clone();
        answered.push(said("- aaaa1111 …", "contributor:experience:recall"));
        assert_eq!(recalled(&fixture, answered.clone()).await, None);

        // A second message in the same turn is a new question.
        answered.push(said("the migration fails again", "tui"));
        assert!(recalled(&fixture, answered).await.is_some());
    }

    #[tokio::test]
    async fn at_most_three_lines_reach_the_prompt() {
        let fixture = Fixture::new();
        for n in 0..6 {
            shelved(
                &fixture,
                &format!("aaaa{n:04}"),
                "the migration fails",
                Status::Active,
            );
        }
        let text = recalled(&fixture, vec![said("the migration fails", "tui")])
            .await
            .expect("a recall");
        assert_eq!(text.lines().filter(|l| l.starts_with("- ")).count(), MAX);
    }

    #[test]
    fn it_is_a_round_start_piece() {
        let fixture = Fixture::new();
        let contributor = RecallContributor::new(fixture.library.clone());
        assert_eq!(contributor.id(), "experience:recall");
        assert_eq!(contributor.placement(), Placement::RoundStart);
    }
}
