//! Settings: TOML layers (user < project < local < command line) merged
//! per key by the rule the claiming plugin declared, then sliced — the
//! kernel keeps its four keys, every plugin gets the keys it claimed, and
//! whatever nobody claimed is reported by source so a typo is not silent.
//!
//! A layer is an object whatever file it came from (ADR-0058 §1): where no
//! `settings.toml` stands, the `settings.json` an older bingo wrote is read
//! as it always was, and [`migrate_all`] moves it across once.

mod edit;
mod format;
mod merge;
mod migrate;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use bingo_sdk::{Effort, Env, Merge, PluginManifest};
use serde_json::{Map, Value};

use crate::models::Declared;

use format::Format;

pub use merge::merge;
pub use migrate::{Migration, migrate_all, migrate_one};

/// The keys the kernel owns. It reads all but `pictures` itself, which is
/// read by whoever builds a picture loader ([`picture_cache_days`]) and is a
/// kernel key so that no plugin may claim it and nobody who sets it is told it
/// is unknown (ADR-0003 §2).
pub const KERNEL_KEYS: &[&str] = &[
    "provider",
    "model",
    "thinking",
    "maxTokens",
    "models",
    "pictures",
    crate::plugins::KEY,
];

/// The one key under `pictures`: the spelling every other kernel key uses, and
/// the spelling the ask was written in. Both are read, so neither is a silent
/// no-op; the first is the one messages name.
const CACHE_DAYS: [&str; 2] = ["cacheDays", "cache_days"];

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
    /// Which plugins run, by name: a manifest id for one this build ships, a
    /// directory name for one a bridge found. Absent is on (ADR-0057 §1).
    pub enabled_plugins: BTreeMap<String, bool>,
}

impl KernelSettings {
    /// The names a layer turned off, which is what the registry and every
    /// plugin that runs plugins of its own are handed (ADR-0057 §2, §4).
    pub fn switched_off(&self) -> BTreeSet<String> {
        self.enabled_plugins
            .iter()
            .filter(|(_, on)| !**on)
            .map(|(name, _)| name.clone())
            .collect()
    }
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
    #[error("{layer}: settings must be an object")]
    NotAnObject { layer: String },
    #[error(
        "settings key {key}: TOML has no null, so a layer written in it cannot \
         clear what a lower one said; remove the key, or keep this layer as JSON"
    )]
    Null { key: String },
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

/// How many days a picture fetched from the web is kept on this machine
/// (`pictures.cacheDays`, ADR-0041, M61). `None` where no layer says: the
/// default belongs to the cache that keeps the pictures, not to the file that
/// configures it, and `0` means never keep one.
///
/// The highest layer that names `pictures` speaks for it, which is what the
/// merge would have produced for a single scalar (ADR-0003 §3) — and a `null`
/// or an empty object there clears what the layers below said, as a `null`
/// does everywhere else. It is read off the layers rather than out of
/// [`Merged`] because the process that hands the number to a surface composes
/// those layers before a host, and so before any claim, exists.
pub fn picture_cache_days(layers: &[Layer]) -> Result<Option<u64>, SettingsError> {
    let mut days = None;
    for layer in layers {
        if let Some(pictures) = layer.value.get("pictures") {
            days = said(layer, pictures)?;
        }
    }
    Ok(days)
}

/// What one layer's `pictures` says about the cache's life. An unrecognised
/// member of it is a typo said out loud: the unknown-key notice only ever sees
/// top-level keys (ADR-0003 §4), so nothing else would catch one.
fn said(layer: &Layer, pictures: &Value) -> Result<Option<u64>, SettingsError> {
    if pictures.is_null() {
        return Ok(None);
    }
    let object = pictures
        .as_object()
        .ok_or_else(|| wrong(layer, "pictures", "expected an object"))?;
    let mut days = None;
    for (key, value) in object {
        if !CACHE_DAYS.contains(&key.as_str()) {
            let known = CACHE_DAYS[0];
            let key = format!("pictures.{key}");
            return Err(wrong(
                layer,
                &key,
                &format!("no such setting; `{known}` is the one"),
            ));
        }
        if !value.is_null() {
            days = Some(value.as_u64().ok_or_else(|| {
                let message = "expected a whole number of days, `0` for never";
                wrong(layer, &format!("pictures.{key}"), message)
            })?);
        }
    }
    Ok(days)
}

fn wrong(layer: &Layer, key: &str, message: &str) -> SettingsError {
    SettingsError::Type {
        key: key.to_string(),
        layer: layer.source.clone(),
        message: message.to_string(),
    }
}

/// The user layer: the lowest of the three, the one that is about the person
/// rather than the project, and the only one a command writes back to. The
/// sdk spells it, so a provider's hint names the same file (ADR-0058 §6).
pub fn user_path(env: &Env) -> PathBuf {
    env.user_settings()
}

/// The three on-disk layers, lowest priority first.
pub fn layer_paths(env: &Env, cwd: &Path) -> [PathBuf; 3] {
    [
        user_path(env),
        cwd.join(".bingo").join("settings.toml"),
        cwd.join(".bingo").join("settings.local.toml"),
    ]
}

/// Set top-level keys in one layer, leaving every neighbour where it is
/// (ADR-0003 §5: writing settings targets one named layer).
///
/// A `null` here is a caller saying *this layer says nothing about this key*
/// — `/think off` is the one that does — so the key goes rather than being
/// written as a null. That is the same fact in the layer this writes, which
/// is always the lowest one: there is nothing under the user layer for a null
/// to clear. It is also the only fact TOML can hold (ADR-0058 §4); the
/// tri-state of ADR-0003 §3 stays a thing a person writes by hand in a higher
/// JSON layer, where it clears what the layers below said.
pub fn remember(path: &Path, keys: &[(&str, Value)]) -> Result<(), SettingsError> {
    let mut document = read_document(path)?;
    for (key, value) in keys {
        match value {
            Value::Null => document.remove(*key),
            value => document.insert((*key).to_string(), value.clone()),
        };
    }
    write(path, &document)
}

/// One layer as a document, in the order it was written
/// (`serde_json/preserve_order`); a file that is not there is an empty
/// document. This is the read half of a round trip, so a JSON layer that is
/// still standing is migrated first (ADR-0058 §2) — otherwise what is read
/// back would not be what the write is about to diff against.
///
/// A JSON path is still refused when it carries comments: rewriting it would
/// drop them, and there is no document to put a leaf back into.
pub fn read_document(path: &Path) -> Result<Map<String, Value>, SettingsError> {
    migrated(path)?;
    let Some(text) = read_text(path)? else {
        return Ok(Map::new());
    };
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match Format::of(path) {
        Format::Toml => object(path, format::parse(Format::Toml, path, &text)?),
        Format::Jsonc => plain_json(path, &text),
    }
}

fn plain_json(path: &Path, text: &str) -> Result<Map<String, Value>, SettingsError> {
    match serde_json::from_str(text) {
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

/// Set a layer to this document. A TOML path keeps everything the file says
/// about itself (ADR-0058 §3); a JSON path is re-encoded as it always was.
pub fn write(path: &Path, document: &Map<String, Value>) -> Result<(), SettingsError> {
    migrated(path)?;
    match Format::of(path) {
        Format::Toml => write_toml(path, document),
        Format::Jsonc => write_json(path, document),
    }
}

/// Only the leaves that differ, set in the document the file already is: a
/// comment, a blank line and an ordering the caller never mentioned are still
/// exactly where the person who wrote them put them.
fn write_toml(path: &Path, document: &Map<String, Value>) -> Result<(), SettingsError> {
    let text = read_text(path)?.unwrap_or_default();
    let mut edited = format::document(path, &text)?;
    let standing = object(path, format::value(path, edited.clone())?)?;
    edit::apply(&mut edited, &edit::diff(&standing, document))?;
    save(path, &edited.to_string())
}

fn write_json(path: &Path, document: &Map<String, Value>) -> Result<(), SettingsError> {
    let json = serde_json::to_string_pretty(document).map_err(|e| SettingsError::Parse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    save(path, &format!("{json}\n"))
}

/// Through a temporary file and a rename: a settings file a person wrote is
/// not something to lose half of.
fn save(path: &Path, text: &str) -> Result<(), SettingsError> {
    let directory = path.parent().unwrap_or(Path::new("."));
    let failed = |source| SettingsError::Write {
        path: path.to_path_buf(),
        source,
    };
    std::fs::create_dir_all(directory).map_err(failed)?;
    // Named for this write and no other: two processes — or two tests —
    // saving at once must not rename each other's half-written file away.
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = path.file_name().unwrap_or(std::ffi::OsStr::new("settings"));
    let temporary = directory.join(format!(
        "{}.{}.{n}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    std::fs::write(&temporary, text).map_err(failed)?;
    std::fs::rename(&temporary, path).map_err(failed)
}

/// The migration this layer still owes, run before it is read or written so
/// that a write never shadows a JSON layer's keys (ADR-0058 §2). At the start
/// of a run [`migrate_all`] has already done it and said so; this is what
/// keeps a command that writes correct on its own.
fn migrated(path: &Path) -> Result<(), SettingsError> {
    match migrate_one(path)? {
        Migration::Refused { key, .. } => Err(SettingsError::Null { key }),
        Migration::Nothing | Migration::Done { .. } => Ok(()),
    }
}

/// Read the on-disk layers plus an optional explicit file, skipping the
/// ones that do not exist.
pub fn load(env: &Env, cwd: &Path, extra: Option<&Path>) -> Result<Vec<Layer>, SettingsError> {
    let mut layers = Vec::new();
    for path in layer_paths(env, cwd) {
        if let Some(layer) = standing_layer(&path)? {
            layers.push(layer);
        }
    }
    if let Some(path) = extra
        && let Some(layer) = read_layer(path)?
    {
        layers.push(layer);
    }
    Ok(layers)
}

/// One of the three layer paths: its TOML, else the JSON that stands where no
/// TOML does (ADR-0058 §1). A `--settings` file has no sibling — it is the
/// file the person named, in the format they named it in.
fn standing_layer(path: &Path) -> Result<Option<Layer>, SettingsError> {
    match read_layer(path)? {
        Some(layer) => Ok(Some(layer)),
        None => read_layer(&format::json_sibling(path)),
    }
}

/// One file as a layer; `None` when it does not exist.
pub fn read_layer(path: &Path) -> Result<Option<Layer>, SettingsError> {
    let Some(text) = read_text(path)? else {
        return Ok(None);
    };
    let value = format::parse(Format::of(path), path, &text)?;
    let source = path.display().to_string();
    match value {
        Value::Object(map) => Ok(Some(Layer::new(source, map))),
        Value::Null => Ok(None),
        _ => Err(SettingsError::NotAnObject { layer: source }),
    }
}

/// A file's text, or `None` where there is no file.
fn read_text(path: &Path) -> Result<Option<String>, SettingsError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SettingsError::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn object(path: &Path, value: Value) -> Result<Map<String, Value>, SettingsError> {
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(SettingsError::NotAnObject {
            layer: path.display().to_string(),
        }),
    }
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
            &env.config_dir.join("settings.toml"),
            "# user\nmodel = \"user\"\n",
        );
        write(
            &cwd.join(".bingo/settings.local.toml"),
            "model = \"local\"\n",
        );
        let extra = dir.path().join("extra.json");
        write(&extra, "{\"model\": \"extra\"}");

        let layers = load(&env, &cwd, Some(&extra)).unwrap();
        let models: Vec<_> = layers.iter().map(|l| l.value["model"].clone()).collect();
        assert_eq!(models, vec![json!("user"), json!("local"), json!("extra")]);
        assert!(layers[0].source.ends_with("config/settings.toml"));
    }

    /// ADR-0058 §1: JSON stays a format bingo reads, and the two spellings of
    /// one layer are the same layer.
    #[test]
    fn a_layer_directory_with_no_toml_reads_the_json_that_stands_there() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let cwd = dir.path().join("project");
        write(
            &env.config_dir.join("settings.json"),
            "{ // user\n \"model\": \"user\", \"permissions\": { \"allow\": [\"Read\"] } }",
        );
        write(
            &cwd.join(".bingo/settings.local.json"),
            "{ \"model\": \"l\", }",
        );

        let read = load(&env, &cwd, None).unwrap();
        assert_eq!(read[0].value["model"], json!("user"));
        assert_eq!(read[0].value["permissions"]["allow"], json!(["Read"]));
        assert_eq!(read[1].value["model"], json!("l"));

        // The same layer, written in TOML, is the same layer.
        write(
            &env.config_dir.join("settings.toml"),
            "model = \"user\"\n\n[permissions]\nallow = [\"Read\"]\n",
        );
        let now = load(&env, &cwd, None).unwrap();
        assert_eq!(
            now[0].value, read[0].value,
            "the TOML says what the JSON did"
        );
        assert!(now[0].source.ends_with("settings.toml"), "and shadows it");
    }

    /// `--settings` is the file a person named, in the format they named it
    /// in: it is read as JSONC and nothing moves it (ADR-0058 §2).
    #[test]
    fn an_explicit_settings_file_is_read_where_it_is_and_never_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let extra = dir.path().join("mine.json");
        write(&extra, "{ // mine\n \"model\": \"m\" }");

        let layers = load(&env, dir.path(), Some(&extra)).unwrap();
        assert_eq!(layers[0].value["model"], json!("m"));
        assert!(extra.exists(), "still where it was");
        assert!(!dir.path().join("mine.toml").exists());
        assert!(!dir.path().join("mine.json.bak").exists());
    }

    #[test]
    fn remember_sets_its_keys_and_leaves_every_neighbour_where_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config").join("settings.toml");
        write(
            &path,
            "# what answers\nmodel = \"old\"\n\n[permissions]\nallow = [\"Read\"]\n",
        );

        super::remember(
            &path,
            &[("provider", json!("openai")), ("model", json!("gpt-5"))],
        )
        .unwrap();

        let after = read_layer(&path).unwrap().unwrap().value;
        assert_eq!(after["model"], json!("gpt-5"));
        assert_eq!(after["provider"], json!("openai"));
        assert_eq!(after["permissions"]["allow"], json!(["Read"]));
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("# what answers"),
            "the comment a person wrote is still above the key it is about"
        );
    }

    /// The round trip a command makes — read, change one key, write — must
    /// find the JSON layer before it reads, or the TOML it writes would
    /// shadow keys it never saw (ADR-0058 §2).
    #[test]
    fn a_write_into_a_layer_that_is_still_json_migrates_it_first() {
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join("settings.json");
        write(&json, "{ \"model\": \"old\", \"maxTokens\": 8192 }");

        let toml = dir.path().join("settings.toml");
        super::remember(&toml, &[("model", json!("new"))]).unwrap();

        let after = read_layer(&toml).unwrap().unwrap().value;
        assert_eq!(after["model"], json!("new"));
        assert_eq!(
            after["maxTokens"],
            json!(8192),
            "what the JSON said is in the TOML, not shadowed by it"
        );
        assert!(dir.path().join("settings.json.bak").exists());
        assert!(!json.exists());
    }

    /// `/think off` hands `remember` a null, which is it saying this layer has
    /// nothing to say about the key — and in the lowest layer, which is the
    /// only one a command writes, that is what an absent key already means.
    #[test]
    fn a_null_handed_to_remember_takes_the_key_out_of_the_layer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        write(&path, "# mine\nmodel = \"m\"\nthinking = \"xHigh\"\n");

        super::remember(&path, &[("thinking", Value::Null)]).unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# mine\nmodel = \"m\"\n"
        );
    }

    /// A layer that spells a `null` cannot cross, and a command that would
    /// have written over it says so rather than losing the rest of the file.
    #[test]
    fn a_write_into_a_json_layer_that_spells_a_null_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join("settings.json");
        let before = "{ \"model\": \"m\", \"permissions\": { \"allow\": null } }";
        write(&json, before);

        let toml = dir.path().join("settings.toml");
        let refused = super::remember(&toml, &[("model", json!("new"))]).unwrap_err();
        assert!(
            refused.to_string().contains("permissions.allow"),
            "{refused}"
        );
        assert_eq!(std::fs::read_to_string(&json).unwrap(), before);
        assert!(!toml.exists(), "and nothing was written over it");
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

    fn layer(source: &str, value: Value) -> Layer {
        let Value::Object(map) = value else {
            panic!("a layer is an object")
        };
        Layer::new(source, map)
    }

    /// The highest layer that names `pictures` decides, in either spelling,
    /// and where none does the cache's own default is left to the cache.
    #[test]
    fn the_cache_life_comes_from_the_highest_layer_that_names_it() {
        assert_eq!(picture_cache_days(&[]).unwrap(), None);
        assert_eq!(
            picture_cache_days(&[layer("user", json!({ "model": "m" }))]).unwrap(),
            None,
            "a layer that says nothing about pictures says nothing"
        );
        let layers = [
            layer("user", json!({ "pictures": { "cacheDays": 30 } })),
            layer("project", json!({ "pictures": { "cache_days": 3 } })),
        ];
        assert_eq!(
            picture_cache_days(&layers).unwrap(),
            Some(3),
            "the ask's own spelling reads too, and the higher layer wins"
        );
        assert_eq!(
            picture_cache_days(&layers[..1]).unwrap(),
            Some(30),
            "and so does the settled one"
        );
    }

    #[test]
    fn never_caching_is_a_number_like_any_other() {
        let layers = [layer("user", json!({ "pictures": { "cacheDays": 0 } }))];
        assert_eq!(picture_cache_days(&layers).unwrap(), Some(0));
    }

    /// A `null` clears what the layers below said, as it does everywhere else.
    #[test]
    fn a_null_over_a_life_gives_the_default_back() {
        for higher in [json!(null), json!({}), json!({ "cacheDays": null })] {
            let layers = [
                layer("user", json!({ "pictures": { "cacheDays": 30 } })),
                layer("project", json!({ "pictures": higher.clone() })),
            ];
            assert_eq!(picture_cache_days(&layers).unwrap(), None, "{higher}");
        }
    }

    /// A typo inside `pictures` is not a top-level key, so nothing else would
    /// ever catch it: it is a startup failure that names the layer.
    #[test]
    fn a_key_no_one_knows_under_pictures_is_said_out_loud() {
        let layers = [layer("user", json!({ "pictures": { "cacheDaze": 3 } }))];
        let error = picture_cache_days(&layers).expect_err("a typo");
        assert!(error.to_string().contains("pictures.cacheDaze"), "{error}");
        assert!(error.to_string().contains("cacheDays"), "{error}");
        assert!(error.to_string().contains("user"), "{error}");
    }

    #[test]
    fn a_life_that_is_not_a_number_of_days_is_refused() {
        for bad in [json!("forever"), json!(-1), json!(1.5), json!([14])] {
            let layers = [layer("user", json!({ "pictures": { "cacheDays": bad } }))];
            let error = picture_cache_days(&layers).expect_err("{bad}");
            assert!(
                error.to_string().contains("whole number of days"),
                "{error}"
            );
        }
        let layers = [layer("user", json!({ "pictures": 14 }))];
        let error = picture_cache_days(&layers).expect_err("not an object");
        assert!(error.to_string().contains("expected an object"), "{error}");
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

    /// A TOML root is a table, so a layer is never anything but an object;
    /// the only way it is not a layer is that it is not TOML at all.
    #[test]
    fn a_toml_layer_that_will_not_parse_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        write(&env.config_dir.join("settings.toml"), "model = ");
        let err = load(&env, dir.path(), None).unwrap_err();
        assert!(matches!(err, SettingsError::Parse { .. }), "{err}");

        write(&env.config_dir.join("settings.toml"), "# nothing yet\n");
        let layers = load(&env, dir.path(), None).unwrap();
        assert_eq!(layers.len(), 1, "a file that exists is a layer");
        assert!(
            layers[0].value.is_empty(),
            "one that happens to say nothing"
        );
    }
}
