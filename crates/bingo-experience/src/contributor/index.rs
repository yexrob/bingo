//! The appended index snapshot: ten lines saying what this project has
//! learned and what each has been worth, so the model knows there is
//! something to search for. The steps are not here — an index is a pointer,
//! and `ExperienceQuery` is how a playbook is read.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{ContextContributor, ContextError, ContextPiece, ContextQuery, Placement};

use crate::entry::Entry;
use crate::render;
use crate::store::Library;

/// Past this the index is a wall of text; the rest is one line saying so.
const MAX: usize = 10;

const HEADING: &str = "# Experience";

const PREAMBLE: &str = "\
Playbooks this project has taught you, the most useful first. These are \
pointers: `ExperienceQuery` gives you the steps of one, `ExperienceOutcome` \
records with evidence what happened when you followed it, and \
`ExperienceCommit` writes down what you learn.";

/// Lists the active entries of the session's project.
#[derive(Debug)]
pub struct IndexContributor {
    library: Arc<Library>,
}

impl IndexContributor {
    pub fn new(library: Arc<Library>) -> Self {
        Self { library }
    }
}

#[async_trait]
impl ContextContributor for IndexContributor {
    fn id(&self) -> &str {
        "experience:index"
    }

    fn placement(&self) -> Placement {
        Placement::RoundStart
    }

    async fn contribute(&self, query: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError> {
        let shelf = self.library.load(query.cwd);
        let active = render::by_worth(shelf.active());
        let text = if active.is_empty() {
            format!("{HEADING}\n\nNo active playbooks.")
        } else {
            format!("{HEADING}\n\n{PREAMBLE}\n\n{}", listing(&active))
        };
        Ok(ContextPiece::snapshot(self.id(), text, query.items)
            .into_iter()
            .collect())
    }
}

fn listing(active: &[&Entry]) -> String {
    let mut lines: Vec<String> = active
        .iter()
        .take(MAX)
        .map(|entry| format!("- {}", render::line(entry)))
        .collect();
    if let Some(rest) = active.len().checked_sub(MAX).filter(|rest| *rest > 0) {
        lines.push(format!("- … {rest} more — ExperienceQuery searches"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::tests::entry;
    use crate::entry::{Outcome, Record, Status};
    use crate::tests::Fixture;
    use jiff::Timestamp;

    fn scored(fixture: &Fixture, id: &str, helpful: usize, harmful: usize) {
        let mut entry = Entry {
            id: id.into(),
            summary: format!("the playbook of {id}"),
            ..entry()
        };
        for _ in 0..helpful {
            entry.outcomes.push(record(Outcome::Helpful));
        }
        for _ in 0..harmful {
            entry.outcomes.push(record(Outcome::Harmful));
        }
        fixture
            .library
            .save(&fixture.cwd(), &entry)
            .expect("an entry");
    }

    fn record(outcome: Outcome) -> Record {
        Record {
            outcome,
            at: Timestamp::UNIX_EPOCH,
            evidence: "checked".into(),
        }
    }

    async fn block(fixture: &Fixture) -> Option<String> {
        let asked = fixture.asked(Vec::new());
        let pieces = IndexContributor::new(fixture.library.clone())
            .contribute(asked.query())
            .await
            .expect("the index never fails a turn");
        pieces.into_iter().next().map(|piece| match piece {
            ContextPiece::System(_) => panic!("an index is an appended user snapshot"),
            ContextPiece::User { parts, .. } => parts
                .iter()
                .filter_map(|part| match part {
                    bingo_sdk::ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        })
    }

    async fn visible(fixture: &Fixture, items: Vec<bingo_sdk::Item>) -> Vec<ContextPiece> {
        let asked = fixture.asked(items);
        IndexContributor::new(fixture.library.clone())
            .contribute(asked.query())
            .await
            .expect("experience context")
    }

    fn recorded(pieces: &[ContextPiece]) -> bingo_sdk::Item {
        let ContextPiece::User { parts, .. } = &pieces[0] else {
            panic!("experience indexes are appended user snapshots");
        };
        let mut item = crate::tests::said("", "contributor:experience:index");
        let bingo_sdk::ItemBody::User { parts: content, .. } = &mut item.body else {
            unreachable!();
        };
        *content = parts.clone();
        item
    }

    #[tokio::test]
    async fn changed_and_retired_entries_append_and_visible_journal_suppresses_repeats() {
        let fixture = Fixture::new();
        scored(&fixture, "aaaa1111", 1, 0);
        let first = visible(&fixture, Vec::new()).await;
        let mut items = vec![recorded(&first)];
        assert!(visible(&fixture, items.clone()).await.is_empty());

        scored(&fixture, "aaaa1111", 2, 0);
        let changed = visible(&fixture, items.clone()).await;
        assert_eq!(changed.len(), 1);
        let changed_item = recorded(&changed);
        let bingo_sdk::ItemBody::User { parts, .. } = &changed_item.body else {
            unreachable!();
        };
        assert!(
            matches!(&parts[0], bingo_sdk::ContentPart::Text { text } if text.contains("helpful 2"))
        );
        items.push(changed_item);
        assert!(visible(&fixture, items.clone()).await.is_empty());

        fixture
            .library
            .save(
                &fixture.cwd(),
                &Entry {
                    id: "aaaa1111".into(),
                    status: Status::Retired,
                    ..entry()
                },
            )
            .expect("retire the entry");
        let cleared = visible(&fixture, items.clone()).await;
        assert_eq!(cleared.len(), 1);
        let cleared_item = recorded(&cleared);
        assert!(
            matches!(&cleared_item.body, bingo_sdk::ItemBody::User { parts, .. }
            if parts == &vec![bingo_sdk::ContentPart::text("# Experience\n\nNo active playbooks.")])
        );
        items.push(cleared_item);
        assert!(visible(&fixture, items).await.is_empty());
        // A new contributor restores state when compaction removes its visible record.
        assert_eq!(visible(&fixture, Vec::new()).await.len(), 1);
    }

    #[tokio::test]
    async fn an_empty_library_says_there_are_no_active_playbooks() {
        let fixture = Fixture::new();
        assert_eq!(
            block(&fixture).await.as_deref(),
            Some("# Experience\n\nNo active playbooks.")
        );
        // A store with nothing active in it is an empty index too.
        fixture
            .library
            .save(
                &fixture.cwd(),
                &Entry {
                    status: Status::Retired,
                    ..entry()
                },
            )
            .expect("an entry");
        assert_eq!(
            block(&fixture).await.as_deref(),
            Some("# Experience\n\nNo active playbooks.")
        );
    }

    #[tokio::test]
    async fn the_most_useful_entries_come_first() {
        let fixture = Fixture::new();
        scored(&fixture, "aaaa1111", 1, 0);
        scored(&fixture, "bbbb2222", 3, 1);
        scored(&fixture, "cccc3333", 1, 2);
        let text = block(&fixture).await.expect("an index");
        let ids: Vec<&str> = text
            .lines()
            .filter_map(|line| line.strip_prefix("- "))
            .map(|line| &line[..8])
            .collect();
        assert_eq!(ids, ["bbbb2222", "aaaa1111", "cccc3333"]);
        assert!(text.contains("(helpful 3, harmful 1)"), "{text}");
        assert!(text.starts_with("# Experience"), "{text}");
    }

    #[tokio::test]
    async fn past_ten_the_rest_is_one_line_naming_the_tool() {
        let fixture = Fixture::new();
        for n in 0..13 {
            scored(&fixture, &format!("aaaa{n:04}"), 0, 0);
        }
        let text = block(&fixture).await.expect("an index");
        assert_eq!(text.lines().filter(|l| l.starts_with("- ")).count(), 11);
        assert!(
            text.contains("- … 3 more — ExperienceQuery searches"),
            "{text}"
        );
    }

    #[test]
    fn it_refreshes_at_the_start_of_each_round() {
        let fixture = Fixture::new();
        let contributor = IndexContributor::new(fixture.library.clone());
        assert_eq!(contributor.id(), "experience:index");
        assert_eq!(contributor.placement(), Placement::RoundStart);
    }
}
