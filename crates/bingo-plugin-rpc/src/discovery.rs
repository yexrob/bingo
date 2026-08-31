//! Where a plugin lives, and what reading one directory finds.
//!
//! Two layers: the person's own `<config_dir>/plugins`, then the project's
//! `.bingo/plugins` (ADR-0015 §1). A name in both is the project's — a
//! repository that ships a plugin is describing that repository, and the
//! person who installed one of the same name did not mean this one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bingo_sdk::Env;

use crate::manifest::Manifest;
use crate::notice::{Notice, Notices};

/// The directory a layer keeps its plugins in.
const PLUGINS: &str = "plugins";

/// The directory a project keeps its own configuration in.
const PROJECT: &str = ".bingo";

/// The file a plugin directory is known by.
pub const MANIFEST_FILE: &str = "plugin.json";

/// One plugin directory, read.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// The directory's name, which is the plugin's name.
    pub name: String,
    pub root: PathBuf,
    pub manifest: Manifest,
}

/// Every directory a plugin may live in, least important first: a later layer
/// overrides a name an earlier one used.
pub fn dirs(env: &Env, cwd: &Path) -> Vec<PathBuf> {
    let config = env.config_dir.join(PLUGINS);
    let project = cwd.join(PROJECT).join(PLUGINS);
    if config == project {
        return vec![config];
    }
    vec![config, project]
}

/// What the layers hold, by name, the last layer winning. A directory that is
/// not there is not an error: most people have no plugins.
pub fn discover(dirs: &[PathBuf], notices: &Notices) -> Vec<Found> {
    let mut found: BTreeMap<String, Found> = BTreeMap::new();
    for dir in dirs {
        for plugin in layer(dir, notices) {
            found.insert(plugin.name.clone(), plugin);
        }
    }
    found.into_values().collect()
}

fn layer(dir: &Path, notices: &Notices) -> Vec<Found> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        if let Some(plugin) = read(&entry.path(), notices) {
            found.push(plugin);
        }
    }
    found
}

/// One directory, if it holds a manifest that names itself. A directory with
/// no `plugin.json` is somebody else's; a `plugin.json` that will not parse is
/// a plugin a person meant to have, so that one is reported.
fn read(root: &Path, notices: &Notices) -> Option<Found> {
    let name = root.file_name()?.to_str()?.to_string();
    let raw = std::fs::read_to_string(root.join(MANIFEST_FILE)).ok()?;
    match serde_json::from_str::<Manifest>(&raw) {
        Ok(manifest) if manifest.name == name => Some(Found {
            name,
            root: root.to_path_buf(),
            manifest,
        }),
        Ok(manifest) => {
            notices.push(Notice::warn(
                "PLUGIN_MISNAMED",
                format!(
                    "{}/{MANIFEST_FILE} calls itself `{}`; a plugin is named by its directory",
                    root.display(),
                    manifest.name
                ),
            ));
            None
        }
        Err(error) => {
            notices.push(Notice::warn(
                "PLUGIN_UNREADABLE",
                format!("{}/{MANIFEST_FILE}: {error}", root.display()),
            ));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest(name: &str) -> String {
        json!({
            "name": name,
            "version": "0.1.0",
            "entry": { "command": "python3", "args": ["${PLUGIN_ROOT}/main.py"] }
        })
        .to_string()
    }

    fn plugin(dir: &Path, name: &str, body: &str) {
        let root = dir.join(name);
        std::fs::create_dir_all(&root).expect("a plugin directory");
        std::fs::write(root.join(MANIFEST_FILE), body).expect("a manifest");
    }

    #[test]
    fn the_person_s_own_plugins_come_before_the_project_s() {
        let env = Env::rooted("/home/user");
        assert_eq!(
            dirs(&env, Path::new("/work/repo")),
            [
                PathBuf::from("/home/user/.bingo/plugins"),
                PathBuf::from("/work/repo/.bingo/plugins"),
            ]
        );
    }

    #[test]
    fn a_home_that_is_the_working_directory_is_read_once() {
        let env = Env::rooted("/work");
        assert_eq!(
            dirs(&env, Path::new("/work")),
            [PathBuf::from("/work/.bingo/plugins")]
        );
    }

    #[test]
    fn a_layer_that_is_not_there_finds_nothing_and_says_nothing() {
        let notices = Notices::default();
        assert!(discover(&[PathBuf::from("/no/such/dir")], &notices).is_empty());
        assert!(notices.drain().is_empty());
    }

    #[test]
    fn the_project_wins_a_name_the_person_also_installed() {
        let config = tempfile::tempdir().expect("a config directory");
        let project = tempfile::tempdir().expect("a project directory");
        plugin(config.path(), "wordcount", &manifest("wordcount"));
        plugin(project.path(), "wordcount", &manifest("wordcount"));
        let notices = Notices::default();
        let found = discover(
            &[config.path().to_path_buf(), project.path().to_path_buf()],
            &notices,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].root, project.path().join("wordcount"));
    }

    #[test]
    fn a_directory_with_no_manifest_is_somebody_else_s() {
        let dir = tempfile::tempdir().expect("a directory");
        std::fs::create_dir_all(dir.path().join("notes")).expect("a plain directory");
        let notices = Notices::default();
        assert!(discover(&[dir.path().to_path_buf()], &notices).is_empty());
        assert!(notices.drain().is_empty());
    }

    #[test]
    fn a_manifest_that_will_not_parse_is_reported_not_skipped() {
        let dir = tempfile::tempdir().expect("a directory");
        plugin(dir.path(), "broken", "{ this is not json");
        let notices = Notices::default();
        assert!(discover(&[dir.path().to_path_buf()], &notices).is_empty());
        let said = notices.drain();
        assert_eq!(said.len(), 1);
        assert_eq!(said[0].code, "PLUGIN_UNREADABLE");
    }

    #[test]
    fn a_manifest_that_names_another_plugin_is_refused() {
        let dir = tempfile::tempdir().expect("a directory");
        plugin(dir.path(), "wordcount", &manifest("linecount"));
        let notices = Notices::default();
        assert!(discover(&[dir.path().to_path_buf()], &notices).is_empty());
        let said = notices.drain();
        assert_eq!(said.len(), 1);
        assert_eq!(said[0].code, "PLUGIN_MISNAMED");
        assert!(said[0].text.contains("linecount"), "{}", said[0].text);
    }

    #[test]
    fn what_is_found_is_the_directory_and_the_manifest_in_it() {
        let dir = tempfile::tempdir().expect("a directory");
        plugin(dir.path(), "wordcount", &manifest("wordcount"));
        let found = discover(&[dir.path().to_path_buf()], &Notices::default());
        assert_eq!(found[0].name, "wordcount");
        assert_eq!(found[0].root, dir.path().join("wordcount"));
        assert_eq!(found[0].manifest.entry.command, "python3");
    }
}
