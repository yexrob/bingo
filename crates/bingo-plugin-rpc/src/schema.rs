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

use crate::doors;
use crate::manifest::Manifest;
use crate::wire::{METHODS, NOTIFICATIONS, PROTOCOL, Ref, schema_of};

/// Types the method table does not name. The manifest is the first thing a
/// plugin author writes and the last thing the wire mentions.
static UNNAMED: &[Ref] = &[
    schema_of::<bingo_sdk::CommandFold>,
    schema_of::<bingo_sdk::PluginState>,
    schema_of::<bingo_sdk::Approval>,
    schema_of::<Manifest>,
    schema_of::<bingo_sdk::component::Props>,
    schema_of::<bingo_sdk::render::RenderRequest>,
    schema_of::<crate::context::Context>,
    schema_of::<crate::wire::ServiceMethod>,
];

fn dollar() -> &'static Value {
    static DOLLAR: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
        serde_json::from_str(include_str!("../../../schema/dollar.json")).unwrap_or(Value::Null)
    });
    &DOLLAR
}
pub(crate) fn kernel_methods() -> Map<String, Value> {
    dollar()["methods"]
        .as_object()
        .into_iter()
        .flat_map(|methods| methods.iter())
        .filter_map(|(name, row)| {
            row["door"].get("kernel").map(|_| {
                (
                    format!("host/{}", name.replace('.', "/")),
                    json!({"params":row["input"], "result":row["result"]}),
                )
            })
        })
        .collect()
}

pub fn document() -> Value {
    let mut generator = generator();
    let methods = method_table(&mut generator);
    let notifications = notification_table(&mut generator);
    let opened = table(&mut generator, doors::METHODS);
    for name in UNNAMED {
        name(&mut generator);
    }
    let mut definitions = dollar()["$defs"].as_object().cloned().unwrap_or_default();
    definitions.remove("DollarInputs");
    definitions.remove("DollarResults");
    definitions.extend(generator.take_definitions(true));
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "bingo plugin",
        "protocol": PROTOCOL,
        "capabilities": crate::wire::capabilities(),
        "$defs": sorted(definitions),
        "requestContext": { "field": "_bingo", "schema": { "$ref": "#/$defs/HostCallContext" } },
        "manifest": { "$ref": "#/$defs/Manifest" },
        "methods": methods,
        "notifications": notifications,
        "hostService": { "key": doors::KEY, "methods": opened },
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
    let mut methods = kernel_methods();
    methods.extend(table(generator, METHODS));
    methods
}

/// One name → `{params, result}` per entry: the wire's methods and the host
/// service's doors are the same shape, so they are written down the same way.
fn table(generator: &mut SchemaGenerator, methods: &[crate::wire::Method]) -> Map<String, Value> {
    let mut table = Map::new();
    for &(name, params, result) in methods {
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

    #[test]
    fn command_folds_are_part_of_the_plugin_contract() {
        let schema = document();
        let fold = &schema["$defs"]["CommandFold"];
        assert_eq!(fold["properties"]["into"]["type"], "string");
        assert_eq!(fold["properties"]["names"]["items"]["type"], "string");
        for field in ["into", "names"] {
            assert!(
                fold["properties"][field]["description"]
                    .as_str()
                    .is_some_and(|description| !description.trim().is_empty()),
                "CommandFold.{field} must document the generated contract"
            );
        }
        assert_eq!(fold["required"], json!(["into", "names"]));
    }

    #[test]
    fn protocol_two_names_the_new_doors() {
        let schema = document();
        assert_eq!(schema["protocol"], 2);
        for method in [
            "host/place",
            "host/ui/invalidate",
            "host/extend",
            "host/deliver",
            "host/state/get",
            "host/state/set",
            "host/model/complete",
            "intercept/tool.call",
            "intercept/tool.call/after",
        ] {
            assert!(schema["methods"].get(method).is_some(), "missing {method}");
        }
        assert!(schema["methods"].get("hook/decide").is_none());
        for field in ["pluginName", "pluginRoot", "capabilities"] {
            assert!(
                schema["$defs"]["InitializeParams"]["properties"]
                    .get(field)
                    .is_some(),
                "missing {field}"
            );
        }
    }

    #[test]
    fn tool_call_wire_answers_include_deny_only_results() {
        let schema = document();
        let mut pending = Vec::new();
        references(
            &schema["methods"]["intercept/tool.call"]["result"],
            &mut pending,
        );
        let mut seen = std::collections::BTreeSet::new();
        while let Some(reference) = pending.pop() {
            if seen.insert(reference.clone()) {
                references(schema.pointer(&reference[1..]).unwrap(), &mut pending);
            }
        }
        assert!(
            seen.iter().any(|reference| {
                schema
                    .pointer(&reference[1..])
                    .unwrap()
                    .to_string()
                    .contains("\"deny\"")
            }),
            "a deny-only answer must be in the wire contract"
        );
    }

    fn frozen() -> Value {
        let path = committed().with_file_name("plugin-2.json");
        serde_json::from_str(&std::fs::read_to_string(path).expect("protocol 2 is frozen")).unwrap()
    }

    // Descriptions are prose; all structural keys and every existing alternative hold.
    fn preserves(before: &Value, after: &Value) -> bool {
        match (&spelled(before), &spelled(after)) {
            (Value::Object(before), Value::Object(after)) => before.iter().all(|(key, value)| {
                matches!(key.as_str(), "description" | "title" | "$comment")
                    || after.get(key).is_some_and(|next| preserves(value, next))
            }),
            (Value::Array(before), Value::Array(after)) => before
                .iter()
                .all(|value| after.iter().any(|next| preserves(value, next))),
            _ => before == after,
        }
    }

    // One schema, two spellings: a documented unit variant is a `const`
    // where a bare one is a one-value `enum`, and a documented `true` is `{}`.
    fn spelled(schema: &Value) -> Value {
        match schema {
            Value::Bool(true) => json!({}),
            Value::Object(object) => match object.get("enum").and_then(Value::as_array) {
                Some(values) if values.len() == 1 && !object.contains_key("const") => {
                    let mut object = object.clone();
                    object.remove("enum");
                    object.insert("const".into(), values[0].clone());
                    Value::Object(object)
                }
                _ => schema.clone(),
            },
            _ => schema.clone(),
        }
    }

    #[test]
    fn protocol_two_keeps_every_frozen_shape() {
        assert!(
            preserves(&frozen(), &document()),
            "protocol 2 removed or retyped a frozen shape"
        );
    }

    #[test]
    fn the_manifest_freeze_keeps_both_launch_alternatives() {
        let before = frozen();
        assert!(before["$defs"]["Manifest"]["oneOf"].is_array());
        let mut after = before.clone();
        after["$defs"]["Manifest"]["oneOf"]
            .as_array_mut()
            .unwrap()
            .pop();
        assert!(!preserves(&before, &after));
    }

    #[test]
    fn the_freeze_rejects_removal_and_retyping_but_allows_additions() {
        let before = frozen();
        for pointer in [
            "/methods/host~1place",
            "/$defs/InitializeParams/properties/pluginName",
        ] {
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            let key = key.replace("~1", "/");
            let mut after = before.clone();
            after
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(&key);
            assert!(!preserves(&before, &after), "removed {pointer}");
        }
        let mut after = before.clone();
        after["$defs"]["InitializeParams"]["properties"]["pluginName"]["type"] = json!("integer");
        assert!(!preserves(&before, &after));
        let mut after = before.clone();
        after["capabilities"].as_array_mut().unwrap().pop();
        assert!(!preserves(&before, &after));
        let mut after = before.clone();
        after["methods"]["future/verb"] = json!({});
        after["$defs"]["InitializeParams"]["properties"]["future"] = json!({"type":"string"});
        after["capabilities"]
            .as_array_mut()
            .unwrap()
            .push(json!("future/verb"));
        assert!(preserves(&before, &after));
    }

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
            let mut baseline = frozen();
            amend_interception(&mut baseline, &document());
            amend_elements(&mut baseline, &document());
            // M142: the authorized pre-release events catalogue kind.
            baseline["$defs"]["CatalogKind"] = document()["$defs"]["CatalogKind"].clone();
            std::fs::write(
                path.with_file_name("plugin-2.json"),
                format!("{}\n", serde_json::to_string_pretty(&baseline).unwrap()),
            )
            .expect("the authorized pre-release baseline is writable");
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            committed,
            rendered(),
            "schema/plugin.json is out of date: run BINGO_UPDATE_SCHEMA=1 cargo test -p bingo-plugin-rpc"
        );
    }

    // The M146 pre-release amendment is restricted to these interception roots.
    fn amend_interception(baseline: &mut Value, current: &Value) {
        let mut pending = Vec::new();
        for method in [
            "intercept/command.run",
            "intercept/command.run/after",
            "intercept/tool.call",
            "intercept/tool.call/after",
        ] {
            baseline["methods"][method] = current["methods"][method].clone();
            references(&current["methods"][method], &mut pending);
        }
        let mut seen = std::collections::BTreeSet::new();
        while let Some(reference) = pending.pop() {
            let Some(name) = reference.strip_prefix("#/$defs/") else {
                continue;
            };
            if seen.insert(name.to_owned()) {
                baseline["$defs"][name] = current["$defs"][name].clone();
                references(&current["$defs"][name], &mut pending);
            }
        }
    }

    fn amend_elements(baseline: &mut Value, current: &Value) {
        // ADR-0082: a pulled placement is completed by its asking surface.
        for name in ["View", "Slot", "Shown", "Component"] {
            baseline["$defs"][name] = current["$defs"][name].clone();
        }
        if let Some(method) = current["methods"].get("host/ui/invalidate") {
            baseline["methods"]["host/ui/invalidate"] = method.clone();
        }
        loop {
            let mut refs = Vec::new();
            references(baseline, &mut refs);
            let missing: Vec<_> = refs
                .into_iter()
                .filter_map(|name| name.strip_prefix("#/$defs/").map(str::to_owned))
                .filter(|name| baseline["$defs"].get(name).is_none())
                .collect();
            if missing.is_empty() {
                break;
            }
            for name in missing {
                assert!(current["$defs"].get(&name).is_some(), "missing {name}");
                baseline["$defs"][&name] = current["$defs"][&name].clone();
            }
        }
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
        spoken.extend(kernel_methods().into_iter().map(|(name, _)| name));
        spoken.sort();
        spoken.dedup();
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

    /// The host's own service is in the document for the same reason the
    /// methods are: a door a plugin author cannot find is a door that does not
    /// exist. It is not a wire method — it is reached through `service/call` —
    /// so it is written down beside them, not among them.
    #[test]
    fn the_committed_schema_names_the_host_s_own_service_and_its_doors() {
        let file = std::fs::read_to_string(committed()).unwrap_or_default();
        let document: Value = serde_json::from_str(&file).expect("the committed schema is json");
        assert_eq!(document["hostService"]["key"], json!(doors::KEY));
        let mut named = keys(&document["hostService"]["methods"]);
        named.sort();
        let mut spoken: Vec<String> = doors::METHODS.iter().map(|d| d.0.to_owned()).collect();
        spoken.sort();
        assert_eq!(named, spoken);
        assert!(
            !keys(&document["methods"]).iter().any(|m| m == "ask"),
            "a door is not a wire method"
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
            .filter(|name| {
                !matches!(name.as_str(), "tool_use_id" | "Ok" | "Err") && !is_camel_case(name)
            })
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
        references(&document["hostService"], &mut found);
        // Unit results are inline null schemas, so not every envelope is a reference.
        references(definitions, &mut found);
        assert!(!found.is_empty());
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
