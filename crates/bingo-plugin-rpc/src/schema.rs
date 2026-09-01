//! The committed contract. `document()` is the whole of what a plugin process
//! writes against: the manifest it ships, every type under `$defs`, the method
//! table and the notification table (ADR-0015 §2, the ADR-0007 pattern).
//!
//! A test regenerates it and fails on any difference, so `schema/plugin.json`
//! is never a copy that drifted — it is this function, written down. Nothing
//! here is hand-authored: a third party who cannot read Rust reads that file.

use std::collections::BTreeMap;

use schemars::{SchemaGenerator, generate::SchemaSettings};
use serde_json::{Map, Value, json};

use crate::manifest::Manifest;
use crate::wire::{METHODS, NOTIFICATIONS, PROTOCOL, Ref, schema_of};

/// Types the method table does not name. The manifest is the first thing a
/// plugin author writes and the last thing the wire mentions.
static UNNAMED: &[Ref] = &[schema_of::<Manifest>];

pub fn document() -> Value {
    let mut generator = generator();
    let methods = method_table(&mut generator);
    let notifications = notification_table(&mut generator);
    for name in UNNAMED {
        name(&mut generator);
    }
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "bingo plugin",
        "protocol": PROTOCOL,
        "$defs": sorted(generator.take_definitions(true)),
        "manifest": { "$ref": "#/$defs/Manifest" },
        "methods": methods,
        "notifications": notifications,
    })
}

/// The `$ref` path is part of the committed file, so it is set here rather
/// than inherited from whatever schemars defaults to.
fn generator() -> SchemaGenerator {
    SchemaSettings::draft2020_12()
        .with(|settings| settings.definitions_path = "#/$defs/".into())
        .into_generator()
}

fn method_table(generator: &mut SchemaGenerator) -> Map<String, Value> {
    let mut table = Map::new();
    for &(name, params, result) in METHODS {
        let params = params(generator).to_value();
        let result = result(generator).to_value();
        table.insert(
            name.to_owned(),
            json!({ "params": params, "result": result }),
        );
    }
    table
}

fn notification_table(generator: &mut SchemaGenerator) -> Map<String, Value> {
    let mut table = Map::new();
    for &(name, params) in NOTIFICATIONS {
        let params = params(generator).to_value();
        table.insert(name.to_owned(), json!({ "params": params }));
    }
    table
}

/// `serde_json` keeps insertion order; sorting keeps the committed file's
/// diffs small when a method is added.
fn sorted(definitions: Map<String, Value>) -> Map<String, Value> {
    definitions
        .into_iter()
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn committed() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/plugin.json")
    }

    fn rendered() -> String {
        format!(
            "{}\n",
            serde_json::to_string_pretty(&document()).expect("the document serialises")
        )
    }

    #[test]
    fn the_committed_schema_is_this_document() {
        let path = committed();
        if std::env::var_os("BINGO_UPDATE_SCHEMA").is_some() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("the schema directory");
            }
            std::fs::write(&path, rendered()).expect("the schema is writable");
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            committed,
            rendered(),
            "schema/plugin.json is out of date: run BINGO_UPDATE_SCHEMA=1 cargo test -p bingo-plugin-rpc"
        );
    }

    fn keys(table: &Value) -> Vec<String> {
        table
            .as_object()
            .map(|table| table.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// ADR-0015 §3 pinned the wire at four methods with a literal; ADR-0030 §6
    /// makes the schema the count. What the host speaks is what the committed
    /// document says it speaks — a method the file does not name is one no
    /// plugin author can find, however many the table holds.
    #[test]
    fn the_committed_schema_names_every_method_and_every_notification() {
        let file = std::fs::read_to_string(committed()).unwrap_or_default();
        let document: Value = serde_json::from_str(&file).expect("the committed schema is json");
        let mut spoken: Vec<String> = METHODS.iter().map(|method| method.0.to_owned()).collect();
        spoken.sort();
        let mut named = keys(&document["methods"]);
        named.sort();
        assert_eq!(named, spoken);
        let mut announced: Vec<String> = NOTIFICATIONS
            .iter()
            .map(|notification| notification.0.to_owned())
            .collect();
        announced.sort();
        let mut listed = keys(&document["notifications"]);
        listed.sort();
        assert_eq!(listed, announced);
        assert_eq!(document["protocol"], PROTOCOL);
    }

    /// The pin ADR-0032 §4 rests on. `HookOutcome` has no `Allow`, and the
    /// wire adds none: everything a hook may answer with is in these three
    /// definitions, and there is no allowing word anywhere in them. (A
    /// person's own permission answer says `allowOnce` elsewhere in the
    /// document; that is the gate's vocabulary, which no process writes.)
    #[test]
    fn the_outcome_a_hook_writes_has_no_allow_to_say() {
        let document = document();
        let kinds: Vec<&str> = document["$defs"]["HookOutcome"]["oneOf"]
            .as_array()
            .map(|variants| {
                variants
                    .iter()
                    .filter_map(|v| v["properties"]["kind"]["const"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(kinds, ["continue", "deny", "ask", "block", "redirect"]);
        for name in ["HookOutcome", "HookDecideResult", "HookValue"] {
            let mut shape = document["$defs"][name].clone();
            undescribed(&mut shape);
            let written = shape.to_string().to_lowercase();
            for spelling in ["allow", "approve", "permit"] {
                assert!(
                    !written.contains(spelling),
                    "{name} spells {spelling}: {written}"
                );
            }
        }
    }

    /// The shape without the prose about it: a doc comment saying there is no
    /// `Allow` must not be what makes this test pass.
    fn undescribed(value: &mut Value) {
        match value {
            Value::Object(map) => {
                map.remove("description");
                map.values_mut().for_each(undescribed);
            }
            Value::Array(items) => items.iter_mut().for_each(undescribed),
            _ => {}
        }
    }

    /// Every key under any `properties`, wherever it sits in the document.
    fn properties(value: &Value, found: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    if key == "properties"
                        && let Value::Object(names) = child
                    {
                        found.extend(names.keys().cloned());
                    }
                    properties(child, found);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| properties(item, found)),
            _ => {}
        }
    }

    fn is_camel_case(name: &str) -> bool {
        let mut characters = name.chars();
        characters
            .next()
            .is_some_and(|first| first.is_ascii_lowercase())
            && characters.all(|character| character.is_ascii_alphanumeric())
    }

    #[test]
    fn every_property_is_camel_case() {
        let mut names = Vec::new();
        properties(&document(), &mut names);
        assert!(!names.is_empty(), "the document has properties to check");
        let mut snake: Vec<String> = names
            .into_iter()
            .filter(|name| !is_camel_case(name))
            .collect();
        snake.sort_unstable();
        snake.dedup();
        assert!(snake.is_empty(), "snake_case on the wire: {snake:?}");
    }

    fn references(value: &Value, found: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    match (key.as_str(), child) {
                        ("$ref", Value::String(reference)) => found.push(reference.clone()),
                        _ => references(child, found),
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| references(item, found)),
            _ => {}
        }
    }

    #[test]
    fn every_named_reference_resolves() {
        let document = document();
        let definitions = &document["$defs"];
        let mut found = Vec::new();
        references(&document["manifest"], &mut found);
        references(&document["methods"], &mut found);
        references(&document["notifications"], &mut found);
        assert_eq!(found.len(), 1 + METHODS.len() * 2 + NOTIFICATIONS.len());
        for reference in found {
            let name = reference
                .strip_prefix("#/$defs/")
                .unwrap_or_else(|| panic!("{reference} is not a $defs reference"));
            assert!(
                definitions.get(name).is_some(),
                "{reference} resolves to nothing"
            );
        }
    }

    /// The document is what a non-Rust author reads: the sdk's own types have
    /// to be in it, not just the envelopes around them.
    #[test]
    fn the_sdk_types_a_plugin_answers_with_are_all_defined() {
        let document = document();
        for name in [
            "Manifest",
            "Entry",
            "ToolSpec",
            "CommandSpec",
            "ToolOutput",
            "CommandOutcome",
            "Completion",
            "View",
            "ContextPiece",
            "Placement",
            "SystemBlock",
            "CompactReason",
            "Compaction",
            "Item",
            "SessionSummary",
            "ModelRequest",
            "ModelEvent",
            "ModelInfo",
            "ProviderError",
            "EndpointCapabilities",
            "HookPoint",
            "HookOutcome",
            "Phase",
            "Input",
            "ToolCall",
            "Frame",
        ] {
            assert!(
                document["$defs"].get(name).is_some(),
                "{name} is missing from $defs"
            );
        }
    }
}
