//! Commands as this surface sees them: the four it answers itself (ADR-0008
//! §6) and the dropdown that merges them with the kernel's catalogue. Every
//! other `/name` and every `!line` is submitted verbatim — the actor parses it.

use std::collections::BTreeMap;

use bingo_sdk::{ArgSpec, Catalog, CommandSpec};

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

/// Prefix matches first, then substring matches, each in catalogue order.
fn rank(partial: &str, specs: &[CommandSpec]) -> Vec<Suggestion> {
    let partial = partial.to_lowercase();
    let mut prefix = Vec::new();
    let mut substring = Vec::new();
    for spec in specs {
        let names = std::iter::once(&spec.name).chain(spec.aliases.iter());
        let Some(name) = names
            .clone()
            .find(|n| n.to_lowercase().starts_with(&partial))
            .or_else(|| names.clone().find(|n| n.to_lowercase().contains(&partial)))
        else {
            continue;
        };
        let row = Suggestion {
            value: format!("/{name} "),
            label: format!("/{name}"),
            hint: spec.hint.clone(),
            group: Group::Commands,
        };
        if name.to_lowercase().starts_with(&partial) {
            prefix.push(row);
        } else {
            substring.push(row);
        }
    }
    prefix.extend(substring);
    prefix
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
    ids.iter()
        .filter(|id| id.to_lowercase().contains(&partial.to_lowercase()))
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

    #[test]
    fn the_dropdown_ranks_prefixes_before_substrings() {
        let specs = vec![spec("compact", ArgSpec::None), spec("model", ArgSpec::None)];
        let rows = suggestions("/co", &specs, &Catalogues::new());
        assert_eq!(
            rows.iter().map(|r| r.label.clone()).collect::<Vec<_>>(),
            vec!["/compact"]
        );
        let rows = suggestions("/o", &specs, &Catalogues::new());
        assert_eq!(
            rows.iter().map(|r| r.label.clone()).collect::<Vec<_>>(),
            vec!["/compact", "/model"],
            "both only contain it, so catalogue order decides"
        );
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
