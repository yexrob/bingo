//! The four tools over the library (ADR-0014 §4), and what they share: the
//! id prefix every one of them accepts, and the errors a model reads and
//! recovers from rather than a call that failed.

mod commit;
mod forget;
mod outcome;
mod query;

pub use commit::ExperienceCommitTool;
pub use forget::ExperienceForgetTool;
pub use outcome::ExperienceOutcomeTool;
pub use query::ExperienceQueryTool;

use bingo_sdk::ToolError;

use crate::entry::Entry;
use crate::id::{self, Named};

/// The entry a prefix names, or a sentence the model can act on. An id
/// nobody has is not a failed call — it is an answer.
pub(crate) fn find<'a>(entries: &'a [Entry], prefix: &str) -> Result<&'a Entry, String> {
    match id::resolve(entries, prefix) {
        Named::One(entry) => Ok(entry),
        Named::Unknown => Err(format!(
            "No experience has an id starting with \"{prefix}\". \
             ExperienceQuery searches the library."
        )),
        Named::Ambiguous(ids) => Err(format!(
            "\"{prefix}\" could be any of: {}. Give more of the id.",
            ids.join(", ")
        )),
    }
}

/// The disk said no; the call cannot go on, and the model is told why.
pub(crate) fn failed(error: std::io::Error) -> ToolError {
    ToolError::Failed(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::tests::entry;

    fn entries() -> Vec<Entry> {
        ["ab12cd34", "ab99zz00"]
            .iter()
            .map(|id| Entry {
                id: (*id).to_string(),
                ..entry()
            })
            .collect()
    }

    #[test]
    fn an_unknown_prefix_names_the_tool_that_searches() {
        let error = find(&entries(), "zz").expect_err("no such entry");
        assert!(error.contains("ExperienceQuery"), "{error}");
    }

    #[test]
    fn an_ambiguous_prefix_lists_what_it_could_have_meant() {
        let error = find(&entries(), "ab").expect_err("two entries");
        assert!(
            error.contains("ab12cd34") && error.contains("ab99zz00"),
            "{error}"
        );
    }
}
