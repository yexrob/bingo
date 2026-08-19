//! The collapse groups: which tool calls fold into one summarized row, and
//! what that row says (D99, extended through D111).
//!
//! A streak of look-around work — reads, searches, listings, read-only bash —
//! renders as one `⏺ Searched for 1 pattern, read 2 files` row instead of a
//! column of tool blocks, and the orchestration verbs fold on the same ruling:
//! AgentControl (D-era), then SendMessage and Channel (D111), because checking
//! three agents, messaging one and reseating a room is one streak of
//! coordination to the reader. The classifier ([`classify_tool`]) says what
//! folds and into which counter; [`collapse_summary`] is the one place the
//! counters become English. Both the console and the zoomed view consume this
//! module — split out of `chat.rs` when the file hit the 4000-line cap.

/// Collapse group for consecutive Read/Search operations: collapses into a one-line rule summary (`Read 3 files`).
#[derive(Debug, Clone, Default)]
pub struct CollapseGroup {
    /// Activity indices in the group (in order).
    pub activities: Vec<usize>,
    /// Number of search operations.
    pub search: usize,
    /// Read file paths (deduplicated count).
    pub read_paths: Vec<String>,
    /// Number of read operations without a path.
    pub read_ops: usize,
    /// Number of list operations (ls/tree/du).
    pub list: usize,
    /// Number of plain Bash operations.
    pub bash: usize,
    /// Number of read-only subagent inspections (AgentControl list/messages).
    pub agent_checks: usize,
    /// Number of subagents stopped (AgentControl stop).
    pub agent_stops: usize,
    /// Number of subagents deleted (AgentControl delete).
    pub agent_deletes: usize,
    /// Targets of directed messages (SendMessage), in send order, repeats kept.
    pub send_targets: Vec<String>,
    /// Number of room-list looks (Channel list).
    pub room_checks: usize,
    /// Number of rooms created (Channel create).
    pub room_creates: usize,
    /// Number of roster changes (Channel invite/kick).
    pub room_rosters: usize,
    /// Group still open (in progress → summary uses the -ing form + …).
    pub active: bool,
    /// ctrl+o / click expands the group into individual tools.
    pub expanded: bool,
    /// Input hint of the group's most recent tool (shown on the ⎿ line while running).
    pub last_hint: Option<String>,
}

/// Collapsible classification of a tool (isSearchOrReadCommand).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CollapseKind {
    Search,
    /// Read or read-like Bash: carries a file path (None for Bash).
    Read(Option<String>),
    List,
    /// Plain Bash that is neither search, read, nor list.
    Bash,
    /// Looking a subagent up (AgentControl list/messages).
    AgentCheck,
    /// Stopping a subagent (AgentControl stop).
    AgentStop,
    /// Deleting a subagent (AgentControl delete).
    AgentDelete,
    /// A directed message leaving (SendMessage) — carries the target's label,
    /// sigil included, so a lone send can say who it reached.
    Send(String),
    /// Looking the room list up (Channel list).
    RoomCheck,
    /// Creating a room (Channel create).
    RoomCreate,
    /// Changing a room's roster (Channel invite/kick).
    RoomRoster,
}

/// Read/Search-style tool classification.
pub fn classify_tool(name: &str, input: &serde_json::Value) -> Option<CollapseKind> {
    match name {
        "Read" => input
            .get("file_path")
            .and_then(|p| p.as_str())
            .map(|p| CollapseKind::Read(Some(p.to_string()))),
        "Grep" | "Glob" => Some(CollapseKind::Search),
        // Managing subagents runs in streaks (check three, stop one), and every row used to be
        // its own two-line block that also closed whatever group was open. Fold the whole
        // streak, but count a stop apart from a look so the summary never reports a
        // deletion as a glance. An action-less call stays standalone (it is a malformed call).
        "AgentControl" => match input.get("action").and_then(|a| a.as_str()) {
            Some("stop") => Some(CollapseKind::AgentStop),
            Some("delete") => Some(CollapseKind::AgentDelete),
            Some(_) => Some(CollapseKind::AgentCheck),
            None => None,
        },
        // The other two orchestration verbs fold on the same ruling
        // AgentControl got: check three agents, message one, reseat a room —
        // that is one streak of coordination, not four standalone blocks. The
        // send keeps its target so a lone send names who it reached; a
        // target-less call is malformed and stays standalone.
        "SendMessage" => input.get("to").and_then(|t| t.as_str()).map(|t| {
            let label = if t.starts_with('#') || t.starts_with('@') {
                t.to_string()
            } else {
                format!("@{t}")
            };
            CollapseKind::Send(label)
        }),
        "Channel" => match input.get("action").and_then(|a| a.as_str()) {
            Some("list") => Some(CollapseKind::RoomCheck),
            Some("create") => Some(CollapseKind::RoomCreate),
            Some("invite") | Some("kick") => Some(CollapseKind::RoomRoster),
            _ => None,
        },
        "Bash" => {
            let kind = input
                .get("command")
                .and_then(|c| c.as_str())
                .and_then(classify_bash_command);
            if kind.is_some() {
                kind
            } else if input
                .get("command")
                .and_then(|c| c.as_str())
                .is_some_and(bash_has_work)
            {
                Some(CollapseKind::Bash)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Whether the command contains a non-neutral segment (pure echo/printf etc. do not collapse).
fn bash_has_work(command: &str) -> bool {
    const NEUTRAL: &[&str] = &["echo", "printf", "true", "false", ":"];
    let mut skip_next = false;
    for part in command.split(['&', '|', ';']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if skip_next {
            skip_next = false;
            continue;
        }
        if part.starts_with('>') {
            skip_next = true;
            continue;
        }
        let base = part.split_whitespace().next().unwrap_or("");
        if !NEUTRAL.contains(&base) {
            return true;
        }
    }
    false
}

/// Bash command classification (split on && / || / | / ;, skipping quantifiers, redirection targets,
/// and neutral commands; every segment must belong to the search/read/list sets; when mixed, place by list > search > read).
pub fn classify_bash_command(command: &str) -> Option<CollapseKind> {
    const SEARCH: &[&str] = &[
        "find", "grep", "rg", "ag", "ack", "locate", "which", "whereis",
    ];
    const READ: &[&str] = &[
        "cat", "head", "tail", "less", "more", "wc", "stat", "file", "strings", "jq", "awk", "cut",
        "sort", "uniq", "tr",
    ];
    const LIST: &[&str] = &["ls", "tree", "du"];
    const NEUTRAL: &[&str] = &["echo", "printf", "true", "false", ":"];
    let mut seen = false;
    let mut list = false;
    let mut search = false;
    let mut read = false;
    let mut skip_next = false;
    for part in command.split(['&', '|', ';']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if skip_next {
            skip_next = false;
            continue;
        }
        if part.starts_with('>') {
            skip_next = true;
            continue;
        }
        let base = part.split_whitespace().next().unwrap_or("");
        if NEUTRAL.contains(&base) {
            continue;
        }
        seen = true;
        if LIST.contains(&base) {
            list = true;
        } else if SEARCH.contains(&base) {
            search = true;
        } else if READ.contains(&base) {
            read = true;
        } else {
            return None;
        }
    }
    if !seen {
        return None;
    }
    if list {
        Some(CollapseKind::List)
    } else if search {
        Some(CollapseKind::Search)
    } else if read {
        Some(CollapseKind::Read(None))
    } else {
        None
    }
}

pub fn collapse_summary(g: &CollapseGroup, in_progress: bool) -> String {
    let active = in_progress;
    let mut parts: Vec<String> = Vec::new();
    let mut push = |verb_done: &str, verb_ing: &str, body: String| {
        if parts.is_empty() {
            let v = if active { verb_ing } else { verb_done };
            parts.push(format!("{}{body}", capitalize(v)));
        } else {
            let v = if active { verb_ing } else { verb_done };
            parts.push(format!("{v}{body}"));
        }
    };
    if g.search > 0 {
        push(
            "searched for",
            "searching for",
            format!(
                " {} {}",
                g.search,
                if g.search == 1 { "pattern" } else { "patterns" }
            ),
        );
    }
    let read_count = if g.read_paths.is_empty() {
        g.read_ops
    } else {
        g.read_paths
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
    };
    if read_count > 0 {
        push(
            "read",
            "reading",
            format!(
                " {} {}",
                read_count,
                if read_count == 1 { "file" } else { "files" }
            ),
        );
    }
    if g.list > 0 {
        push(
            "listed",
            "listing",
            format!(
                " {} {}",
                g.list,
                if g.list == 1 {
                    "directory"
                } else {
                    "directories"
                }
            ),
        );
    }
    if g.agent_checks > 0 {
        push(
            "checked",
            "checking",
            format!(" {} {}", g.agent_checks, subagents(g.agent_checks)),
        );
    }
    // A stop and a delete are counted (and worded) apart from a look: folding them into
    // "checked 4 subagents" would report a run being killed as a glance.
    if g.agent_stops > 0 {
        push(
            "stopped",
            "stopping",
            format!(" {} {}", g.agent_stops, subagents(g.agent_stops)),
        );
    }
    if g.agent_deletes > 0 {
        push(
            "deleted",
            "deleting",
            format!(" {} {}", g.agent_deletes, subagents(g.agent_deletes)),
        );
    }
    // A send is the one part whose object matters more than its count: one
    // send names its target, a burst to one target counts itself, and a spray
    // across targets falls back to the count alone (the expanded rows name
    // them all).
    if !g.send_targets.is_empty() {
        let n = g.send_targets.len();
        let distinct: std::collections::HashSet<&String> = g.send_targets.iter().collect();
        let body = if n == 1 {
            format!(" {}", g.send_targets[0])
        } else if distinct.len() == 1 {
            format!(" {} {n} times", g.send_targets[0])
        } else {
            format!(" {n} recipients")
        };
        push("messaged", "messaging", body);
    }
    if g.room_creates > 0 {
        push(
            "created",
            "creating",
            format!(
                " {} {}",
                g.room_creates,
                if g.room_creates == 1 { "room" } else { "rooms" }
            ),
        );
    }
    if g.room_rosters > 0 {
        push(
            "changed",
            "changing",
            format!(
                " {} {}",
                g.room_rosters,
                if g.room_rosters == 1 {
                    "roster"
                } else {
                    "rosters"
                }
            ),
        );
    }
    // `list` looks at all rooms at once, so the unit is "the rooms", however
    // many there are; only a repeat earns a count.
    if g.room_checks > 0 {
        let body = if g.room_checks == 1 {
            " the rooms".to_string()
        } else {
            format!(" the rooms {} times", g.room_checks)
        };
        push("looked at", "looking at", body);
    }
    if g.bash > 0 {
        push(
            "ran",
            "running",
            format!(
                " {} bash {}",
                g.bash,
                if g.bash == 1 { "command" } else { "commands" }
            ),
        );
    }
    let text = parts.join(", ");
    if active { format!("{text}…") } else { text }
}

fn subagents(n: usize) -> &'static str {
    if n == 1 { "subagent" } else { "subagents" }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bash_classifier_collapsible_commands() {
        assert_eq!(
            classify_bash_command("cat README.md"),
            Some(CollapseKind::Read(None))
        );
        assert_eq!(
            classify_bash_command("grep -rn foo src/"),
            Some(CollapseKind::Search)
        );
        assert_eq!(classify_bash_command("ls -la ."), Some(CollapseKind::List));
        assert_eq!(
            classify_bash_command("cat a | grep foo"),
            Some(CollapseKind::Search)
        );
        assert_eq!(
            classify_bash_command("ls dir && echo \"---\" && ls dir2"),
            Some(CollapseKind::List)
        );
        assert_eq!(
            classify_bash_command("head -20 file > /tmp/out"),
            Some(CollapseKind::Read(None))
        );
    }

    #[test]
    fn bash_classifier_other_commands_not_collapsible() {
        assert_eq!(classify_bash_command("git log --oneline -10"), None);
        assert_eq!(classify_bash_command("npm install"), None);
        assert_eq!(classify_bash_command("echo hello"), None);
        assert_eq!(classify_bash_command("ls && git status"), None);
        assert_eq!(classify_bash_command(""), None);
    }

    #[test]
    fn tool_classifier_read_grep_glob() {
        assert_eq!(
            classify_tool("Read", &json!({"file_path": "a.md"})),
            Some(CollapseKind::Read(Some("a.md".to_string())))
        );
        assert_eq!(classify_tool("Read", &json!({})), None);
        assert_eq!(
            classify_tool("Grep", &json!({"pattern": "x"})),
            Some(CollapseKind::Search)
        );
        assert_eq!(
            classify_tool("Glob", &json!({"glob": "**/*.rs"})),
            Some(CollapseKind::Search)
        );
        assert_eq!(
            classify_tool("Bash", &json!({"command": "git log"})),
            Some(CollapseKind::Bash)
        );
        assert_eq!(classify_tool("Bash", &json!({"command": "echo hi"})), None);
        assert_eq!(
            classify_tool("Bash", &json!({"command": "cargo test && echo done"})),
            Some(CollapseKind::Bash)
        );
        assert_eq!(classify_tool("WebFetch", &json!({"url": "x"})), None);
        assert_eq!(classify_tool("WebSearch", &json!({"query": "x"})), None);
    }

    #[test]
    fn agent_control_classifier_counts_a_change_apart_from_a_look() {
        assert_eq!(
            classify_tool("AgentControl", &json!({"action": "list"})),
            Some(CollapseKind::AgentCheck)
        );
        assert_eq!(
            classify_tool(
                "AgentControl",
                &json!({"action": "messages", "agent": "scout"})
            ),
            Some(CollapseKind::AgentCheck)
        );
        assert_eq!(
            classify_tool("AgentControl", &json!({"action": "stop", "agent": "scout"})),
            Some(CollapseKind::AgentStop)
        );
        assert_eq!(
            classify_tool(
                "AgentControl",
                &json!({"action": "delete", "agent": "scout"})
            ),
            Some(CollapseKind::AgentDelete)
        );
        // No action at all is a malformed call: it stays a standalone row rather than
        // being counted as one of anything.
        assert_eq!(
            classify_tool("AgentControl", &json!({"agent": "scout"})),
            None
        );
    }

    #[test]
    fn summary_past_tense_counts() {
        let mut g = CollapseGroup {
            activities: vec![0, 1, 2],
            search: 1,
            read_paths: vec!["a.md".into(), "b.md".into(), "c.md".into()],
            ..CollapseGroup::default()
        };
        assert_eq!(
            collapse_summary(&g, false),
            "Searched for 1 pattern, read 3 files"
        );
        g.search = 2;
        assert_eq!(
            collapse_summary(&g, false),
            "Searched for 2 patterns, read 3 files"
        );
        g.active = true;
        assert_eq!(
            collapse_summary(&g, true),
            "Searching for 2 patterns, reading 3 files…"
        );
    }

    #[test]
    fn summary_never_reports_a_stopped_subagent_as_a_look() {
        let g = CollapseGroup {
            activities: vec![0, 1, 2, 3],
            agent_checks: 3,
            agent_stops: 1,
            ..CollapseGroup::default()
        };
        assert_eq!(
            collapse_summary(&g, false),
            "Checked 3 subagents, stopped 1 subagent"
        );
        let g = CollapseGroup {
            activities: vec![0],
            agent_deletes: 1,
            ..CollapseGroup::default()
        };
        assert_eq!(collapse_summary(&g, false), "Deleted 1 subagent");
        // Mixed with file work: one group, one line, every kind still named.
        let g = CollapseGroup {
            activities: vec![0, 1],
            read_paths: vec!["a.md".into()],
            agent_checks: 2,
            ..CollapseGroup::default()
        };
        assert_eq!(
            collapse_summary(&g, true),
            "Reading 1 file, checking 2 subagents…"
        );
    }

    /// The other two orchestration verbs fold on AgentControl's ruling (D111):
    /// a send is classified with its target, sigil normalised, and every Channel
    /// action lands in its own counter — a created room must never be reported as
    /// a glance at the list.
    #[test]
    fn sends_and_room_actions_classify_into_the_streak() {
        assert_eq!(
            classify_tool("SendMessage", &json!({"to": "scout", "message": "hi"})),
            Some(CollapseKind::Send("@scout".into()))
        );
        assert_eq!(
            classify_tool("SendMessage", &json!({"to": "#build", "message": "hi"})),
            Some(CollapseKind::Send("#build".into()))
        );
        // A target-less call is malformed and stays a standalone row.
        assert_eq!(
            classify_tool("SendMessage", &json!({"message": "hi"})),
            None
        );
        assert_eq!(
            classify_tool("Channel", &json!({"action": "list"})),
            Some(CollapseKind::RoomCheck)
        );
        assert_eq!(
            classify_tool("Channel", &json!({"action": "create", "channel": "t"})),
            Some(CollapseKind::RoomCreate)
        );
        assert_eq!(
            classify_tool("Channel", &json!({"action": "invite", "channel": "t"})),
            Some(CollapseKind::RoomRoster)
        );
        assert_eq!(
            classify_tool("Channel", &json!({"action": "kick", "channel": "t"})),
            Some(CollapseKind::RoomRoster)
        );
        assert_eq!(classify_tool("Channel", &json!({})), None);
    }

    /// One send names its target, a burst to one target counts itself, a spray
    /// falls back to the count; room verbs read as themselves beside the rest.
    #[test]
    fn summary_names_a_send_target_and_counts_a_spray() {
        let g = CollapseGroup {
            activities: vec![0],
            send_targets: vec!["@scout".into()],
            ..CollapseGroup::default()
        };
        assert_eq!(collapse_summary(&g, false), "Messaged @scout");
        let g = CollapseGroup {
            activities: vec![0, 1, 2],
            send_targets: vec!["@scout".into(), "@scout".into(), "@scout".into()],
            ..CollapseGroup::default()
        };
        assert_eq!(collapse_summary(&g, false), "Messaged @scout 3 times");
        let g = CollapseGroup {
            activities: vec![0, 1],
            send_targets: vec!["@scout".into(), "#build".into()],
            ..CollapseGroup::default()
        };
        assert_eq!(collapse_summary(&g, false), "Messaged 2 recipients");
        // The whole coordination streak on one line, live.
        let g = CollapseGroup {
            activities: vec![0, 1, 2, 3],
            agent_checks: 1,
            send_targets: vec!["@scout".into()],
            room_creates: 1,
            room_checks: 1,
            ..CollapseGroup::default()
        };
        assert_eq!(
            collapse_summary(&g, true),
            "Checking 1 subagent, messaging @scout, creating 1 room, looking at the rooms…"
        );
        let g = CollapseGroup {
            activities: vec![0, 1],
            room_rosters: 2,
            ..CollapseGroup::default()
        };
        assert_eq!(collapse_summary(&g, false), "Changed 2 rosters");
    }

    #[test]
    fn summary_read_paths_dedupe_and_ops_fallback() {
        let g = CollapseGroup {
            activities: vec![0, 1],
            read_paths: vec!["a.md".into(), "a.md".into()],
            ..CollapseGroup::default()
        };
        assert_eq!(collapse_summary(&g, false), "Read 1 file");
        let g = CollapseGroup {
            activities: vec![0],
            read_ops: 2,
            list: 1,
            ..CollapseGroup::default()
        };
        assert_eq!(
            collapse_summary(&g, false),
            "Read 2 files, listed 1 directory"
        );
    }
}
