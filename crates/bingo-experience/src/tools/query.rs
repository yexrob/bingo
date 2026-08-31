//! `ExperienceQuery`: the search a person or the model asked for. It ranks
//! every entry, retired ones included with their status shown, and it drops
//! the relevance floor — a weak answer is still the best answer to a question
//! somebody actually asked (ADR-0014 §5).

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, View, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::entry::Entry;
use crate::store::Library;
use crate::{rank, render};

const DESCRIPTION: &str = "\
Search this project's experience library for a playbook that fits what you \
are about to do. The words of the task work as the query — an error message, \
a symptom, the kind of change. Entries are ranked by relevance, retired ones \
included and marked, so read the status before following one.";

const DEFAULT_LIMIT: usize = 5;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Query {
    /// What you are about to do, in the words you would use.
    query: String,
    /// How many entries to read back. Five when absent.
    #[serde(default)]
    limit: Option<usize>,
}

/// Reads the library; writes nothing, asks nobody.
#[derive(Debug)]
pub struct ExperienceQueryTool {
    library: Arc<Library>,
}

impl ExperienceQueryTool {
    pub fn new(library: Arc<Library>) -> Self {
        Self { library }
    }
}

#[async_trait]
impl Tool for ExperienceQueryTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ExperienceQuery".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<Query>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits::read_only()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: Query =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let shelf = self.library.load(&cx.cwd);
        let all: Vec<&Entry> = shelf.entries.iter().collect();
        let mut hits = rank::best(&all, &args.query, false);
        hits.truncate(args.limit.unwrap_or(DEFAULT_LIMIT));
        Ok(answer(&hits, &args.query))
    }
}

fn answer(hits: &[&Entry], query: &str) -> ToolOutput {
    if hits.is_empty() {
        return ToolOutput::text(format!(
            "No experience matches \"{query}\". ExperienceCommit writes one down."
        ));
    }
    let text = hits
        .iter()
        .map(|entry| render::full(entry))
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut out = ToolOutput::text(text);
    out.display = Some(View::List {
        items: hits
            .iter()
            .map(|entry| render::line_with_status(entry))
            .collect(),
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::Status;
    use crate::entry::tests::entry;
    use crate::tests::{Fixture, text};
    use serde_json::json;

    fn shelved(fixture: &Fixture, entries: Vec<Entry>) {
        for entry in entries {
            fixture
                .library
                .save(&fixture.cwd(), &entry)
                .expect("an entry");
        }
    }

    fn about(id: &str, trigger: &str, summary: &str) -> Entry {
        Entry {
            id: id.into(),
            trigger: vec![trigger.into()],
            summary: summary.into(),
            ..entry()
        }
    }

    async fn query(fixture: &Fixture, input: Value) -> ToolOutput {
        ExperienceQueryTool::new(fixture.library.clone())
            .call(input, &fixture.context())
            .await
            .expect("a search")
    }

    #[tokio::test]
    async fn the_best_entry_comes_first_and_carries_its_steps() {
        let fixture = Fixture::new();
        shelved(
            &fixture,
            vec![
                about("aaaa1111", "the sqlite migration fails", "run it twice"),
                about(
                    "bbbb2222",
                    "the frontend bundle is stale",
                    "clear the cache",
                ),
            ],
        );
        let out = query(&fixture, json!({"query": "migration failed again"})).await;
        let text = text(&out);
        assert!(text.starts_with("aaaa1111 [active] run it twice"), "{text}");
        assert!(text.contains("when: the sqlite migration fails"), "{text}");
        assert!(text.contains("1. cargo clean"), "{text}");
        assert!(!text.contains("bbbb2222"), "{text}");
        assert!(matches!(out.display, Some(View::List { .. })));
    }

    #[tokio::test]
    async fn a_retired_entry_is_still_an_answer_and_says_so() {
        let fixture = Fixture::new();
        shelved(
            &fixture,
            vec![Entry {
                status: Status::Retired,
                ..about("aaaa1111", "the sqlite migration fails", "run it twice")
            }],
        );
        let out = query(&fixture, json!({"query": "sqlite migration"})).await;
        assert!(text(&out).contains("[retired]"), "{}", text(&out));
    }

    #[tokio::test]
    async fn a_weak_match_is_still_returned_because_someone_asked() {
        let fixture = Fixture::new();
        shelved(
            &fixture,
            vec![
                about(
                    "aaaa1111",
                    "the cache is cold",
                    "warm the cache before the run",
                ),
                about(
                    "bbbb2222",
                    "the run is slow",
                    "warm the cache first, then run",
                ),
            ],
        );
        let out = query(&fixture, json!({"query": "the run"})).await;
        let text = text(&out);
        assert!(
            text.contains("aaaa1111") && text.contains("bbbb2222"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_limit_cuts_the_list_and_nothing_at_all_says_what_to_do() {
        let fixture = Fixture::new();
        shelved(
            &fixture,
            vec![
                about("aaaa1111", "the run is slow", "warm the cache"),
                about("bbbb2222", "the run is slow", "prime the index"),
            ],
        );
        let out = query(&fixture, json!({"query": "the run is slow", "limit": 1})).await;
        assert_eq!(
            text(&out)
                .lines()
                .filter(|l| l.contains("[active]"))
                .count(),
            1
        );

        let out = query(&fixture, json!({"query": "quantum tunnelling"})).await;
        assert!(text(&out).contains("ExperienceCommit"), "{}", text(&out));
        assert!(!out.is_error, "an empty library is not an error");
    }

    #[test]
    fn the_spec_reads_and_nothing_else() {
        let fixture = Fixture::new();
        let tool = ExperienceQueryTool::new(fixture.library.clone());
        assert_eq!(tool.spec().name, "ExperienceQuery");
        let traits = tool.traits(&Value::Null);
        assert!(traits.read_only && traits.trusted && traits.concurrency_safe);
    }
}
