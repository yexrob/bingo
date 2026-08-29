//! The pure merge. Every key path is folded across the layers, lowest
//! first, with the rule the claim gave that path; an explicit `null` in a
//! higher layer clears everything below it; objects merge field by field.

use std::collections::{BTreeMap, HashMap};

use bingo_sdk::Merge;
use serde_json::{Map, Value};

use super::{Claim, KERNEL_KEYS, KernelSettings, Layer, Merged, SettingsError, UnknownKey};

/// A value with the layer it came from, for messages.
type Sourced<'a> = (&'a str, &'a Value);

pub fn merge(layers: &[Layer], claims: &[Claim]) -> Result<Merged, SettingsError> {
    let owners = owners(claims)?;
    let modes = modes(claims);
    let unknown = unknown_keys(layers, &owners);
    let objects: Vec<(&str, &Map<String, Value>)> = layers
        .iter()
        .map(|l| (l.source.as_str(), &l.value))
        .collect();
    let merged = fold_objects("", &objects, &modes)?;
    let kernel = kernel_settings(&merged, layers)?;
    let plugins = slices(&merged, claims);
    Ok(Merged {
        kernel,
        plugins,
        unknown,
    })
}

/// Top-level key → the plugin that claimed it; the kernel keys are taken.
fn owners(claims: &[Claim]) -> Result<HashMap<String, String>, SettingsError> {
    let mut owners: HashMap<String, String> = KERNEL_KEYS
        .iter()
        .map(|k| ((*k).to_string(), "kernel".to_string()))
        .collect();
    for claim in claims {
        for root in claim.roots() {
            if let Some(first) = owners.get(root)
                && first != &claim.plugin
            {
                return Err(SettingsError::Conflict {
                    key: root.to_string(),
                    first: first.clone(),
                    second: claim.plugin.clone(),
                });
            }
            owners.insert(root.to_string(), claim.plugin.clone());
        }
    }
    Ok(owners)
}

fn modes(claims: &[Claim]) -> HashMap<&str, Merge> {
    claims
        .iter()
        .flat_map(|c| c.keys.iter().map(|(k, m)| (k.as_str(), *m)))
        .collect()
}

fn unknown_keys(layers: &[Layer], owners: &HashMap<String, String>) -> Vec<UnknownKey> {
    layers
        .iter()
        .flat_map(|layer| {
            layer
                .value
                .keys()
                .filter(|k| !owners.contains_key(k.as_str()))
                .map(|k| UnknownKey {
                    source: layer.source.clone(),
                    key: k.clone(),
                })
        })
        .collect()
}

fn child_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

fn fold_objects(
    path: &str,
    objects: &[(&str, &Map<String, Value>)],
    modes: &HashMap<&str, Merge>,
) -> Result<Map<String, Value>, SettingsError> {
    let mut keys: Vec<&String> = Vec::new();
    for (_, object) in objects {
        for key in object.keys() {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    let mut out = Map::new();
    for key in keys {
        let values: Vec<Sourced<'_>> = objects
            .iter()
            .filter_map(|(source, object)| object.get(key).map(|v| (*source, v)))
            .collect();
        if let Some(value) = fold_value(&child_path(path, key), &values, modes)? {
            out.insert(key.clone(), value);
        }
    }
    Ok(out)
}

fn fold_value(
    path: &str,
    values: &[Sourced<'_>],
    modes: &HashMap<&str, Merge>,
) -> Result<Option<Value>, SettingsError> {
    // A `null` clears everything below it.
    let start = values
        .iter()
        .rposition(|(_, v)| v.is_null())
        .map_or(0, |i| i + 1);
    let live = &values[start..];
    let Some((_, last)) = live.last() else {
        return Ok(None);
    };
    if live.iter().all(|(_, v)| v.is_object()) {
        let objects: Vec<(&str, &Map<String, Value>)> = live
            .iter()
            .filter_map(|(s, v)| v.as_object().map(|o| (*s, o)))
            .collect();
        return Ok(Some(Value::Object(fold_objects(path, &objects, modes)?)));
    }
    match modes.get(path).copied().unwrap_or(Merge::Replace) {
        Merge::Replace => Ok(Some((*last).clone())),
        Merge::Accumulate => accumulate(path, live).map(Some),
        Merge::ByName => by_name(path, live).map(Some),
    }
}

fn arrays<'a>(path: &str, values: &[Sourced<'a>]) -> Result<Vec<&'a Value>, SettingsError> {
    let mut out = Vec::new();
    for (source, value) in values {
        let Some(items) = value.as_array() else {
            return Err(SettingsError::Type {
                key: path.to_string(),
                layer: (*source).to_string(),
                message: "expected a list".into(),
            });
        };
        out.extend(items.iter());
    }
    Ok(out)
}

/// Lists concatenate, lowest layer first, keeping the first copy of a repeat.
fn accumulate(path: &str, values: &[Sourced<'_>]) -> Result<Value, SettingsError> {
    let mut out: Vec<Value> = Vec::new();
    for item in arrays(path, values)? {
        if !out.contains(item) {
            out.push(item.clone());
        }
    }
    Ok(Value::Array(out))
}

fn entry_name(path: &str, item: &Value) -> Result<String, SettingsError> {
    item.get("name")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| SettingsError::Type {
            key: path.to_string(),
            layer: "merged".into(),
            message: "every entry needs a string `name` or `id`".into(),
        })
}

/// Named entries: a higher layer's entry replaces the lower one in place.
fn by_name(path: &str, values: &[Sourced<'_>]) -> Result<Value, SettingsError> {
    let mut order: Vec<String> = Vec::new();
    let mut entries: BTreeMap<String, Value> = BTreeMap::new();
    for item in arrays(path, values)? {
        let name = entry_name(path, item)?;
        if !entries.contains_key(&name) {
            order.push(name.clone());
        }
        entries.insert(name, item.clone());
    }
    Ok(Value::Array(
        order
            .into_iter()
            .filter_map(|n| entries.remove(&n))
            .collect(),
    ))
}

fn last_source(layers: &[Layer], key: &str) -> String {
    layers
        .iter()
        .rev()
        .find(|l| l.value.contains_key(key))
        .map_or_else(|| "settings".into(), |l| l.source.clone())
}

fn typed<T: serde::de::DeserializeOwned>(
    merged: &Map<String, Value>,
    layers: &[Layer],
    key: &str,
) -> Result<Option<T>, SettingsError> {
    let Some(value) = merged.get(key) else {
        return Ok(None);
    };
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|e| SettingsError::Type {
            key: key.to_string(),
            layer: last_source(layers, key),
            message: e.to_string(),
        })
}

fn kernel_settings(
    merged: &Map<String, Value>,
    layers: &[Layer],
) -> Result<KernelSettings, SettingsError> {
    Ok(KernelSettings {
        provider: typed(merged, layers, "provider")?,
        model: typed(merged, layers, "model")?,
        thinking: typed(merged, layers, "thinking")?,
        max_tokens: typed(merged, layers, "maxTokens")?,
        models: typed(merged, layers, "models")?.unwrap_or_default(),
    })
}

fn slices(merged: &Map<String, Value>, claims: &[Claim]) -> BTreeMap<String, Value> {
    claims
        .iter()
        .map(|claim| {
            let mut slice = Map::new();
            for root in claim.roots() {
                if let Some(value) = merged.get(root) {
                    slice.insert(root.to_string(), value.clone());
                }
            }
            (claim.plugin.clone(), Value::Object(slice))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::Effort;
    use serde_json::json;

    fn layer(source: &str, value: Value) -> Layer {
        let Value::Object(map) = value else {
            panic!("layers are objects")
        };
        Layer::new(source, map)
    }

    fn permissions_claim() -> Claim {
        Claim {
            plugin: "bingo.permissions".into(),
            keys: vec![
                ("permissions.defaultMode".into(), Merge::Replace),
                ("permissions.allow".into(), Merge::Accumulate),
                ("permissions.deny".into(), Merge::Accumulate),
            ],
        }
    }

    fn anthropic_claim() -> Claim {
        Claim {
            plugin: "bingo.provider.anthropic".into(),
            keys: vec![("anthropic".into(), Merge::Replace)],
        }
    }

    #[test]
    fn model_declarations_merge_by_field_across_layers_and_refuse_a_typo() {
        let merged = merge(
            &[
                layer(
                    "user",
                    json!({"models": {"openai/x": {"contextWindow": 128000}}}),
                ),
                layer(
                    "project",
                    json!({"models": {"openai/x": {"maxOutput": 8192}, "anthropic/y": {"images": false}}}),
                ),
            ],
            &[],
        )
        .unwrap();
        let x = &merged.kernel.models["openai/x"];
        assert_eq!(
            (x.context_window, x.max_output),
            (Some(128_000), Some(8_192))
        );
        assert_eq!(merged.kernel.models["anthropic/y"].images, Some(false));

        let err = merge(
            &[layer(
                "project",
                json!({"models": {"openai/x": {"contextWindwo": 1}}}),
            )],
            &[],
        )
        .unwrap_err();
        assert!(
            matches!(&err, SettingsError::Type { key, layer, .. } if key == "models" && layer == "project"),
            "{err}"
        );
    }

    #[test]
    fn higher_layers_replace_and_objects_merge_field_by_field() {
        let merged = merge(
            &[
                layer(
                    "user",
                    json!({"model": "u", "provider": "p", "anthropic": {"apiKey": "k"}}),
                ),
                layer(
                    "project",
                    json!({"model": "m", "anthropic": {"baseUrl": "http://x"}}),
                ),
                layer(
                    "cli",
                    json!({"model": "c", "thinking": "high", "maxTokens": 4096}),
                ),
            ],
            &[anthropic_claim()],
        )
        .unwrap();
        assert_eq!(merged.kernel.model.as_deref(), Some("c"));
        assert_eq!(merged.kernel.provider.as_deref(), Some("p"));
        assert_eq!(merged.kernel.thinking, Some(Effort::High));
        assert_eq!(merged.kernel.max_tokens, Some(4096));
        assert_eq!(
            merged.plugins["bingo.provider.anthropic"],
            json!({"anthropic": {"apiKey": "k", "baseUrl": "http://x"}})
        );
        assert!(merged.unknown.is_empty());
    }

    #[test]
    fn null_clears_the_layers_below_it() {
        let merged = merge(
            &[
                layer(
                    "user",
                    json!({"model": "u", "permissions": {"allow": ["Read"]}}),
                ),
                layer(
                    "project",
                    json!({"model": null, "permissions": {"allow": null}}),
                ),
                layer("local", json!({"permissions": {"allow": ["Bash(ls:*)"]}})),
            ],
            &[permissions_claim()],
        )
        .unwrap();
        assert_eq!(merged.kernel.model, None);
        assert_eq!(
            merged.plugins["bingo.permissions"]["permissions"]["allow"],
            json!(["Bash(ls:*)"])
        );
    }

    #[test]
    fn accumulate_concatenates_in_layer_order_without_repeats() {
        let merged = merge(
            &[
                layer(
                    "user",
                    json!({"permissions": {"allow": ["A", "B"], "defaultMode": "plan"}}),
                ),
                layer(
                    "project",
                    json!({"permissions": {"allow": ["B", "C"], "defaultMode": "default"}}),
                ),
            ],
            &[permissions_claim()],
        )
        .unwrap();
        let permissions = &merged.plugins["bingo.permissions"]["permissions"];
        assert_eq!(permissions["allow"], json!(["A", "B", "C"]));
        assert_eq!(permissions["defaultMode"], json!("default"));
    }

    #[test]
    fn accumulate_rejects_a_non_list_with_its_source() {
        let err = merge(
            &[layer("user", json!({"permissions": {"allow": "Read"}}))],
            &[permissions_claim()],
        )
        .unwrap_err();
        assert!(
            matches!(&err, SettingsError::Type { key, layer, .. } if key == "permissions.allow" && layer == "user"),
            "{err}"
        );
    }

    #[test]
    fn by_name_replaces_entries_in_place() {
        let claim = Claim {
            plugin: "p".into(),
            keys: vec![("servers".into(), Merge::ByName)],
        };
        let merged = merge(
            &[
                layer(
                    "user",
                    json!({"servers": [{"name": "x", "v": 1}, {"name": "y", "v": 1}]}),
                ),
                layer(
                    "project",
                    json!({"servers": [{"name": "x", "v": 2}, {"name": "z", "v": 3}]}),
                ),
            ],
            &[claim],
        )
        .unwrap();
        assert_eq!(
            merged.plugins["p"]["servers"],
            json!([{"name": "x", "v": 2}, {"name": "y", "v": 1}, {"name": "z", "v": 3}])
        );
    }

    #[test]
    fn unclaimed_keys_are_reported_with_their_source() {
        let merged = merge(
            &[
                layer("user", json!({"model": "m", "theme": "dark"})),
                layer("project", json!({"permisions": {}})),
            ],
            &[permissions_claim()],
        )
        .unwrap();
        assert_eq!(
            merged.unknown,
            vec![
                UnknownKey {
                    source: "user".into(),
                    key: "theme".into()
                },
                UnknownKey {
                    source: "project".into(),
                    key: "permisions".into()
                },
            ]
        );
    }

    #[test]
    fn a_key_claimed_twice_or_a_kernel_key_claimed_is_a_conflict() {
        let other = Claim {
            plugin: "other".into(),
            keys: vec![("permissions.extra".into(), Merge::Replace)],
        };
        let err = merge(&[], &[permissions_claim(), other]).unwrap_err();
        assert!(matches!(err, SettingsError::Conflict { ref key, .. } if key == "permissions"));

        let kernel = Claim {
            plugin: "p".into(),
            keys: vec![("model".into(), Merge::Replace)],
        };
        let err = merge(&[], &[kernel]).unwrap_err();
        assert!(matches!(err, SettingsError::Conflict { ref first, .. } if first == "kernel"));
    }

    #[test]
    fn a_kernel_key_of_the_wrong_type_names_the_layer_that_set_it() {
        let err = merge(
            &[
                layer("user", json!({"maxTokens": 10})),
                layer("project", json!({"maxTokens": "lots"})),
            ],
            &[],
        )
        .unwrap_err();
        assert!(
            matches!(&err, SettingsError::Type { key, layer, .. } if key == "maxTokens" && layer == "project"),
            "{err}"
        );
    }
}
