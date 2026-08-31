//! The three tools over the store (ADR-0019 §6), and what they share: the
//! id prefix every one of them accepts, and the errors a model reads and
//! acts on rather than a call that failed.

mod create;
mod forget;
mod list;

pub use create::ScheduleCreateTool;
pub use forget::ScheduleForgetTool;
pub use list::ScheduleListTool;

use bingo_sdk::ToolError;

use crate::entry::Entry;
use crate::id::{self, Named};
use crate::store::Shelf;

/// The entry a prefix names, or a sentence the model can act on. An id
/// nobody has is not a failed call — it is an answer.
pub(crate) fn find<'a>(shelf: &'a Shelf, prefix: &str) -> Result<&'a Entry, String> {
    let named = id::resolve(&shelf.ids(), prefix);
    match named {
        Named::One(id) => shelf
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .ok_or_else(|| format!("no schedule is called {id}")),
        Named::Unknown => Err(format!(
            "No schedule has an id starting with \"{prefix}\". \
             ScheduleList shows them all."
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

    fn shelf(ids: &[&str]) -> Shelf {
        Shelf {
            entries: ids
                .iter()
                .map(|id| Entry {
                    id: (*id).to_string(),
                    ..entry()
                })
                .collect(),
            unreadable: Vec::new(),
        }
    }

    #[test]
    fn a_unique_prefix_names_one_schedule() {
        let shelf = shelf(&["ab12cd34", "ab99zz00"]);
        assert_eq!(find(&shelf, "ab12").expect("one").id, "ab12cd34");
    }

    #[test]
    fn an_unknown_prefix_names_the_tool_that_lists_them() {
        let error = find(&shelf(&["ab12cd34"]), "zz").expect_err("no such entry");
        assert!(error.contains("ScheduleList"), "{error}");
    }

    #[test]
    fn an_ambiguous_prefix_lists_what_it_could_have_meant() {
        let error = find(&shelf(&["ab12cd34", "ab99zz00"]), "ab").expect_err("two entries");
        assert!(
            error.contains("ab12cd34") && error.contains("ab99zz00"),
            "{error}"
        );
    }
}
