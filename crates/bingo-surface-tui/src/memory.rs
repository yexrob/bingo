//! A call on a memory file, drawn as what it is (M84): `⏺ Recall from
//! memory(bingo-rewrite-plan)`, `⏺ Write memory(prefers-short-replies)`,
//! `⏺ Edit memory(the-build)`, so a person can see the model is at its memory
//! rather than at a file.
//!
//! The plugin that keeps memory (`bingo-context`, ADR-0044, ADR-0049) says
//! where it is: the journal extension `_bingo.context`/`memory`, `{ "user":
//! <dir>, "project": <dir> }`, the same two paths its prompt headings name
//! for the model. This reads it as [`crate::tasks`] reads the list — by name,
//! as data, the whole contract in this file — because a surface may not
//! import a plugin (ADR-0001). A session whose journal has no such record
//! draws every call as it was.

use std::path::{Path, PathBuf};

use bingo_sdk::SessionState;
use serde_json::Value;

/// The namespace and the kind the directories are published under.
const PLUGIN: &str = "_bingo.context";
const KIND: &str = "memory";
/// The two scopes, each a directory.
const SCOPES: [&str; 2] = ["user", "project"];
/// The index every scope keeps, which draws under its own name: it is not a
/// fact, and a person knows it by this one.
const INDEX: &str = "MEMORY.md";
/// The argument of the three file tools that names the file.
const FILE_PATH: &str = "file_path";
/// The three tools that touch a file, and what each is when the file is a
/// memory. Any other tool on the same path — a `Grep`, a `Glob` — is what it
/// was: it is searching, not remembering.
const VERBS: [(&str, &str); 3] = [
    ("Read", "Recall from memory"),
    ("Write", "Write memory"),
    ("Edit", "Edit memory"),
];

/// Where the session's memories are, as far as the journal says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Directories(Vec<PathBuf>);

pub fn of(state: &SessionState) -> Directories {
    let Some(published) = state
        .extensions
        .get(PLUGIN)
        .and_then(|kinds| kinds.get(KIND))
    else {
        return Directories::default();
    };
    Directories(
        SCOPES
            .iter()
            .filter_map(|scope| published.get(scope).and_then(Value::as_str))
            .map(PathBuf::from)
            .collect(),
    )
}

/// The row's name and argument when the call is on a memory file — the verb
/// for the tool and the memory's own name — and nothing when it is not.
pub fn call(
    name: &str,
    input: &Value,
    cwd: &str,
    dirs: &Directories,
) -> Option<(&'static str, String)> {
    let (_, verb) = VERBS.iter().find(|(tool, _)| *tool == name)?;
    let path = absolute(input.get(FILE_PATH)?.as_str()?, cwd);
    let inside = dirs
        .0
        .iter()
        .any(|dir| path.parent() == Some(dir.as_path()));
    inside.then(|| (*verb, about(&path)))
}

/// A path the model wrote relative to where it was working.
fn absolute(path: &str, cwd: &str) -> PathBuf {
    let path = Path::new(path);
    match path.is_absolute() {
        true => path.to_path_buf(),
        false => Path::new(cwd).join(path),
    }
}

/// The memory's name as the index links it — the slug, without `.md` — and
/// the index by its own name.
fn about(path: &Path) -> String {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    if name == INDEX {
        return name.into_owned();
    }
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{extended, folded, frame, state};
    use serde_json::json;

    fn published() -> Directories {
        of(&folded(vec![frame(
            1,
            extended(
                PLUGIN,
                KIND,
                json!({"user": "/data/memory/user", "project": "/data/memory/web-2bf6c26a7362cd1f"}),
            ),
        )]))
    }

    #[test]
    fn the_directories_are_read_by_name_and_absent_by_default() {
        assert_eq!(
            published(),
            Directories(vec![
                PathBuf::from("/data/memory/user"),
                PathBuf::from("/data/memory/web-2bf6c26a7362cd1f"),
            ])
        );
        assert_eq!(of(&state()), Directories::default());
    }

    #[test]
    fn a_file_tool_on_a_memory_is_the_memory_verb_and_the_slug() {
        let dirs = published();
        let read = json!({"file_path": "/data/memory/web-2bf6c26a7362cd1f/the-build.md"});
        assert_eq!(
            call("Read", &read, "/work", &dirs),
            Some(("Recall from memory", "the-build".to_string()))
        );
        let write = json!({"file_path": "/data/memory/user/MEMORY.md", "content": "- …"});
        assert_eq!(
            call("Write", &write, "/work", &dirs),
            Some(("Write memory", "MEMORY.md".to_string()))
        );
        let edit = json!({"file_path": "/data/memory/user/prefers-short-replies.md"});
        assert_eq!(
            call("Edit", &edit, "/work", &dirs),
            Some(("Edit memory", "prefers-short-replies".to_string()))
        );
    }

    #[test]
    fn a_relative_path_is_read_from_where_the_session_works() {
        let dirs = published();
        let read = json!({"file_path": "memory/user/a-habit.md"});
        assert_eq!(
            call("Read", &read, "/data", &dirs),
            Some(("Recall from memory", "a-habit".to_string()))
        );
        assert_eq!(call("Read", &read, "/elsewhere", &dirs), None);
    }

    #[test]
    fn anything_else_is_what_it_was() {
        let dirs = published();
        let elsewhere = json!({"file_path": "/work/src/lib.rs"});
        assert_eq!(call("Read", &elsewhere, "/work", &dirs), None);
        let search = json!({"pattern": "habit", "path": "/data/memory/user"});
        assert_eq!(call("Grep", &search, "/work", &dirs), None);
        let deeper = json!({"file_path": "/data/memory/user/notes/a.md"});
        assert_eq!(
            call("Read", &deeper, "/work", &dirs),
            None,
            "a memory is one file deep"
        );
        let read = json!({"file_path": "/data/memory/user/a-habit.md"});
        assert_eq!(call("Read", &read, "/work", &Directories::default()), None);
    }
}
