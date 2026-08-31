//! The index in the system prompt: ten lines saying what this project has
//! learned and what each has been worth, so the model knows there is
//! something to search for. The steps are not here — an index is a pointer,
//! and `ExperienceQuery` is how a playbook is read.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    ContextContributor, ContextError, ContextPiece, ContextQuery, Placement, SystemBlock,
};

use crate::entry::Entry;
use crate::render;
use crate::store::Library;

/// After the instructions and the skills list: what a project has learned is
/// context, not the frame the request is read in.
const ORDER: i32 = 10;

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
        Placement::System { order: ORDER }
    }

    /// Never cached: a commit within the turn changes this block, and a
    /// cached copy would be stale in the session that wrote it.
    async fn contribute(&self, query: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError> {
        if !self.library.occupied(query.cwd) {
            return Ok(Vec::new());
        }
        let shelf = self.library.load(query.cwd);
        let active = render::by_worth(shelf.active());
        if active.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![ContextPiece::System(SystemBlock {
            text: format!("{HEADING}\n\n{PREAMBLE}\n\n{}", listing(&active)),
            cache: false,
        })])
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
            ContextPiece::System(block) => block.text,
            ContextPiece::User { .. } => panic!("an index is a system block"),
        })
    }

    #[tokio::test]
    async fn an_empty_library_says_nothing_at_all() {
        let fixture = Fixture::new();
        assert_eq!(block(&fixture).await, None);
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
        assert_eq!(block(&fixture).await, None);
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
    fn it_sits_after_the_skills_and_is_never_cached() {
        let fixture = Fixture::new();
        let contributor = IndexContributor::new(fixture.library.clone());
        assert_eq!(contributor.id(), "experience:index");
        assert_eq!(contributor.placement(), Placement::System { order: 10 });
    }
}
