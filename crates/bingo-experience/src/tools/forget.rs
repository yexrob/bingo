//! `ExperienceForget`: remove one entry and its history. Destructive, and the
//! card shows the file that would go — there is no gc, no TTL and no cap
//! (ADR-0014 §8), so this is the whole of the pruning.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Preview, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::store::Library;
use crate::{diff, frontmatter, render, tools};

const DESCRIPTION: &str = "\
Delete one experience and every outcome it has been given. Use it for a \
playbook that was wrong, never for one that stopped applying — that is a \
`retired` status through ExperienceCommit, which keeps what was learned.";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Forget {
    /// The entry to delete; a unique prefix of its id is enough.
    id: String,
}

/// Removes one file.
#[derive(Debug)]
pub struct ExperienceForgetTool {
    library: Arc<Library>,
}

impl ExperienceForgetTool {
    pub fn new(library: Arc<Library>) -> Self {
        Self { library }
    }
}

#[async_trait]
impl Tool for ExperienceForgetTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ExperienceForget".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<Forget>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits::destructive()
    }

    /// What would go: the whole file, as a removal.
    fn preview(&self, input: &Value, cwd: &Path) -> Option<Preview> {
        let args: Forget = serde_json::from_value(input.clone()).ok()?;
        let shelf = self.library.load(cwd);
        let entry = tools::find(&shelf.entries, &args.id).ok()?;
        Some(Preview::Diff {
            unified: diff::unified(
                &self.library.path(cwd, &entry.id),
                &frontmatter::to_markdown(entry),
                "",
            ),
        })
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: Forget =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let shelf = self.library.load(&cx.cwd);
        let entry = match tools::find(&shelf.entries, &args.id) {
            Ok(entry) => entry,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        self.library
            .delete(&cx.cwd, &entry.id)
            .map_err(tools::failed)?;
        Ok(ToolOutput::text(format!("Forgot {}", render::line(entry))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::Entry;
    use crate::entry::tests::entry;
    use crate::tests::{Fixture, files, text};
    use serde_json::json;

    fn shelved(fixture: &Fixture, ids: &[&str]) {
        for id in ids {
            let entry = Entry {
                id: (*id).to_string(),
                ..entry()
            };
            fixture
                .library
                .save(&fixture.cwd(), &entry)
                .expect("an entry");
        }
    }

    async fn forget(fixture: &Fixture, id: &str) -> ToolOutput {
        ExperienceForgetTool::new(fixture.library.clone())
            .call(json!({ "id": id }), &fixture.context())
            .await
            .expect("an answer")
    }

    #[tokio::test]
    async fn one_entry_goes_and_the_others_stay() {
        let fixture = Fixture::new();
        shelved(&fixture, &["aaaa1111", "bbbb2222"]);
        let out = forget(&fixture, "aaaa").await;
        assert!(!out.is_error, "{out:?}");
        assert!(text(&out).contains("aaaa1111"), "{}", text(&out));
        assert_eq!(files(&fixture.dir()), ["bbbb2222.md"]);
    }

    #[tokio::test]
    async fn an_ambiguous_prefix_deletes_nothing() {
        let fixture = Fixture::new();
        shelved(&fixture, &["aaaa1111", "aaaa2222"]);
        let out = forget(&fixture, "aaaa").await;
        assert!(out.is_error);
        assert!(text(&out).contains("Give more of the id"), "{}", text(&out));
        assert_eq!(files(&fixture.dir()).len(), 2);
    }

    #[test]
    fn the_card_shows_the_file_that_would_go() {
        let fixture = Fixture::new();
        shelved(&fixture, &["aaaa1111"]);
        let tool = ExperienceForgetTool::new(fixture.library.clone());
        let Some(Preview::Diff { unified }) = tool.preview(&json!({"id": "aaaa"}), &fixture.cwd())
        else {
            panic!("a deletion shows what goes");
        };
        assert!(unified.contains("aaaa1111.md"), "{unified}");
        assert!(
            unified.contains("-summary: \"clear the target directory\""),
            "{unified}"
        );
        assert_eq!(files(&fixture.dir()).len(), 1, "a preview never writes");
    }

    #[test]
    fn the_spec_is_destructive() {
        let fixture = Fixture::new();
        let tool = ExperienceForgetTool::new(fixture.library.clone());
        assert_eq!(tool.spec().name, "ExperienceForget");
        let traits = tool.traits(&Value::Null);
        assert!(traits.destructive && traits.trusted);
        assert!(!traits.read_only);
    }
}
