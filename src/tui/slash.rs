//! The terminal's view of the command table: the dropdown and its ranking.
//!
//! The table itself is `crate::app::action` (D146). What is left here is
//! presentation — how a half-typed `/mo` picks which entries to show and in what
//! order — because ranking a dropdown is a frontend's business and knowing what
//! `/model` means is not.

/// Slash command metadata: name, argument hint, and user-facing description.
pub type SlashCommand = (&'static str, &'static str, &'static str);

/// The command table in the shape the dropdown draws it.
pub fn commands() -> Vec<SlashCommand> {
    crate::app::action::COMMANDS
        .iter()
        .map(|spec| (spec.name, spec.hint, spec.description))
        .collect()
}

/// Slash dropdown suggestion item (`/name`, hint, and description).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashSuggestion {
    pub name: String,
    pub hint: String,
    pub description: String,
}

/// Result of rebuilding the slash dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashSuggestions {
    pub items: Vec<SlashSuggestion>,
    pub no_match: bool,
}

/// Builds slash suggestions from the input and an already-loaded set of extra entries
/// such as skills. Prefix matches rank before substring matches; ties prefer shorter names.
pub fn suggestions(
    input: &str,
    commands: &[SlashCommand],
    extras: impl IntoIterator<Item = SlashSuggestion>,
    max_items: usize,
) -> SlashSuggestions {
    let input = input.trim_end();
    let Some(query) = input.strip_prefix('/') else {
        return SlashSuggestions {
            items: Vec::new(),
            no_match: false,
        };
    };
    if query.contains(char::is_whitespace) {
        return SlashSuggestions {
            items: Vec::new(),
            no_match: false,
        };
    }

    let mut items: Vec<SlashSuggestion> = commands
        .iter()
        .map(|(name, hint, description)| SlashSuggestion {
            name: (*name).to_string(),
            hint: (*hint).to_string(),
            description: (*description).to_string(),
        })
        .chain(extras)
        .collect();
    let normalized = query.to_lowercase();
    if !normalized.is_empty() {
        items.retain(|suggestion| {
            let name = suggestion.name.to_lowercase();
            name.starts_with(&normalized) || name.contains(&normalized)
        });
        items.sort_by(|left, right| {
            let left_prefix = left.name.to_lowercase().starts_with(&normalized);
            let right_prefix = right.name.to_lowercase().starts_with(&normalized);
            right_prefix
                .cmp(&left_prefix)
                .then(left.name.len().cmp(&right.name.len()))
        });
    }
    items.truncate(max_items);
    SlashSuggestions {
        no_match: !normalized.is_empty() && items.is_empty(),
        items,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMANDS: &[SlashCommand] = &[
        ("help", "", "show available commands"),
        ("model", "[name]", "show/switch the model"),
        ("cd", "<dir>", "switch the session working directory"),
        ("status", "", "show session status"),
    ];

    /// The dropdown draws the one table, in its order. `/help`'s own shape is
    /// asserted where the table lives (`app::action::help_is_the_table_and_nothing_else`).
    #[test]
    fn the_dropdown_draws_the_command_table() {
        let drawn = commands();
        assert_eq!(drawn.len(), crate::app::action::COMMANDS.len());
        for ((name, hint, description), spec) in drawn.iter().zip(crate::app::action::COMMANDS) {
            assert_eq!(*name, spec.name);
            assert_eq!(*hint, spec.hint);
            assert_eq!(*description, spec.description);
        }
    }

    #[test]
    fn suggestions_filter_rank_merge_and_cap() {
        let extras = vec![SlashSuggestion {
            name: "model-check".to_string(),
            hint: String::new(),
            description: "skill".to_string(),
        }];
        let result = suggestions("/mo", COMMANDS, extras, 2);
        assert_eq!(
            result
                .items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["model", "model-check"]
        );
        assert!(!result.no_match);

        let empty = suggestions("/missing", COMMANDS, Vec::new(), 5);
        assert!(empty.items.is_empty());
        assert!(empty.no_match);
    }

    #[test]
    fn suggestions_only_open_for_argument_free_slash_input() {
        assert!(
            suggestions("hello", COMMANDS, Vec::new(), 5)
                .items
                .is_empty()
        );
        assert!(
            suggestions("/model name", COMMANDS, Vec::new(), 5)
                .items
                .is_empty()
        );
        assert_eq!(suggestions("/", COMMANDS, Vec::new(), 2).items.len(), 2);
    }
}
