//! Slash command metadata and pure menu/help transformations.

/// Slash command metadata: name, argument hint, and user-facing description.
pub type SlashCommand = (&'static str, &'static str, &'static str);

pub const COMMANDS: &[SlashCommand] = &[
    ("help", "", "show available commands"),
    (
        "clear",
        "",
        "clear the conversation and start a new session (aliases: /reset /new)",
    ),
    (
        "compact",
        "",
        "compact the context (old messages → summary)",
    ),
    ("model", "[name]", "show/switch the model"),
    ("cd", "<dir>", "switch the session working directory"),
    ("resume", "[name or keyword]", "resume a past session"),
    ("rename", "[name]", "rename the current session"),
    (
        "gc",
        "",
        "clean expired session data (30-day TTL; latest 100 inactive sessions kept)",
    ),
    (
        "share",
        "[--public] [--open]",
        "export HTML; --public publishes a public link",
    ),
    ("context", "", "show context usage"),
    (
        "status",
        "",
        "show session status (model/permissions/session/context)",
    ),
    (
        "config",
        "",
        "show the effective config and its sources (layer/env vars/endpoint)",
    ),
    (
        "permissions",
        "[allow|deny|ask] [rule]",
        "list/add permission rules",
    ),
    ("theme", "[dark|light|auto]", "switch the theme"),
    ("mcp", "[enable|disable|reconnect]", "manage MCP servers"),
    ("provider", "[name]", "list/switch API providers"),
    (
        "think",
        "[off|low|medium|high|xhigh|max]",
        "set the thinking level",
    ),
    ("skills", "", "list available skills"),
    ("tasks", "", "list background tasks"),
    (
        "team",
        "start|status|assign|stop|list",
        "manage the project team",
    ),
    (
        "open",
        "@agent|#channel|hub",
        "open a conversation in this terminal",
    ),
    ("exit", "", "exit the session"),
];

/// Slash commands that execute immediately while a model turn is active.
// `/open` is instant because switching conversations is exactly what a user
// wants while a turn runs: the turn keeps going, and the flow follows the eye.
pub const INSTANT_COMMANDS: &[&str] = &[
    "think", "model", "provider", "theme", "status", "context", "tasks", "help", "skills",
    "config", "gc", "open",
];

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

/// Formats `/help` directly from the command registry.
pub fn help_lines(commands: &[SlashCommand]) -> Vec<String> {
    let mut lines = vec!["available commands:".to_string()];
    let command_width = commands
        .iter()
        .map(|(name, hint, _)| {
            name.chars().count() + usize::from(!hint.is_empty()) + hint.chars().count()
        })
        .max()
        .unwrap_or(0);
    for (name, hint, description) in commands {
        let command = if hint.is_empty() {
            format!("/{name}")
        } else {
            format!("/{name} {hint}")
        };
        lines.push(format!("  {command:<command_width$} — {description}"));
    }
    // Sub-commands invisible to the dropdown, and the cross-link to the key
    // panel (the two help surfaces used to be disjoint islands).
    lines.push(
        "  /provider login <name> [--device-auth|--manual <token>] · /provider logout <name>"
            .to_string(),
    );
    lines.push("keys: press ? on an empty input for the full table".to_string());
    lines
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

    #[test]
    fn help_is_derived_from_registry() {
        let lines = help_lines(COMMANDS);
        assert_eq!(
            lines.len(),
            COMMANDS.len() + 3,
            "title + commands + sub-command line + key cross-link"
        );
        assert_eq!(lines[0], "available commands:");
        for ((name, hint, description), line) in COMMANDS.iter().zip(&lines[1..]) {
            assert!(
                line.contains(&format!("/{name}")),
                "command present: {line}"
            );
            assert!(line.contains(hint), "arg hint present: {line}");
            assert!(
                line.ends_with(description),
                "description comes from the registry: {line}"
            );
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
