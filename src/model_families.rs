//! The family-catalog file (D73): `~/.config/bingo/model-catalog.json`.
//!
//! The compiled prefix table in `api::models` carries researched defaults per
//! model family — window, output ceiling, thinking support. This file makes
//! that table visible and overridable without touching settings.json:
//!
//! - `builtin` is bingo's section: a mirror of the compiled table, rewritten
//!   whenever an upgrade ships new research. Reference only — edits there are
//!   reverted on the next start, which is exactly how corrected defaults reach
//!   users who never tuned anything.
//! - `overrides` is the user's section: never written by bingo, consulted at
//!   runtime between the settings declaration and the compiled table. Keys are
//!   id prefixes (a full id is just the longest prefix); fields are optional
//!   and cascade per field through shorter matching prefixes.
//!
//! Every failure degrades to "no overrides" with a startup note — a corrupt
//! file must never keep bingo from starting, and must never be overwritten
//! (the overrides in it are the user's work; a repair would destroy it).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Stable filename next to settings.json.
const CATALOG_FILE: &str = "model-catalog.json";

/// What the `_doc` key says. Serialized on every write so the file explains
/// its own contract to someone opening it cold.
const DOC: &str = "bingo rewrites `builtin` on upgrade; put your changes in `overrides`. \
     Keys are model-id prefixes (longest match wins, field by field); fields are \
     contextWindow, maxTokens, thinking. settings.json model declarations outrank this file.";

/// One family's (possibly partial) metadata as written in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FamilyMeta {
    #[serde(
        rename = "contextWindow",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub context_window: Option<u64>,
    #[serde(rename = "maxTokens", default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<bool>,
}

/// Full file shape. Unknown top-level keys are rejected on purpose: a typo'd
/// section would otherwise sit there silently doing nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FamilyFile {
    #[serde(rename = "_doc")]
    doc: String,
    #[serde(default)]
    builtin: BTreeMap<String, FamilyMeta>,
    #[serde(default)]
    overrides: BTreeMap<String, FamilyMeta>,
}

pub fn catalog_path(user_dir: &Path) -> PathBuf {
    user_dir.join("bingo").join(CATALOG_FILE)
}

/// The compiled table in file form — the `builtin` section's content.
fn seed() -> BTreeMap<String, FamilyMeta> {
    crate::api::models::builtin_families()
        .iter()
        .map(|(prefix, meta)| {
            (
                prefix.to_string(),
                FamilyMeta {
                    context_window: Some(meta.context_window),
                    max_tokens: Some(meta.max_tokens),
                    thinking: Some(meta.supports_thinking),
                },
            )
        })
        .collect()
}

/// Startup maintenance: create the file on first run, refresh `builtin` when
/// an upgrade changed the compiled table, leave `overrides` alone always.
/// Returns startup notes (config-lint style); an empty vec means all is well.
pub fn maintain(user_dir: &Path) -> Vec<String> {
    let path = catalog_path(user_dir);
    let seed = seed();
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let file = FamilyFile {
                doc: DOC.to_string(),
                builtin: seed,
                overrides: BTreeMap::new(),
            };
            return match write_atomic(&path, &file) {
                Ok(()) => Vec::new(),
                Err(e) => vec![format!(
                    "⚠ could not create {} ({e}); model family defaults stay built-in only",
                    path.display()
                )],
            };
        }
        Err(e) => {
            return vec![format!(
                "⚠ could not read {} ({e}); using built-in model family defaults",
                path.display()
            )];
        }
    };
    let mut file: FamilyFile = match serde_json::from_str(&raw) {
        Ok(file) => file,
        // Never rewrite a file that fails to parse: the broken content may
        // hold the user's overrides mid-edit. Degrade loudly instead.
        Err(e) => {
            return vec![format!(
                "⚠ {} is not valid ({e}); using built-in model family defaults \
                 (fix or delete the file to re-seed)",
                path.display()
            )];
        }
    };
    if file.builtin == seed && file.doc == DOC {
        return Vec::new();
    }
    file.builtin = seed;
    file.doc = DOC.to_string();
    match write_atomic(&path, &file) {
        Ok(()) => Vec::new(),
        Err(e) => vec![format!(
            "⚠ could not refresh {} ({e}); its builtin section is stale",
            path.display()
        )],
    }
}

/// Read-only override lookup for `Client` construction: longest prefixes
/// first, so the resolver's field-by-field cascade can walk the list in
/// order. Any failure is "no overrides" — `maintain` already reported it.
pub fn load_overrides(user_dir: &Path) -> Vec<(String, FamilyMeta)> {
    let raw = match std::fs::read_to_string(catalog_path(user_dir)) {
        Ok(raw) => raw,
        Err(_) => return Vec::new(),
    };
    let file: FamilyFile = match serde_json::from_str(&raw) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    let mut overrides: Vec<(String, FamilyMeta)> = file.overrides.into_iter().collect();
    overrides.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    overrides
}

/// tmp + rename (the AuthStore discipline): a kill mid-write must leave the
/// previous file intact rather than a truncated one.
fn write_atomic(path: &Path, file: &FamilyFile) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("catalog path has no parent"))?;
    std::fs::create_dir_all(dir)?;
    let encoded = serde_json::to_string_pretty(file).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    let written = std::fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(encoded.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()
    });
    match written {
        Ok(()) => std::fs::rename(&tmp, path),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_user_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bingo-model-families-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn read(path: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// First run: the file appears, seeded with the compiled table and an
    /// empty overrides section, and the round trip reports nothing.
    #[test]
    fn first_run_seeds_the_file() {
        let user_dir = tmp_user_dir("seed");
        assert!(maintain(&user_dir).is_empty());
        let value = read(&catalog_path(&user_dir));
        assert!(value["_doc"].as_str().unwrap().contains("overrides"));
        assert_eq!(
            value["builtin"]["deepseek"]["maxTokens"].as_u64(),
            Some(8_000),
            "the seed mirrors the compiled table"
        );
        assert_eq!(value["overrides"], serde_json::json!({}));
        assert!(load_overrides(&user_dir).is_empty());
        // A second run is a no-op, not a rewrite.
        assert!(maintain(&user_dir).is_empty());
        let _ = std::fs::remove_dir_all(&user_dir);
    }

    /// An upgrade that changed the compiled table refreshes `builtin` and
    /// leaves `overrides` untouched — that section is the user's.
    #[test]
    fn upgrade_refreshes_builtin_and_keeps_overrides() {
        let user_dir = tmp_user_dir("refresh");
        let path = catalog_path(&user_dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{
                "_doc": "old doc",
                "builtin": {"deepseek": {"maxTokens": 4000}},
                "overrides": {"deepseek-v4-flash": {"maxTokens": 32000}}
            }"#,
        )
        .unwrap();
        assert!(maintain(&user_dir).is_empty());
        let value = read(&path);
        assert_eq!(
            value["builtin"]["deepseek"]["maxTokens"].as_u64(),
            Some(8_000),
            "builtin is bingo's section and follows the binary"
        );
        assert_eq!(
            value["overrides"]["deepseek-v4-flash"]["maxTokens"].as_u64(),
            Some(32_000),
            "overrides are the user's section and survive the refresh"
        );
        let overrides = load_overrides(&user_dir);
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].0, "deepseek-v4-flash");
        assert_eq!(overrides[0].1.max_tokens, Some(32_000));
        let _ = std::fs::remove_dir_all(&user_dir);
    }

    /// Longer prefixes come first so the resolver's first-match-per-field
    /// cascade sees the most specific entry before the family-wide one.
    #[test]
    fn overrides_sort_longest_prefix_first() {
        let user_dir = tmp_user_dir("sort");
        let path = catalog_path(&user_dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{
                "_doc": "d",
                "builtin": {},
                "overrides": {
                    "deepseek": {"maxTokens": 64000},
                    "deepseek-v4-flash": {"contextWindow": 200000}
                }
            }"#,
        )
        .unwrap();
        let overrides = load_overrides(&user_dir);
        assert_eq!(overrides[0].0, "deepseek-v4-flash");
        assert_eq!(overrides[1].0, "deepseek");
        let _ = std::fs::remove_dir_all(&user_dir);
    }

    /// A file that fails to parse degrades to built-ins with a note and is
    /// never rewritten — the broken content may hold the user's overrides.
    #[test]
    fn a_broken_file_degrades_loudly_and_is_left_alone() {
        let user_dir = tmp_user_dir("broken");
        let path = catalog_path(&user_dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        let notes = maintain(&user_dir);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("model-catalog.json"), "{}", notes[0]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{not json",
            "a repair would destroy whatever the user had mid-edit"
        );
        assert!(load_overrides(&user_dir).is_empty());
        let _ = std::fs::remove_dir_all(&user_dir);
    }

    /// A typo'd field name fails the parse (deny_unknown_fields) rather than
    /// silently doing nothing — same doctrine as the settings key lint.
    #[test]
    fn a_typoed_field_is_a_visible_failure() {
        let user_dir = tmp_user_dir("typo");
        let path = catalog_path(&user_dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"_doc": "d", "builtin": {}, "overrides": {"m": {"maxToken": 1}}}"#,
        )
        .unwrap();
        let notes = maintain(&user_dir);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("maxToken"), "{}", notes[0]);
        let _ = std::fs::remove_dir_all(&user_dir);
    }
}
