use bingo_sdk::{
    CatalogEntry, CommandSpec,
    component::{PaletteOpen, PaletteSession},
};
use std::collections::BTreeMap;

/// The family prefix every skill command carries, `skill/<shelf>`: the one
/// word of contract between the skills plugin and this list.
pub const SKILL_FAMILY: &str = "skill/";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Command(String),
    /// A skill's command; its row's value is the shelf it was read from.
    Skill(String),
    /// A folded part of the list: Enter unfolds it in place.
    Expand(String),
    Key(String),
    Session(String),
    View(String),
    Inert,
    Edit(String),
    Run(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub label: String,
    pub hint: String,
    pub alert: Option<String>,
    pub now: String,
    pub marks: Vec<usize>,
    pub hint_marks: Vec<usize>,
    pub target: Target,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub rows: Vec<Row>,
}

pub fn matched(text: &str, query: &str) -> Option<Vec<usize>> {
    let mut wanted = query.chars().flat_map(char::to_lowercase).peekable();
    let mut indices = Vec::new();
    for (index, c) in text.chars().enumerate() {
        if c.to_lowercase().next().as_ref() == wanted.peek() {
            wanted.next();
            indices.push(index);
        }
    }
    wanted.peek().is_none().then_some(indices)
}

pub fn rows(
    opened: PaletteOpen,
    query: &str,
    commands: &[CatalogEntry],
    bindings: &BTreeMap<String, String>,
    sessions: &[PaletteSession],
    recent: &[String],
    current: &str,
) -> Vec<Group> {
    let (skills, commands_only): (Vec<Row>, Vec<Row>) = command_rows(commands, query)
        .into_iter()
        .partition(|row| matches!(row.target, Target::Skill(_)));
    let mut groups = vec![
        Group {
            name: "commands".into(),
            rows: commands_only,
        },
        Group {
            name: "skills".into(),
            rows: skills,
        },
        Group {
            name: "sessions".into(),
            rows: sessions.iter().map(|s| session(s, current)).collect(),
        },
        Group {
            name: "keys".into(),
            rows: bindings
                .iter()
                .filter_map(|(key, line)| binding(key, line, commands))
                .collect(),
        },
    ];
    let first = match opened {
        PaletteOpen::Commands => "commands",
        PaletteOpen::Keys => "keys",
        PaletteOpen::Sessions => "sessions",
    };
    groups.sort_by_key(|group| group.name != first);
    if query.is_empty() {
        let skills = opened == PaletteOpen::Commands;
        groups.retain(|group| group.name == first || (skills && group.name == "skills"));
        if opened == PaletteOpen::Commands {
            recent_first(&mut groups, recent);
        }
    } else {
        searched(&mut groups, query, commands);
    }
    groups.retain(|group| !group.rows.is_empty());
    groups
}
/// Every group cut to the rows that match, the commands and the skills best
/// first; the skills lead only when their best is strictly better, so a
/// command wins a tie.
fn searched(groups: &mut [Group], query: &str, commands: &[CatalogEntry]) {
    for group in groups.iter_mut() {
        group.rows = group
            .rows
            .drain(..)
            .filter_map(|row| {
                if folded_match(&row, commands, query).is_some() {
                    Some(row)
                } else {
                    filter(row, query)
                }
            })
            .collect();
        if matches!(group.name.as_str(), "commands" | "skills") {
            group
                .rows
                .sort_by_key(|row| search_score(row, commands, query));
        }
    }
    let best = |name: &str| {
        groups
            .iter()
            .position(|group| group.name == name)
            .and_then(|at| {
                Some((
                    at,
                    groups[at]
                        .rows
                        .first()
                        .map(|row| search_score(row, commands, query))?,
                ))
            })
    };
    if let (Some((commands, command)), Some((skills, skill))) = (best("commands"), best("skills"))
        && skill < command
        && skills > commands
    {
        groups[commands..=skills].rotate_right(1);
    }
}
fn search_score(row: &Row, commands: &[CatalogEntry], query: &str) -> (bool, bool, bool) {
    folded_match(row, commands, query)
        .map_or_else(|| score(row, query), |fold| fold.min(score(row, query)))
}

/// A folded spelling finds the place that replaced its row, without giving
/// the destination a second label or a second catalogue entry.
fn folded_match(row: &Row, commands: &[CatalogEntry], query: &str) -> Option<(bool, bool, bool)> {
    let Target::Command(target) = &row.target else {
        return None;
    };
    commands
        .iter()
        .filter(|entry| entry.meta["foldedInto"].as_str() == Some(target.as_str()))
        .filter_map(|entry| serde_json::from_value::<CommandSpec>(entry.meta.clone()).ok())
        .filter(|spec| !spec.hidden)
        .flat_map(|spec| std::iter::once(spec.name).chain(spec.aliases))
        .filter(|name| matched(name, query).is_some())
        .map(|name| (name != query, !name.starts_with(query), false))
        .min()
}

/// Lower is better: the name itself, then a name it begins, then any match
/// in the name before one in the description alone.
fn score(row: &Row, query: &str) -> (bool, bool, bool) {
    let name = row.label.trim_start_matches('/');
    (
        name != query,
        !name.starts_with(query),
        row.marks.is_empty(),
    )
}
fn command_rows(entries: &[CatalogEntry], query: &str) -> Vec<Row> {
    const FIRST: &[&str] = &[
        "model",
        "think",
        "permission",
        "rename",
        "compact",
        "resume",
        "login",
        "logout",
        "status",
        "plugins",
        "mcp",
        "agents",
        "memory",
        "help",
    ];
    let mut rows = entries
        .iter()
        .filter_map(|entry| command(entry, query))
        .collect::<Vec<_>>();
    rows.sort_by_key(|row| {
        FIRST
            .iter()
            .position(|name| *name == row.label.trim_start_matches('/'))
            .unwrap_or(FIRST.len())
    });
    rows
}
/// The most used commands and skills, lifted out of their groups into
/// `recent` ahead of them.
fn recent_first(groups: &mut Vec<Group>, recent: &[String]) {
    let mut rows = Vec::new();
    for name in recent {
        let used = |row: &Row| matches!(&row.target, Target::Command(it) | Target::Skill(it) if it == name);
        if let Some(group) = groups.iter_mut().find(|group| group.rows.iter().any(used))
            && let Some(at) = group.rows.iter().position(used)
        {
            rows.push(group.rows.remove(at));
        }
    }
    if !rows.is_empty() {
        groups.insert(
            0,
            Group {
                name: "recent".into(),
                rows,
            },
        );
    }
}
pub(crate) fn filter(mut row: Row, query: &str) -> Option<Row> {
    if let Some(marks) = matched(&row.label, query) {
        row.marks = marks;
        Some(row)
    } else if row.hint.to_lowercase().contains(&query.to_lowercase()) {
        matched(&row.hint, query).map(|marks| {
            row.hint_marks = marks;
            row
        })
    } else {
        None
    }
}
fn command(entry: &CatalogEntry, query: &str) -> Option<Row> {
    let spec: CommandSpec = serde_json::from_value(entry.meta.clone()).ok()?;
    let shelf = spec.family.strip_prefix(SKILL_FAMILY).map(str::to_owned);
    (!spec.hidden && entry.meta["foldedInto"].as_str().is_none()).then(|| Row {
        label: format!("/{}", spelling(&spec, query)),
        hint: spec.hint.clone(),
        alert: None,
        now: match &shelf {
            Some(shelf) => shelf.clone(),
            None => entry.meta["now"].as_str().unwrap_or_default().into(),
        },
        marks: vec![],
        hint_marks: vec![],
        target: if shelf.is_some() {
            Target::Skill(spec.name)
        } else {
            Target::Command(spec.name)
        },
    })
}
fn spelling<'a>(spec: &'a CommandSpec, query: &str) -> &'a str {
    let names =
        || std::iter::once(spec.name.as_str()).chain(spec.aliases.iter().map(String::as_str));
    names()
        .find(|name| *name == query)
        .or_else(|| names().find(|name| matched(name, query).is_some()))
        .unwrap_or(&spec.name)
}
fn binding(key: &str, line: &str, commands: &[CatalogEntry]) -> Option<Row> {
    let name = line
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or_default();
    let entry = commands.iter().find(|entry| {
        entry.id == name
            || entry.meta["aliases"]
                .as_array()
                .is_some_and(|aliases| aliases.iter().any(|alias| alias == name))
    })?;
    if entry.meta["hidden"].as_bool().unwrap_or(false)
        || entry.meta["foldedInto"].as_str().is_some()
    {
        return None;
    }
    Some(Row {
        label: key.into(),
        hint: if crate::args::person_only(line, commands) {
            line
        } else {
            entry.meta["hint"].as_str().unwrap_or(line)
        }
        .into(),
        alert: None,
        now: entry.meta["now"].as_str().unwrap_or_default().into(),
        marks: vec![],
        hint_marks: vec![],
        target: Target::Key(line.into()),
    })
}
pub(crate) fn session(summary: &PaletteSession, current: &str) -> Row {
    Row {
        label: summary.label.clone(),
        hint: String::new(),
        alert: summary.attention.clone(),
        now: if summary.id.as_str() == current {
            "this one".into()
        } else {
            summary.detail.clone()
        },
        marks: vec![],
        hint_marks: vec![],
        target: Target::Session(summary.id.to_string()),
    }
}
pub fn conversation(sessions: &[PaletteSession], query: &str, current: &str) -> Vec<Row> {
    sessions
        .iter()
        .map(|summary| {
            let mut row = session(summary, current);
            row.target = Target::View(summary.id.to_string());
            row.hint = summary.detail.clone();
            if summary.id.as_str() != current {
                row.now.clear();
            }
            row
        })
        .filter_map(|row| filter(row, query))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn commands() -> Vec<CatalogEntry> {
        ["model","more","hidden"].map(|name| CatalogEntry { id:name.into(), label:name.into(), meta:json!({"name":name,"hint":"choose model","args":{"kind":"none"},"family":"session","hidden":name=="hidden"}) }).to_vec()
    }
    #[test]
    fn openers_search_all_groups_but_empty_queries_keep_their_own() {
        let commands = commands();
        let keys = BTreeMap::from([("alt+m".into(), "/model".into())]);
        for open in [
            PaletteOpen::Commands,
            PaletteOpen::Keys,
            PaletteOpen::Sessions,
        ] {
            let found = rows(open, "mo", &commands, &keys, &[], &[], "s");
            assert_eq!(found.iter().map(|g| g.rows.len()).sum::<usize>(), 3);
            if open == PaletteOpen::Keys {
                assert_eq!(found[0].name, "keys");
            }
        }
        assert!(rows(PaletteOpen::Sessions, "", &commands, &keys, &[], &[], "s").is_empty());
        let found = rows(
            PaletteOpen::Commands,
            "",
            &commands,
            &keys,
            &[],
            &["model".into()],
            "s",
        );
        assert_eq!(found[0].name, "recent");
        assert_eq!(found[1].rows.len(), 1);
        assert_eq!(matched("/model", "md"), Some(vec![1, 3]));
        assert_eq!(matched("model", "zz"), None);
    }
    #[test]
    fn hidden_bindings_stay_hidden_and_hint_matches_are_marked() {
        let bindings = BTreeMap::from([("alt+h".into(), "/hidden".into())]);
        assert!(rows(PaletteOpen::Keys, "", &commands(), &bindings, &[], &[], "s").is_empty());
        let found = rows(
            PaletteOpen::Commands,
            "choose",
            &commands(),
            &bindings,
            &[],
            &[],
            "s",
        );
        assert_eq!(found[0].rows.len(), 2);
        assert_eq!(found[0].rows[0].hint_marks, vec![0, 1, 2, 3, 4, 5]);
    }
    #[test]
    fn an_exact_command_cannot_be_shadowed_by_a_description() {
        let commands = [("agents", "the sub-agents this session started, or stop or dismiss one"), ("board", "show the board")].map(|(name, hint)| CatalogEntry { id:name.into(), label:name.into(), meta:serde_json::json!({"name":name,"hint":hint,"args":{"kind":"none"},"family":"fixture"}) });
        let found = rows(
            PaletteOpen::Commands,
            "board",
            &commands,
            &BTreeMap::new(),
            &[],
            &[],
            "s",
        );
        assert_eq!(found[0].rows.len(), 1);
        assert_eq!(found[0].rows[0].target, Target::Command("board".into()));
        let mut commands = commands;
        commands[0].meta["hint"] = serde_json::json!("show the board");
        let found = rows(
            PaletteOpen::Commands,
            "board",
            &commands,
            &BTreeMap::new(),
            &[],
            &[],
            "s",
        );
        assert_eq!(found[0].rows.len(), 2);
        assert_eq!(found[0].rows[0].target, Target::Command("board".into()));
    }
    #[test]
    fn common_controls_lead_extensions_regardless_of_registration_order() {
        let mut commands = commands();
        commands.insert(0, CatalogEntry { id:"permission".into(), label:"permission".into(), meta:serde_json::json!({"name":"permission","hint":"change mode","args":{"kind":"none"},"family":"session"}) });
        for query in ["", "mo"] {
            let found = rows(
                PaletteOpen::Commands,
                query,
                &commands,
                &BTreeMap::new(),
                &[],
                &[],
                "s",
            );
            assert_eq!(found[0].rows[0].target, Target::Command("model".into()));
        }
    }
}

#[cfg(test)]
mod skill_tests {
    use super::*;
    use serde_json::json;

    fn entry(name: &str, hint: &str, family: &str) -> CatalogEntry {
        CatalogEntry {
            id: name.into(),
            label: name.into(),
            meta: json!({"name":name,"hint":hint,"args":{"kind":"free","hint":""},"family":family}),
        }
    }

    fn catalog() -> Vec<CatalogEntry> {
        vec![
            entry("deploy", "ship the branch", "skill/project"),
            entry("model", "model, thinking and providers", "session"),
            entry("standup", "yesterday's commits", "skill/yours"),
            entry("depends", "what a deploy depends on", "session"),
        ]
    }

    fn find(open: PaletteOpen, query: &str, recent: &[String]) -> Vec<Group> {
        rows(open, query, &catalog(), &BTreeMap::new(), &[], recent, "s")
    }

    fn names(groups: &[Group]) -> Vec<&str> {
        groups.iter().map(|group| group.name.as_str()).collect()
    }

    #[test]
    fn skills_are_their_own_group_after_the_commands_with_their_shelf() {
        let found = find(PaletteOpen::Commands, "", &[]);
        assert_eq!(names(&found), ["commands", "skills"]);
        assert_eq!(found[1].rows[0].target, Target::Skill("deploy".into()));
        assert_eq!(found[1].rows[0].now, "project");
        assert_eq!(found[1].rows[1].now, "yours");
        assert!(
            find(PaletteOpen::Keys, "", &[]).is_empty(),
            "a bare `?` keeps to keys"
        );
    }

    #[test]
    fn a_command_wins_a_tie_and_a_better_skill_leads() {
        assert_eq!(
            names(&find(PaletteOpen::Commands, "dep", &[])),
            ["commands", "skills"]
        );
        let found = find(PaletteOpen::Commands, "deploy", &[]);
        assert_eq!(names(&found), ["skills", "commands"], "the exact name");
        assert_eq!(found[0].rows[0].target, Target::Skill("deploy".into()));
    }

    #[test]
    fn a_skill_used_often_is_recent_too() {
        let found = find(
            PaletteOpen::Commands,
            "",
            &["standup".into(), "model".into()],
        );
        assert_eq!(names(&found), ["recent", "commands", "skills"]);
        assert_eq!(
            found[0]
                .rows
                .iter()
                .map(|row| row.target.clone())
                .collect::<Vec<_>>(),
            [
                Target::Skill("standup".into()),
                Target::Command("model".into())
            ]
        );
        assert_eq!(found[2].rows.len(), 1);
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;
    #[test]
    fn aliases_are_searchable_and_keep_the_canonical_target() {
        let entry = CatalogEntry {
            id: "exit".into(),
            label: "exit".into(),
            meta: serde_json::json!({"name":"exit","aliases":["quit"],"hint":"leave","args":{"kind":"none"},"family":"surface"}),
        };
        let rows = rows(
            PaletteOpen::Commands,
            "quit",
            &[entry],
            &BTreeMap::new(),
            &[],
            &[],
            "s",
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rows[0].label, "/quit");
        assert_eq!(rows[0].rows[0].target, Target::Command("exit".into()));
    }
}

pub(crate) fn all_sessions() -> Row {
    Row {
        label: "All sessions…".into(),
        hint: String::new(),
        alert: None,
        now: String::new(),
        marks: vec![],
        hint_marks: vec![],
        target: Target::Run("/sessions".into()),
    }
}

#[cfg(test)]
mod fold_tests {
    use super::*;
    use serde_json::json;

    fn catalogue() -> Vec<CatalogEntry> {
        [
            ("crew", None),
            ("agents", Some("crew")),
            ("tasks", Some("crew")),
            ("room", Some("crew")),
            ("team", Some("crew")),
            ("browse", None),
            ("rewind", Some("browse")),
            ("memory", None),
            ("experience", Some("memory")),
            ("schedule", None),
            ("wake", Some("schedule")),
            ("model", None),
            ("models", Some("model")),
            ("provider", Some("model")),
            ("plugins", None),
            ("mcp", Some("plugins")),
        ]
        .map(|(name, into)| CatalogEntry {
            id: name.into(),
            label: name.into(),
            meta: json!({"name":name,"hint":"open a place","family":"fixture",
                    "args":{"kind":"none"},"foldedInto":into,
                    "aliases":if name == "room" { vec!["rooms"] } else { vec![] }}),
        })
        .to_vec()
    }

    #[test]
    fn folded_commands_leave_one_row_per_place_and_return_without_the_fold() {
        let mut entries = catalogue();
        let names = |entries: &[CatalogEntry]| {
            command_rows(entries, "")
                .into_iter()
                .map(|row| row.label)
                .collect::<Vec<_>>()
        };
        let shown = names(&entries);
        assert_eq!(shown.len(), 6);
        for entry in &entries {
            assert_eq!(
                shown.contains(&format!("/{}", entry.id)),
                entry.meta["foldedInto"].is_null()
            );
        }
        for entry in &mut entries {
            entry.meta.as_object_mut().unwrap().remove("foldedInto");
        }
        assert_eq!(names(&entries).len(), entries.len());
    }

    #[test]
    fn disabling_each_owner_returns_only_its_folded_commands() {
        for into in ["crew", "browse", "memory", "schedule", "model", "plugins"] {
            let mut entries = catalogue();
            for entry in &mut entries {
                if entry.meta["foldedInto"] == into {
                    entry.meta.as_object_mut().unwrap().remove("foldedInto");
                }
            }
            let shown = command_rows(&entries, "");
            for entry in &entries {
                assert_eq!(
                    shown
                        .iter()
                        .any(|row| row.label == format!("/{}", entry.id)),
                    entry.meta["foldedInto"].is_null(),
                    "{into}: {}",
                    entry.id
                );
            }
        }
    }

    #[test]
    fn folded_names_and_aliases_find_the_destination_once() {
        for query in ["agents", "room", "rooms", "roo", "rms"] {
            let found = rows(
                PaletteOpen::Commands,
                query,
                &catalogue(),
                &BTreeMap::new(),
                &[],
                &[],
                "s",
            );
            let offered = found
                .iter()
                .flat_map(|group| &group.rows)
                .collect::<Vec<_>>();
            assert_eq!(offered.len(), 1, "{query}");
            assert_eq!(offered[0].label, "/crew");
            assert_eq!(offered[0].target, Target::Command("crew".into()));
        }
    }

    #[test]
    fn folded_bindings_do_not_reappear_in_keys() {
        let keys = BTreeMap::from([
            ("alt+a".into(), "/agents".into()),
            ("alt+r".into(), "/rooms".into()),
        ]);
        assert!(rows(PaletteOpen::Keys, "", &catalogue(), &keys, &[], &[], "s").is_empty());
    }
}
