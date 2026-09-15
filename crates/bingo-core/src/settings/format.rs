//! Which language a settings file is written in, and how its text becomes a
//! value. A layer is an object whatever file it came from (ADR-0058 §1), so
//! nothing past this module knows which of the two it was reading.

use std::path::{Path, PathBuf};

use serde_json::Value;
use toml_edit::DocumentMut;

use super::SettingsError;

/// How a file is read: by its extension and nothing else, so `--settings
/// notes.json` and the `settings.json` a layer directory may still hold are
/// read the way they always were.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Toml,
    Jsonc,
}

impl Format {
    pub fn of(path: &Path) -> Self {
        let toml = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"));
        match toml {
            true => Format::Toml,
            false => Format::Jsonc,
        }
    }
}

/// The JSON file that stands where a TOML layer does not: `settings.toml` →
/// `settings.json`, `settings.local.toml` → `settings.local.json`. Only the
/// last extension moves, so a layer keeps the name it is known by.
pub fn json_sibling(path: &Path) -> PathBuf {
    path.with_extension("json")
}

/// One file's text as a value.
pub fn parse(format: Format, path: &Path, text: &str) -> Result<Value, SettingsError> {
    match format {
        Format::Toml => value(path, document(path, text)?),
        Format::Jsonc => jsonc(path, text),
    }
}

/// The text as a document — its comments, blank lines and ordering kept — so
/// that a write can put one leaf back without disturbing any of them.
pub fn document(path: &Path, text: &str) -> Result<DocumentMut, SettingsError> {
    text.parse().map_err(|e: toml_edit::TomlError| {
        SettingsError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        }
    })
}

/// What a document says, with nothing of how it was written. A TOML root is a
/// table, so this is always an object; a datetime, which no setting is, comes
/// through as the one-key object `toml_edit` spells it with rather than as a
/// panic (ADR-0058 §4).
pub fn value(path: &Path, document: DocumentMut) -> Result<Value, SettingsError> {
    toml_edit::de::from_document(document).map_err(|e| SettingsError::Parse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

fn jsonc(path: &Path, text: &str) -> Result<Value, SettingsError> {
    jsonc_parser::parse_to_serde_value(text, &jsonc_parser::ParseOptions::default())
        .map(|v: Option<Value>| v.unwrap_or(Value::Null))
        .map_err(|e| SettingsError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn toml(text: &str) -> Value {
        parse(Format::Toml, Path::new("settings.toml"), text).expect("a document")
    }

    #[test]
    fn the_extension_decides_and_nothing_else_does() {
        assert_eq!(Format::of(Path::new("/a/settings.toml")), Format::Toml);
        assert_eq!(Format::of(Path::new("/a/settings.local.toml")), Format::Toml);
        assert_eq!(Format::of(Path::new("/a/settings.json")), Format::Jsonc);
        assert_eq!(Format::of(Path::new("/a/notes")), Format::Jsonc);
    }

    #[test]
    fn a_layer_keeps_its_name_when_it_loses_its_extension() {
        assert_eq!(
            json_sibling(Path::new("/a/.bingo/settings.local.toml")),
            Path::new("/a/.bingo/settings.local.json")
        );
        assert_eq!(
            json_sibling(Path::new("/a/settings.toml")),
            Path::new("/a/settings.json")
        );
    }

    /// The two formats are two spellings of one layer (ADR-0058 §1).
    #[test]
    fn the_same_settings_in_either_language_parse_to_the_same_value() {
        let as_toml = toml(
            "model = \"m\"\nmaxTokens = 8192\n\n[permissions]\nallow = [\"Read\", \"Write\"]\n",
        );
        let as_json = parse(
            Format::Jsonc,
            Path::new("settings.json"),
            "{ // mine\n \"model\": \"m\", \"maxTokens\": 8192,\n \
             \"permissions\": { \"allow\": [\"Read\", \"Write\"] } }",
        )
        .expect("jsonc");
        assert_eq!(as_toml, as_json);
    }

    #[test]
    fn a_toml_root_is_always_an_object_even_when_the_file_is_empty() {
        assert_eq!(toml(""), json!({}));
        assert_eq!(toml("# only a comment\n"), json!({}));
    }

    /// R-datetime: no setting is a datetime, but a file may hold one and a
    /// start must not die on it. `toml_edit` hands it back as its own one-key
    /// object; what matters is that it is a value like any other.
    #[test]
    fn a_datetime_reads_as_a_value_rather_than_a_panic() {
        let read = toml("when = 1979-05-27T07:32:00Z\n");
        assert_eq!(
            read["when"],
            json!({ "$__toml_private_datetime": "1979-05-27T07:32:00Z" })
        );
    }

    #[test]
    fn a_file_that_is_not_toml_says_where_it_stopped() {
        let error = parse(Format::Toml, Path::new("settings.toml"), "model = ").expect_err("bad");
        assert!(matches!(error, SettingsError::Parse { .. }), "{error}");
    }
}
