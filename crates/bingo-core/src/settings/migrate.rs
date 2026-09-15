//! The one move from `settings.json` to `settings.toml` (ADR-0058 §2). It
//! runs for every layer at the start of every run and once more inside any
//! read-modify-write of a TOML layer, and it is the same function both times:
//! a layer crosses once, and a layer that has crossed is left alone.

use std::path::{Path, PathBuf};

use bingo_sdk::Env;

use super::format::{self, Format};
use super::{SettingsError, layer_paths, read_layer, write_toml};

/// What one layer's migration came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Migration {
    /// A TOML layer is already the layer, or no JSON stands beside it.
    Nothing,
    /// The JSON crossed. `kept` is true when a `.bak` was already there, so
    /// the original stays where it is — unread, because the TOML shadows it.
    Done {
        from: PathBuf,
        to: PathBuf,
        kept: bool,
    },
    /// TOML has no word for the `null` this file spells, so the JSON stays
    /// the layer until a person rewrites it.
    Refused { from: PathBuf, key: String },
}

impl Migration {
    /// What to say about it on stderr, and under which code. `Nothing` is
    /// silent: a run with nothing to move has nothing to say.
    pub fn notice(&self) -> Option<(&'static str, String)> {
        match self {
            Migration::Nothing => None,
            Migration::Done { from, to, kept: false } => Some((
                "SETTINGS_MIGRATED",
                format!(
                    "moved {} to {}; the original is {}",
                    from.display(),
                    to.display(),
                    backup(from).display()
                ),
            )),
            Migration::Done { from, to, kept: true } => Some((
                "SETTINGS_MIGRATED",
                format!(
                    "wrote {} from {}; {} was already there, so {} is left where it is \
                     and is no longer read",
                    to.display(),
                    from.display(),
                    backup(from).display(),
                    from.display()
                ),
            )),
            Migration::Refused { from, key } => Some((
                "SETTINGS_KEPT_JSON",
                format!(
                    "{} still reads as JSON: TOML has no null, and `{key}` is one",
                    from.display()
                ),
            )),
        }
    }
}

/// Every layer, in the order they are read. One layer's trouble is its own:
/// the others still cross, and the caller reports what each came to.
pub fn migrate_all(env: &Env, cwd: &Path) -> Vec<Result<Migration, SettingsError>> {
    layer_paths(env, cwd)
        .iter()
        .map(|path| migrate_one(path))
        .collect()
}

/// One layer's `settings.json`, converted and written as `settings.toml`,
/// with the original kept as `settings.json.bak` — the comments do not cross
/// (ADR-0058 §4) and the `.bak` is where they stay.
pub fn migrate_one(toml: &Path) -> Result<Migration, SettingsError> {
    if Format::of(toml) != Format::Toml || toml.exists() {
        return Ok(Migration::Nothing);
    }
    let json = format::json_sibling(toml);
    let Some(layer) = read_layer(&json)? else {
        return Ok(Migration::Nothing);
    };
    match write_toml(toml, &layer.value) {
        Err(SettingsError::Null { key }) => Ok(Migration::Refused { from: json, key }),
        Err(e) => Err(e),
        Ok(()) => {
            let kept = keep_original(&json)?;
            Ok(Migration::Done {
                from: json,
                to: toml.to_path_buf(),
                kept,
            })
        }
    }
}

/// The JSON out of the way. A `.bak` already there is never overwritten: it
/// is the older original, and the older one is the one worth keeping — so the
/// JSON is left where it is instead, where the TOML now shadows it.
fn keep_original(json: &Path) -> Result<bool, SettingsError> {
    let backup = backup(json);
    if backup.exists() {
        return Ok(true);
    }
    std::fs::rename(json, &backup).map_err(|source| SettingsError::Write {
        path: backup,
        source,
    })?;
    Ok(false)
}

/// `settings.json` → `settings.json.bak`: the whole name kept, so what it was
/// written in is still legible from the outside.
fn backup(json: &Path) -> PathBuf {
    let mut name = json.to_path_buf().into_os_string();
    name.push(".bak");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    /// The comments do not cross, which is the whole reason the `.bak` is
    /// kept rather than deleted.
    #[test]
    fn a_jsonc_layer_crosses_and_the_bak_keeps_what_did_not() {
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join("settings.json");
        write(&json, "{\n  // what answers\n  \"provider\": \"openai\"\n}");

        let toml = dir.path().join("settings.toml");
        let done = migrate_one(&toml).unwrap();
        assert_eq!(
            done,
            Migration::Done {
                from: json.clone(),
                to: toml.clone(),
                kept: false,
            }
        );
        assert_eq!(read(&toml), "provider = \"openai\"\n");
        assert!(read(&backup(&json)).contains("// what answers"));
        assert!(!json.exists(), "the original moved, it was not copied");

        let (code, said) = done.notice().unwrap();
        assert_eq!(code, "SETTINGS_MIGRATED");
        assert!(said.contains("settings.json.bak"), "{said}");
    }

    #[test]
    fn a_layer_that_has_crossed_is_left_alone_on_every_run_after() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("settings.json"), "{ \"model\": \"m\" }");
        let toml = dir.path().join("settings.toml");
        assert!(matches!(migrate_one(&toml).unwrap(), Migration::Done { .. }));
        assert_eq!(migrate_one(&toml).unwrap(), Migration::Nothing);
        assert_eq!(migrate_one(&toml).unwrap().notice(), None);
    }

    #[test]
    fn nothing_to_move_is_nothing_said() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            migrate_one(&dir.path().join("settings.toml")).unwrap(),
            Migration::Nothing
        );
        let json = dir.path().join("settings.json");
        write(&json, "{ \"model\": \"m\" }");
        assert_eq!(
            migrate_one(&json).unwrap(),
            Migration::Nothing,
            "a JSON path is not a layer to migrate"
        );
    }

    /// TOML cannot spell the tri-state of ADR-0003 §3, so the layer stays
    /// JSON and both files are exactly as they were.
    #[test]
    fn a_null_refuses_by_name_and_moves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join("settings.json");
        let before = "{ \"model\": \"m\", \"openai\": { \"apiKey\": null } }";
        write(&json, before);

        let toml = dir.path().join("settings.toml");
        assert_eq!(
            migrate_one(&toml).unwrap(),
            Migration::Refused {
                from: json.clone(),
                key: "openai.apiKey".to_string(),
            }
        );
        assert_eq!(read(&json), before, "the layer is still the JSON");
        assert!(!toml.exists(), "and nothing stands over it");
        assert!(!backup(&json).exists());

        let (code, said) = migrate_one(&toml).unwrap().notice().unwrap();
        assert_eq!(code, "SETTINGS_KEPT_JSON");
        assert!(said.contains("openai.apiKey"), "{said}");
        assert!(said.contains("no null"), "{said}");
    }

    /// A `.bak` from an earlier crossing is the older original: it is the one
    /// worth keeping, so the newer JSON is left where it is instead.
    #[test]
    fn a_bak_already_there_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join("settings.json");
        write(&json, "{ \"model\": \"new\" }");
        write(&backup(&json), "{ \"model\": \"older\" }");

        let toml = dir.path().join("settings.toml");
        assert_eq!(
            migrate_one(&toml).unwrap(),
            Migration::Done {
                from: json.clone(),
                to: toml.clone(),
                kept: true,
            }
        );
        assert_eq!(read(&backup(&json)), "{ \"model\": \"older\" }");
        assert_eq!(read(&json), "{ \"model\": \"new\" }");
        assert_eq!(read(&toml), "model = \"new\"\n");
    }

    #[test]
    fn every_layer_crosses_on_one_call() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::rooted(dir.path());
        let cwd = dir.path().join("project");
        write(&env.config_dir.join("settings.json"), "{ \"model\": \"u\" }");
        write(&cwd.join(".bingo/settings.local.json"), "{ \"model\": \"l\" }");

        let crossed: Vec<_> = migrate_all(&env, &cwd)
            .into_iter()
            .map(|outcome| outcome.unwrap())
            .collect();
        assert!(matches!(crossed[0], Migration::Done { .. }), "user");
        assert_eq!(crossed[1], Migration::Nothing, "no project layer");
        assert!(matches!(crossed[2], Migration::Done { .. }), "local");
        assert_eq!(read(&env.user_settings()), "model = \"u\"\n");
        assert_eq!(
            read(&cwd.join(".bingo/settings.local.toml")),
            "model = \"l\"\n"
        );
    }
}
