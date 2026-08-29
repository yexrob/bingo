//! The committed contract. `document()` is the whole wire in JSON Schema: every
//! type under `$defs`, the method table and the notification table (ADR-0007).
//!
//! A test regenerates it and fails on any difference, so the file in `schema/`
//! is never a copy that drifted — it is this function, written down.

use std::collections::BTreeMap;

use bingo_sdk::ErrorCode;
use schemars::{SchemaGenerator, generate::SchemaSettings};
use serde_json::{Map, Value, json};

use crate::methods::{METHODS, NOTIFICATIONS, PROTOCOL, Ref, schema_of};

/// Types a client needs that no method names. `ErrorCode` is `error.data.code`.
static UNNAMED: &[Ref] = &[schema_of::<ErrorCode>];

pub fn document() -> Value {
    let mut generator = generator();
    let methods = method_table(&mut generator);
    let notifications = notification_table(&mut generator);
    for name in UNNAMED {
        name(&mut generator);
    }
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "bingo rpc",
        "protocol": PROTOCOL,
        "$defs": sorted(generator.take_definitions(true)),
        "methods": methods,
        "notifications": notifications,
    })
}

/// The `$ref` path is part of the committed file, so it is set here rather than
/// inherited from whatever schemars defaults to.
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

/// `serde_json` keeps insertion order; sorting keeps the committed file's diffs
/// small when a method is added.
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
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/rpc.json")
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
            "schema/rpc.json is out of date: run BINGO_UPDATE_SCHEMA=1 cargo test -p bingo-surface-rpc"
        );
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
    fn every_method_reference_resolves() {
        let document = document();
        let definitions = &document["$defs"];
        let mut found = Vec::new();
        references(&document["methods"], &mut found);
        references(&document["notifications"], &mut found);
        assert_eq!(found.len(), METHODS.len() * 2 + NOTIFICATIONS.len());
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
}
