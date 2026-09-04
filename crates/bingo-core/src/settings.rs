//! Settings: JSONC layers (user < project < local < command line) merged
//! per key by the rule the claiming plugin declared, then sliced — the
//! kernel keeps its four keys, every plugin gets the keys it claimed, and
//! whatever nobody claimed is reported by source so a typo is not silent.

mod merge;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bingo_sdk::{Effort, Env, Merge, PluginManifest};
use serde_json::{Map, Value};

use crate::models::Declared;

pub use merge::merge;

/// The keys the kernel reads itself.
pub const KERNEL_KEYS: &[&str] = &["provider", "model", "thinking", "maxTokens", "models"];

/// One settings source, lowest priority first when listed.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    /// Where it came from, for messages: a path or `cli`.
    pub source: String,
    pub value: Map<String, Value>,
}

impl Layer {
    pub fn new(source: impl Into<String>, value: Map<String, Value>) -> Self {
        Self {
            source: source.into(),
            value,
        }
    }
}

/// What one plugin claims: dotted key paths and how each merges.
#[derive(Clone, Debug, PartialEq)]
pub struct Claim {
    pub plugin: String,
    pub keys: Vec<(String, Merge)>,
}

impl Claim {
    pub fn from_manifest(manifest: &PluginManifest) -> Option<Self> {
        let claim = manifest.config?;
        Some(Self {
            plugin: manifest.id.to_string(),
            keys: claim
                .keys
                .iter()
                .map(|(key, merge)| ((*key).to_string(), *merge))
                .collect(),
        })
    }

    /// The top-level keys this claim covers.
    fn roots(&self) -> impl Iterator<Item = &str> {
        self.keys
            .iter()
            .map(|(key, _)| key.split('.').next().unwrap_or(key))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct KernelSettings {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking: Option<Effort>,
    pub max_tokens: Option<u32>,
    /// Per-model overrides of the catalogue, keyed `<provider>/<model>` (ADR-0004).
    pub models: BTreeMap<String, Declared>,
}

/// A top-level key nobody claimed, with the layer that set it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownKey {
    pub source: String,
    pub key: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Merged {
    pub kernel: KernelSettings,
    /// Each plugin's slice: an object holding only the roots it claimed.
    pub plugins: BTreeMap<String, Value>,
    pub unknown: Vec<UnknownKey>,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("writing {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{layer}: settings must be a JSON object")]
    NotAnObject { layer: String },
    #[error("settings key {key} is claimed by both {first} and {second}")]
    Conflict {
        key: String,
        first: String,
        second: String,
    },
    #[error("settings key {key} (in {layer}): {message}")]
    Type {
        key: String,
        layer: String,
        message: String,
    },
}

/// The user layer: the lowest of the three, the one that is about the person
/// rather than the project, and the only one a command writes back to.
pub fn user_path(env: &Env) -> PathBuf {
    env.config_dir.join("settings.json")
}

/// The three on-disk layers, lowest priority first.
pub fn layer_paths(env: &Env, cwd: &Path) -> [PathBuf; 3] {
    [
        user_path(env),
        cwd.join(".bingo").join("settings.json"),
        cwd.join(".bingo").join("settings.local.json"),
    ]
}

/// Set top-level keys in one layer, leaving every neighbour where it is
/// (ADR-0003 §5: writing settings targets one named layer).
pub fn remember(path: &Path, keys: &[(&str, Value)]) -> Result<(), SettingsError> {
    let mut document = read_document(path)?;
    for (key, value) in keys {
        document.insert((*key).to_string(), value.clone());
    }
    write(path, &document)
}

/// One layer as JSON, in the order it was written (`serde_json/preserve_order`);
/// a file that is not there is an empty document. This is the read half of a
/// round trip, so unlike [`read_layer`] it refuses JSONC: rewriting a file
/// with comments in it would drop them.
pub fn read_document(path: &Path) -> Result<Map<String, Value>, SettingsError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(source) => {
            return Err(SettingsError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(SettingsError::NotAnObject {
            layer: path.display().to_string(),
        }),
        Err(e) => Err(SettingsError::Parse {
            path: path.to_path_buf(),
            message: format!(
                "not plain JSON ({e}); a file with comments is read at startup \
                 but never rewritten — change it by hand"
            ),
        }),
    }
}

/// Through a temporary file and a rename: a settings file a person wrote is
/// not something to lose half of.
pub fn write(path: &Path, document: &Map<String, Value>) -> Result<(), SettingsError> {
    let directory = path.parent().unwrap_or(Path::new("."));
    let failed = |source| SettingsError::Write {
        path: path.to_path_buf(),
        source,
    };
    std::fs::create_dir_all(directory).map_err(failed)?;
    let json = serde_json::to_string_pretty(document).map_err(|e| SettingsError::Parse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    // Named for this write and no other: two processes — or two tests —
    // saving at once must not rename each other's half-written file away.
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temporary = directory.join(format!("settings.json.{}.{n}.tmp", std::process::id()));
    std::fs::write(&temporary, format!("{json}\n")).map_err(failed)?;
    std::fs::rename(&temporary, path).map_err(failed)
}

/// Read the on-disk layers plus an optional explicit file, skipping the
/// ones that do not exist.
pub fn load(env: &Env, cwd: &Path, extra: Option<&Path>) -> Result<Vec<Layer>, SettingsError> {
    let mut layers = Vec::new();
    let paths = layer_paths(env, cwd);
    for path in paths.iter().map(PathBuf::as_path).chain(extra) {
        if let Some(layer) = read_layer(path)? {
            layers.push(layer);
        }
    }
    Ok(layers)
}

/// One file as a layer; `None` when it does not exist.
pub fn read_layer(path: &Path) -> Result<Option<Layer>, SettingsError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(SettingsError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let value = parse_jsonc(path, &text)?;
    let source = path.display().to_string();
    match value {
        Value::Object(map) => Ok(Some(Layer::new(source, map))),
        Value::Null => Ok(None),
        _ => Err(SettingsError::NotAnObject { layer: source }),
    }
}

fn parse_jsonc(path: &Path, text: &str) -> Result<Value, SettingsError> {
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

    fn env(dir: &Path) -> Env {
        Env {
            home: dir.to_path_buf(),
            config_dir: dir.join("config"),
            data_dir: dir.join("data"),
        }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn load_reads_the_layers_that_exist_in_priority_order() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let cwd = dir.path().join("project");
        write(
            &env.config_dir.join("settings.json"),
            "{ // user\n \"model\": \"user\" }",
        );
        write(
            &cwd.join(".bingo/settings.local.json"),
            "{ \"model\": \"local\", }",
        );
        let extra = dir.path().join("extra.json");
        write(&extra, "{\"model\": \"extra\"}");

        let layers = load(&env, &cwd, Some(&extra)).unwrap();
        let models: Vec<_> = layers.iter().map(|l| l.value["model"].clone()).collect();
        assert_eq!(models, vec![json!("user"), json!("local"), json!("extra")]);
        assert!(layers[0].source.ends_with("config/settings.json"));
    }

    #[test]
    fn remember_sets_its_keys_and_leaves_every_neighbour_where_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config").join("settings.json");
        write(
            &path,
            "{\n  \"model\": \"old\",\n  \"permissions\": { \"allow\": [\"Read\"] }\n}",
        );

        super::remember(
            &path,
            &[("provider", json!("openai")), ("model", json!("gpt-5"))],
        )
        .unwrap();

        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after["model"], json!("gpt-5"));
        assert_eq!(after["provider"], json!("openai"));
        assert_eq!(after["permissions"]["allow"], json!(["Read"]));
    }

    #[test]
    fn a_file_with_comments_is_read_but_never_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        write(&path, "// mine\n{ \"model\": \"m\" }");
        assert!(
            read_layer(&path).unwrap().is_some(),
            "the layers read JSONC"
        );

        let refused = super::remember(&path, &[("model", json!("m2"))]).unwrap_err();
        assert!(refused.to_string().contains("not plain JSON"), "{refused}");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("// mine"),
            "the file a person wrote is left alone"
        );
    }

    #[test]
    fn a_non_object_layer_is_an_error_and_an_empty_file_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        write(&env.config_dir.join("settings.json"), "[1, 2]");
        let err = load(&env, dir.path(), None).unwrap_err();
        assert!(matches!(err, SettingsError::NotAnObject { .. }), "{err}");

        write(&env.config_dir.join("settings.json"), "");
        assert!(load(&env, dir.path(), None).unwrap().is_empty());

        write(&env.config_dir.join("settings.json"), "{ \"model\": ");
        let err = load(&env, dir.path(), None).unwrap_err();
        assert!(matches!(err, SettingsError::Parse { .. }), "{err}");
    }
}
