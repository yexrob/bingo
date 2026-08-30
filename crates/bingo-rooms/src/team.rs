//! `.bingo/team.json`, and of that file only its `rooms`. A project says who
//! sits where; the file is shared with the plugins that own the other nouns,
//! so every other key in it belongs to somebody else and is read by nobody
//! here.

use std::path::{Path, PathBuf};

use serde::Deserialize;

const DIR: &str = ".bingo";
const FILE: &str = "team.json";

/// One room a project declares.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    #[serde(default)]
    pub members: Vec<String>,
}

/// Only the key this plugin owns; serde drops the rest of the file.
#[derive(Debug, Default, Deserialize)]
struct Team {
    #[serde(default)]
    rooms: Vec<Entry>,
}

#[derive(Debug, thiserror::Error)]
pub enum TeamError {
    #[error("{}: {source}", path.display())]
    Unreadable {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{}: {source}", path.display())]
    Malformed {
        path: PathBuf,
        source: serde_json::Error,
    },
}

/// The rooms declared for a session working in `cwd`: the nearest
/// `.bingo/team.json` at or above it, and nothing from the ones further up — a
/// project that declares rooms declares all of them. A file that is not there
/// declares none.
pub fn rooms(cwd: &Path) -> Result<Vec<Entry>, TeamError> {
    let Some(path) = nearest(cwd) else {
        return Ok(Vec::new());
    };
    let source = std::fs::read_to_string(&path).map_err(|source| TeamError::Unreadable {
        path: path.clone(),
        source,
    })?;
    let team: Team =
        serde_json::from_str(&source).map_err(|source| TeamError::Malformed { path, source })?;
    Ok(team.rooms)
}

/// The first `.bingo/team.json` at or above `cwd`.
fn nearest(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .map(|dir| dir.join(DIR).join(FILE))
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project tree with a team file at `at`, and a directory below it.
    fn project(at: &str, source: &str) -> (tempfile::TempDir, PathBuf) {
        let home = tempfile::tempdir().expect("a temporary home");
        let root = home.path().join(at);
        let file = root.join(DIR).join(FILE);
        std::fs::create_dir_all(file.parent().expect("a directory")).expect("a directory");
        std::fs::write(&file, source).expect("a file");
        let deep = root.join("crates").join("thing");
        std::fs::create_dir_all(&deep).expect("a directory");
        (home, deep)
    }

    #[test]
    fn a_declared_room_names_its_members() {
        let (_home, cwd) = project(
            "work",
            r#"{"rooms": [{"name": "design", "members": ["reviewer", "scout"]}]}"#,
        );
        assert_eq!(
            rooms(&cwd).expect("a team file"),
            [Entry {
                name: "design".into(),
                members: ["reviewer", "scout"].map(str::to_string).to_vec(),
            }]
        );
    }

    #[test]
    fn every_other_key_in_the_file_belongs_to_somebody_else() {
        let (_home, cwd) = project(
            "work",
            r#"{
                "roles": [{"name": "reviewer", "agent": "reviewer"}],
                "norms": "NORMS.md",
                "rooms": [{"name": "design"}]
            }"#,
        );
        let declared = rooms(&cwd).expect("a team file");
        assert_eq!(declared.len(), 1);
        assert_eq!(declared[0].name, "design");
        assert!(declared[0].members.is_empty(), "a room may seat nobody");
    }

    #[test]
    fn a_file_without_rooms_and_no_file_at_all_declare_none() {
        let (_home, cwd) = project("work", r#"{"roles": []}"#);
        assert!(rooms(&cwd).expect("a team file").is_empty());

        let empty = tempfile::tempdir().expect("a temporary home");
        assert!(rooms(empty.path()).expect("no team file").is_empty());
    }

    #[test]
    fn the_nearest_file_wins_and_the_ones_above_it_are_not_merged() {
        let (home, _) = project("work", r#"{"rooms": [{"name": "outer"}]}"#);
        let inner = home.path().join("work").join("crates").join("thing");
        let file = inner.join(DIR).join(FILE);
        std::fs::create_dir_all(file.parent().expect("a directory")).expect("a directory");
        std::fs::write(&file, r#"{"rooms": [{"name": "inner"}]}"#).expect("a file");

        let declared = rooms(&inner).expect("a team file");
        assert_eq!(declared.len(), 1);
        assert_eq!(declared[0].name, "inner");
    }

    #[test]
    fn a_file_that_will_not_parse_says_which_one() {
        let (_home, cwd) = project("work", "{ not json");
        let error = rooms(&cwd).expect_err("a malformed team file");
        assert!(matches!(error, TeamError::Malformed { .. }), "{error}");
        assert!(error.to_string().contains(FILE), "{error}");
    }
}
