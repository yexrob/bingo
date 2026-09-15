//! What a write actually changes, and the changing of it. A settings file is
//! a person's document (ADR-0058 §3): the value a command asks for is diffed
//! against what the file already says, and only the leaves that differ are
//! touched, so every comment, blank line and ordering elsewhere survives.

use serde_json::{Map, Value};
use toml_edit::{DocumentMut, Item, Table, TableLike, Value as Written};

use super::SettingsError;

/// One leaf, and what is to become of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// The keys from the root down, so `["openai", "apiKey"]`.
    pub path: Vec<String>,
    pub op: Op,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Set(Value),
    Remove,
}

/// Every leaf that differs, in the order the new document names them, with
/// the keys it dropped last. Objects are recursed into so that a sibling is
/// never rewritten; an array or a scalar is one leaf and is replaced whole.
pub fn diff(old: &Map<String, Value>, new: &Map<String, Value>) -> Vec<Change> {
    let mut changes = Vec::new();
    collect(&mut Vec::new(), old, new, &mut changes);
    changes
}

fn collect(
    at: &mut Vec<String>,
    old: &Map<String, Value>,
    new: &Map<String, Value>,
    changes: &mut Vec<Change>,
) {
    for (key, value) in new {
        at.push(key.clone());
        match (old.get(key), value) {
            (Some(Value::Object(was)), Value::Object(now)) => collect(at, was, now, changes),
            (Some(same), _) if same == value => {}
            _ => changes.push(Change {
                path: at.clone(),
                op: Op::Set(value.clone()),
            }),
        }
        at.pop();
    }
    for key in old.keys().filter(|key| !new.contains_key(*key)) {
        at.push(key.clone());
        changes.push(Change {
            path: at.clone(),
            op: Op::Remove,
        });
        at.pop();
    }
}

/// The changes into the document, in order. A `null` anywhere stops all of
/// them before the first is made: TOML has no word for one (ADR-0058 §4), and
/// half a write is worse than none.
pub fn apply(document: &mut DocumentMut, changes: &[Change]) -> Result<(), SettingsError> {
    refuse_nulls(changes)?;
    for change in changes {
        match &change.op {
            Op::Set(value) => set(document, &change.path, value)?,
            Op::Remove => remove(document, &change.path),
        }
    }
    Ok(())
}

fn refuse_nulls(changes: &[Change]) -> Result<(), SettingsError> {
    for change in changes {
        if let Op::Set(value) = &change.op
            && let Some(key) = first_null(&change.path.join("."), value)
        {
            return Err(SettingsError::Null { key });
        }
    }
    Ok(())
}

/// The dotted key of the first `null` under a value, named the way a person
/// would find it: `.` into an object, `[n]` into an array.
fn first_null(key: &str, value: &Value) -> Option<String> {
    match value {
        Value::Null => Some(key.to_string()),
        Value::Object(members) => members
            .iter()
            .find_map(|(name, value)| first_null(&format!("{key}.{name}"), value)),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(n, value)| first_null(&format!("{key}[{n}]"), value)),
        _ => None,
    }
}

fn set(document: &mut DocumentMut, path: &[String], value: &Value) -> Result<(), SettingsError> {
    let Some((leaf, parents)) = path.split_last() else {
        return Ok(());
    };
    let (table, inline) = descend(document, parents)?;
    put(table, leaf, item(&path.join("."), value, inline)?);
    Ok(())
}

/// A leaf replaced, with what the file says around it left alone: the comment
/// above the key and whatever trails the value are the person's, not the
/// caller's, and the new value is written between them. `TableLike::insert`
/// reformats the key it lands on, which is what would drop the comment.
fn put(table: &mut dyn TableLike, leaf: &str, item: Item) {
    let key = table.key(leaf).cloned();
    let around = table
        .get(leaf)
        .and_then(Item::as_value)
        .map(|value: &Written| value.decor().clone());
    table.insert(leaf, item);
    if let Some(kept) = key
        && let Some(mut now) = table.key_mut(leaf)
    {
        *now.leaf_decor_mut() = kept.leaf_decor().clone();
        *now.dotted_decor_mut() = kept.dotted_decor().clone();
    }
    if let Some(kept) = around
        && let Some(now) = table.get_mut(leaf).and_then(Item::as_value_mut)
    {
        *now.decor_mut() = kept;
    }
}

/// A key's comment goes with the key: removing the item removes the line and
/// the decor above it.
fn remove(document: &mut DocumentMut, path: &[String]) {
    let Some((leaf, parents)) = path.split_last() else {
        return;
    };
    let mut table: &mut dyn TableLike = document.as_table_mut();
    for key in parents {
        match table.get_mut(key).and_then(Item::as_table_like_mut) {
            Some(deeper) => table = deeper,
            None => return,
        }
    }
    table.remove(leaf);
}

/// The table a leaf lives in, and whether that table is an inline one — which
/// decides how an object may be written into it: `{ … }` beside its siblings,
/// never a `[header]` in the middle of a line.
fn descend<'a>(
    document: &'a mut DocumentMut,
    parents: &[String],
) -> Result<(&'a mut dyn TableLike, bool), SettingsError> {
    let mut table: &mut dyn TableLike = document.as_table_mut();
    let mut inline = false;
    for key in parents {
        let item = table.entry(key).or_insert(implicit());
        inline = item.is_inline_table();
        table = item
            .as_table_like_mut()
            .ok_or_else(|| SettingsError::Type {
                key: key.clone(),
                layer: "the settings being written".to_string(),
                message: "expected a table to write into".to_string(),
            })?;
    }
    Ok((table, inline))
}

/// A table with no header of its own until something is written under it.
fn implicit() -> Item {
    let mut table = Table::new();
    table.set_implicit(true);
    Item::Table(table)
}

/// One JSON value as the item TOML holds it in. An object becomes a standard
/// table — `[a.b]`, the shape a person writing settings by hand writes — and
/// stays inline only where that is the one shape its parent can hold.
fn item(key: &str, value: &Value, inline: bool) -> Result<Item, SettingsError> {
    let built = built(key, value)?;
    Ok(match inline {
        true => built,
        false => standard(built),
    })
}

/// The serialiser writes a document, and every object in it inline, so it is
/// asked for one key and that key's item is taken.
fn built(key: &str, value: &Value) -> Result<Item, SettingsError> {
    const ONE: &str = "one";
    let described = |message: String| SettingsError::Type {
        key: key.to_string(),
        layer: "the settings being written".to_string(),
        message,
    };
    let mut document = toml_edit::ser::to_document(&serde_json::json!({ ONE: value }))
        .map_err(|e: toml_edit::ser::Error| described(e.to_string()))?;
    document
        .as_table_mut()
        .remove(ONE)
        .ok_or_else(|| described("a value TOML cannot hold".to_string()))
}

/// An inline table as the tables a person would have written: `[a.b]` rather
/// than one long `{ … }` line, and no header where a table holds nothing but
/// other tables. An array is one leaf and crosses as it is.
fn standard(item: Item) -> Item {
    let Item::Value(Written::InlineTable(inline)) = item else {
        return item;
    };
    let mut table = inline.into_table();
    for (_, child) in table.iter_mut() {
        *child = standard(std::mem::take(child));
    }
    let bare = !table.is_empty() && !table.iter().any(|(_, child)| child.is_value());
    table.set_implicit(bare);
    Item::Table(table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("an object")
    }

    fn written(text: &str, new: Value) -> String {
        let mut document: DocumentMut = text.parse().expect("a document");
        let old = toml_edit::de::from_document::<Value>(document.clone()).expect("a value");
        apply(&mut document, &diff(&map(old), &map(new))).expect("a write");
        document.to_string()
    }

    #[test]
    fn a_leaf_that_did_not_change_is_not_a_change() {
        assert_eq!(diff(&map(json!({ "a": 1 })), &map(json!({ "a": 1 }))), []);
        assert_eq!(
            diff(
                &map(json!({ "a": { "b": 1 } })),
                &map(json!({ "a": { "b": 1 } }))
            ),
            []
        );
    }

    #[test]
    fn an_object_is_recursed_into_and_an_array_is_one_leaf() {
        assert_eq!(
            diff(
                &map(json!({ "a": { "b": 1, "c": 2 }, "d": [1] })),
                &map(json!({ "a": { "b": 9, "c": 2 }, "d": [1, 2] })),
            ),
            [
                Change {
                    path: vec!["a".into(), "b".into()],
                    op: Op::Set(json!(9)),
                },
                Change {
                    path: vec!["d".into()],
                    op: Op::Set(json!([1, 2])),
                },
            ]
        );
    }

    #[test]
    fn a_key_the_new_document_does_not_name_is_removed() {
        assert_eq!(
            diff(&map(json!({ "a": 1, "b": 2 })), &map(json!({ "a": 1 }))),
            [Change {
                path: vec!["b".into()],
                op: Op::Remove,
            }]
        );
    }

    /// The whole point of ADR-0058: the file a person annotated is the file a
    /// command writes into.
    #[test]
    fn a_comment_above_an_untouched_key_survives() {
        let before =
            "# what answers\nprovider = \"openai\"\n\n# how hard it thinks\nthinking = \"low\"\n";
        assert_eq!(
            written(before, json!({ "provider": "openai", "thinking": "high" })),
            "# what answers\nprovider = \"openai\"\n\n# how hard it thinks\nthinking = \"high\"\n",
        );
    }

    #[test]
    fn a_removed_keys_comment_goes_with_it() {
        let before = "# mine\na = 1\n# theirs\nb = 2\n";
        assert_eq!(written(before, json!({ "a": 1 })), "# mine\na = 1\n");
    }

    #[test]
    fn a_nested_leaf_change_leaves_its_siblings_byte_identical() {
        let before = "[openai]\n# the proxy at work\nbaseUrl = \"http://old\"\napiKey = \"k\"\n\n[openai.instances.proxy1]\nbaseUrl = \"http://one\"\n";
        assert_eq!(
            written(
                before,
                json!({ "openai": {
                    "baseUrl": "http://new",
                    "apiKey": "k",
                    "instances": { "proxy1": { "baseUrl": "http://one" } },
                } }),
            ),
            before.replace("http://old", "http://new"),
        );
    }

    /// R-tables: a new nested object lands as a standard table, which is what
    /// a person writing the same settings by hand would have written.
    #[test]
    fn a_new_nested_object_lands_as_a_standard_table() {
        assert_eq!(
            written(
                "model = \"m\"\n",
                json!({ "model": "m", "openai": { "instances": { "proxy1": { "baseUrl": "u" } } } }),
            ),
            "model = \"m\"\n\n[openai.instances.proxy1]\nbaseUrl = \"u\"\n",
            "a table that holds only tables writes no header of its own"
        );
        assert_eq!(
            written("", json!({ "a": { "b": 1, "c": { "d": 2 } } })),
            "[a]\nb = 1\n\n[a.c]\nd = 2\n",
        );
    }

    /// R-tables, the other half: a table a person wrote inline stays inline,
    /// because the leaf inside it is edited in place.
    #[test]
    fn an_existing_inline_table_that_changes_stays_inline() {
        assert_eq!(
            written(
                "servers = { one = { url = \"a\" } }\n",
                json!({ "servers": { "one": { "url": "b" }, "two": { "url": "c" } } }),
            ),
            "servers = { one = { url = \"b\" } , two = { url = \"c\" } }\n",
            "the spacing is the file's own; what matters is that no header appeared"
        );
    }

    #[test]
    fn a_null_names_the_dotted_key_it_was_written_at() {
        let changes = diff(&Map::new(), &map(json!({ "a": { "b": null } })));
        let refused = apply(&mut DocumentMut::new(), &changes).expect_err("a null");
        assert!(refused.to_string().contains("a.b"), "{refused}");
        assert!(refused.to_string().contains("no null"), "{refused}");

        let changes = diff(&Map::new(), &map(json!({ "a": [1, null] })));
        let refused = apply(&mut DocumentMut::new(), &changes).expect_err("a null");
        assert!(refused.to_string().contains("a[1]"), "{refused}");
    }

    /// A refused write leaves the document exactly as it found it, so the
    /// caller has nothing half-written to print.
    #[test]
    fn a_null_stops_every_change_and_not_just_its_own() {
        let mut document: DocumentMut = "a = 1\n".parse().expect("a document");
        let changes = diff(&map(json!({ "a": 1 })), &map(json!({ "a": 2, "b": null })));
        apply(&mut document, &changes).expect_err("a null");
        assert_eq!(document.to_string(), "a = 1\n");
    }
}
