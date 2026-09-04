//! Commands as this surface sees them: the four it answers itself (ADR-0008
//! §6) and the dropdown that merges them with the kernel's catalogue. Every
//! other `/name` and every `!line` is submitted verbatim — the actor parses it.

use std::collections::BTreeMap;

use bingo_sdk::{ArgSpec, Catalog, CommandSpec};

use crate::matching;

/// The surface a command's own prompt carries (ADR-0008 §3): what re-enters
/// the session when a `/name` answers with a prompt, rather than the surface
/// the person typed at. Written down once, because two readers ask about it —
/// the quiet set in [`crate::transcript`] and [`crate::skill`].
pub const SURFACE: &str = "command";

/// A command the surface owns. Nothing here reaches the kernel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Local {
    Help,
    Clear,
    Resume(Option<String>),
    Exit,
}

/// Name and hint of each local command, in dropdown order.
const LOCAL: &[(&str, &str)] = &[
    ("help", "shortcuts and commands"),
    ("clear", "start a fresh session here"),
    ("resume", "reopen a session"),
    ("exit", "leave bingo"),
    ("quit", "leave bingo"),
];

/// The local commands as specs, so the dropdown ranks one kind of thing.
pub fn local_specs() -> Vec<CommandSpec> {
    LOCAL
        .iter()
        .map(|(name, hint)| CommandSpec {
            name: (*name).to_string(),
            aliases: Vec::new(),
            hint: (*hint).to_string(),
            args: if *name == "resume" {
                ArgSpec::Free {
                    hint: "[id]".into(),
                }
            } else {
                ArgSpec::None
            },
            instant: true,
            family: "surface".into(),
        })
        .collect()
}

/// The kernel's command catalogue, as specs. An entry whose `meta` is not a
/// spec is skipped rather than failing the fetch.
pub fn specs_from(catalog: &Catalog) -> Vec<CommandSpec> {
    catalog
        .entries
        .iter()
        .filter_map(|entry| serde_json::from_value(entry.meta.clone()).ok())
        .collect()
}

/// Which local command a line is, if any.
pub fn local(line: &str) -> Option<Local> {
    let (name, args) = split(line.trim())?;
    match name {
        "help" => Some(Local::Help),
        "clear" => Some(Local::Clear),
        "resume" => Some(Local::Resume((!args.is_empty()).then(|| args.to_string()))),
        "exit" | "quit" => Some(Local::Exit),
        _ => None,
    }
}

/// `/name args` split into its two halves — the actor's own parse (ADR-0008
/// §1), read back by whoever recognises a line rather than dispatching it.
pub fn split(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix('/')?;
    Some(match rest.split_once(char::is_whitespace) {
        Some((name, args)) => (name, args.trim()),
        None => (rest, ""),
    })
}

/// Which run of the dropdown a row belongs to. The runs are told apart by a
/// label above each of them only while there is another beside it — the list's
/// own grammar ([`crate::roster`]) — so a dropdown offering one kind of thing
/// wears no label at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// A `/` command, or a value for one: the only run its dropdown has.
    Commands,
    /// A session an `@` can reach ([`crate::mentions`]).
    Agents,
    /// A path under the session's own directory ([`crate::complete`]).
    Files,
}

impl Group {
    /// What a run is called, where there is another beside it.
    pub fn label(self) -> &'static str {
        match self {
            Group::Commands => "Commands",
            Group::Agents => "Agents",
            Group::Files => "Files",
        }
    }
}

/// One row of the dropdown: what it shows, the line it completes to, and which
/// run of the list it is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// The whole composer line this row completes to.
    pub value: String,
    pub label: String,
    pub hint: String,
    pub group: Group,
}

/// The dropdown for the line being typed: command names while the caret is
/// still in the name, catalogue values once the command is known.
pub fn suggestions(line: &str, specs: &[CommandSpec], catalogues: &Catalogues) -> Vec<Suggestion> {
    let Some(rest) = line.strip_prefix('/') else {
        return Vec::new();
    };
    match rest.split_once(char::is_whitespace) {
        None => rank(rest, specs),
        Some((name, partial)) => arguments(name, partial.trim_start(), specs, catalogues),
    }
}

/// The ids of each catalogue a command's argument may name, by the source
/// its `ArgSpec::Catalog` gives (`models`, `providers`), read once at start.
pub type Catalogues = BTreeMap<String, Vec<String>>;

/// The commands that match, best first: [`crate::matching`]'s order, which is
/// the one every list in this surface is narrowed by. A command is offered once
/// however many of its spellings match, under the one that matched best.
fn rank(partial: &str, specs: &[CommandSpec]) -> Vec<Suggestion> {
    let spelled = spellings(specs);
    let mut offered: Vec<&str> = Vec::new();
    let mut rows = Vec::new();
    for (name, spec) in matching::rank(partial, &spelled, |(name, _)| name.as_str()) {
        if offered.contains(&spec.name.as_str()) {
            continue;
        }
        offered.push(&spec.name);
        rows.push(named(name, spec));
    }
    rows
}

/// Every way a command may be typed — its own name, then each of its aliases —
/// beside the command it names, so one ranking sees all of them.
fn spellings(specs: &[CommandSpec]) -> Vec<(String, &CommandSpec)> {
    specs
        .iter()
        .flat_map(|spec| {
            std::iter::once(&spec.name)
                .chain(spec.aliases.iter())
                .map(move |name| (name.clone(), spec))
        })
        .collect()
}

/// One row of the `/` menu: the spelling that matched, and what it does.
fn named(name: &str, spec: &CommandSpec) -> Suggestion {
    Suggestion {
        value: format!("/{name} "),
        label: format!("/{name}"),
        hint: spec.hint.clone(),
        group: Group::Commands,
    }
}

/// Values for a command whose argument names a catalogue.
fn arguments(
    name: &str,
    partial: &str,
    specs: &[CommandSpec],
    catalogues: &Catalogues,
) -> Vec<Suggestion> {
    let Some(spec) = specs.iter().find(|s| s.name == name) else {
        return Vec::new();
    };
    let ArgSpec::Catalog { source } = &spec.args else {
        return Vec::new();
    };
    let Some(ids) = catalogues.get(source) else {
        return Vec::new();
    };
    matching::rank(partial, ids, String::as_str)
        .into_iter()
        .map(|id| Suggestion {
            value: format!("/{name} {id}"),
            label: id.clone(),
            hint: String::new(),
            group: Group::Commands,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::{CatalogEntry, CatalogKind};

    fn spec(name: &str, args: ArgSpec) -> CommandSpec {
        CommandSpec {
            name: name.into(),
            aliases: Vec::new(),
            hint: format!("the {name} command"),
            args,
            instant: false,
            family: "kernel".into(),
        }
    }

    #[test]
    fn the_four_local_commands_are_recognised_and_nothing_else_is() {
        assert_eq!(local("/help"), Some(Local::Help));
        assert_eq!(local("  /clear  "), Some(Local::Clear));
        assert_eq!(local("/resume"), Some(Local::Resume(None)));
        assert_eq!(
            local("/resume ses_1"),
            Some(Local::Resume(Some("ses_1".into())))
        );
        assert_eq!(local("/exit"), Some(Local::Exit));
        assert_eq!(local("/quit"), Some(Local::Exit));
        assert_eq!(local("/model fake-1"), None, "the kernel owns /model");
        assert_eq!(local("!ls"), None);
        assert_eq!(local("hello"), None);
    }

    fn labels(line: &str, specs: &[CommandSpec]) -> Vec<String> {
        suggestions(line, specs, &Catalogues::new())
            .iter()
            .map(|row| row.label.clone())
            .collect()
    }

    /// M55: the dropdown is ranked, not prefix-tested. A name a person typed
    /// the bones of is the name they meant.
    #[test]
    fn a_subsequence_of_a_command_offers_it() {
        let specs = vec![spec("compact", ArgSpec::None), spec("model", ArgSpec::None)];
        assert_eq!(labels("/mo", &specs), vec!["/model"]);
        assert_eq!(labels("/mdl", &specs), vec!["/model"], "no prefix at all");
        assert_eq!(labels("/co", &specs), vec!["/compact"]);
        assert_eq!(
            labels("/o", &specs),
            vec!["/compact", "/model"],
            "the one letter says nothing to tell them apart, so catalogue \
             order decides"
        );
        assert!(labels("/mdoel", &specs).is_empty(), "a typo is a typo");
    }

    /// A command with aliases is one row however many of its spellings match,
    /// and the row is the spelling that matched.
    #[test]
    fn an_alias_offers_the_command_once_under_the_name_that_matched() {
        let mut spec = spec("permission", ArgSpec::None);
        spec.aliases = vec!["mode".to_string()];
        let specs = vec![spec];
        assert_eq!(labels("/perm", &specs), vec!["/permission"]);
        assert_eq!(labels("/mode", &specs), vec!["/mode"]);
        assert_eq!(labels("/m", &specs), vec!["/mode"], "one row, the best of");
    }

    #[test]
    fn a_catalogue_argument_completes_from_that_catalogue() {
        let specs = vec![spec(
            "model",
            ArgSpec::Catalog {
                source: "models".into(),
            },
        )];
        let models = vec!["fake/fake-1".to_string(), "openai/gpt-5".to_string()];
        let catalogues = Catalogues::from([("models".to_string(), models)]);
        let rows = suggestions("/model fak", &specs, &catalogues);
        assert_eq!(
            rows,
            vec![Suggestion {
                value: "/model fake/fake-1".into(),
                label: "fake/fake-1".into(),
                hint: String::new(),
                group: Group::Commands,
            }]
        );
    }

    /// The values of a catalogue are ranked by the same matcher the names are,
    /// so `son` finds the model whose family it names, tightest first.
    #[test]
    fn a_catalogue_value_is_ranked_and_not_substring_tested() {
        let specs = vec![spec(
            "model",
            ArgSpec::Catalog {
                source: "models".into(),
            },
        )];
        let models = ["openai/gpt-5.4", "anthropic/claude-sonnet-5", "some/on-1"]
            .map(str::to_string)
            .to_vec();
        let catalogues = Catalogues::from([("models".to_string(), models)]);
        let labels: Vec<String> = suggestions("/model son", &specs, &catalogues)
            .iter()
            .map(|row| row.label.clone())
            .collect();
        assert_eq!(
            labels,
            vec!["anthropic/claude-sonnet-5", "some/on-1"],
            "the contiguous match leads, and the one with a gap follows"
        );
    }

    #[test]
    fn a_free_argument_offers_nothing() {
        let specs = vec![spec(
            "compact",
            ArgSpec::Free {
                hint: "[why]".into(),
            },
        )];
        assert!(suggestions("/compact any", &specs, &Catalogues::new()).is_empty());
    }

    #[test]
    fn a_line_that_is_not_a_command_has_no_dropdown() {
        assert!(suggestions("hello", &local_specs(), &Catalogues::new()).is_empty());
        assert!(suggestions("!ls", &local_specs(), &Catalogues::new()).is_empty());
    }

    #[test]
    fn catalogue_entries_that_are_not_specs_are_skipped() {
        let catalog = Catalog {
            kind: CatalogKind::Commands,
            entries: vec![
                CatalogEntry {
                    id: "model".into(),
                    label: "model".into(),
                    meta: serde_json::to_value(spec("model", ArgSpec::None)).unwrap(),
                },
                CatalogEntry {
                    id: "junk".into(),
                    label: "junk".into(),
                    meta: serde_json::Value::Null,
                },
            ],
        };
        let specs = specs_from(&catalog);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "model");
    }
}
