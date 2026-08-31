//! The library on disk: `<config_dir>/experience/<project>/<id>.md`, rebuilt
//! from the directory on every read (ADR-0014). There is no index to drift —
//! `grep` and `rm` work on it, and a file nobody can parse costs one entry and
//! is said out loud, never silently skipped.
//!
//! The reads are synchronous: a permission card's `preview` is a synchronous
//! call, and one shelf of small files read two ways would be two
//! representations of one store.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::entry::Entry;
use crate::{frontmatter, id, project};

const DIR: &str = "experience";

/// Every entry this project has, and every file that was meant to be one.
#[derive(Debug, Default)]
pub struct Shelf {
    /// In id order: the corpus a ranking indexes into, and what ties fall back
    /// on, are the same order for everyone.
    pub entries: Vec<Entry>,
    pub unreadable: Vec<Unreadable>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Unreadable {
    pub file: String,
    pub why: String,
}

impl Shelf {
    pub fn active(&self) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(|entry| entry.status == crate::entry::Status::Active)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Where the entries live, and which project they belong to. One per process;
/// the project key is worked out once per directory and kept, because the old
/// project spawned two `git` processes every turn to ask the same question.
#[derive(Debug)]
pub struct Library {
    root: PathBuf,
    keys: Mutex<HashMap<PathBuf, String>>,
}

impl Library {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            root: config_dir.join(DIR),
            keys: Mutex::new(HashMap::new()),
        }
    }

    /// This project's directory. It may not exist: nothing here creates it
    /// until something is written.
    pub fn dir(&self, cwd: &Path) -> PathBuf {
        self.root.join(self.key(cwd))
    }

    /// Where one entry lives. The file may not exist; the id is the name.
    pub fn path(&self, cwd: &Path, id: &str) -> PathBuf {
        self.dir(cwd).join(format!("{id}.md"))
    }

    /// Whether there is a store at all — the one check a contributor makes
    /// before it reads anything.
    pub fn occupied(&self, cwd: &Path) -> bool {
        self.dir(cwd).is_dir()
    }

    pub fn load(&self, cwd: &Path) -> Shelf {
        let dir = self.dir(cwd);
        let Ok(files) = std::fs::read_dir(&dir) else {
            return Shelf::default();
        };
        let mut shelf = Shelf::default();
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_none_or(|ext| ext != "md") {
                continue;
            }
            match read(&path) {
                Ok(entry) => shelf.entries.push(entry),
                Err(why) => shelf.unreadable.push(Unreadable {
                    file: file.file_name().to_string_lossy().into_owned(),
                    why,
                }),
            }
        }
        shelf.entries.sort_by(|a, b| a.id.cmp(&b.id));
        shelf.unreadable.sort_by(|a, b| a.file.cmp(&b.file));
        shelf
    }

    /// The entry's file, written whole or not at all.
    pub fn save(&self, cwd: &Path, entry: &Entry) -> std::io::Result<PathBuf> {
        let dir = self.dir(cwd);
        std::fs::create_dir_all(&dir)?;
        let path = self.path(cwd, &entry.id);
        let tmp = dir.join(format!(".{}.tmp", entry.id));
        std::fs::write(&tmp, frontmatter::to_markdown(entry))?;
        match std::fs::rename(&tmp, &path) {
            Ok(()) => Ok(path),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        }
    }

    /// Forget one entry. A file that is already gone is not an error.
    pub fn delete(&self, cwd: &Path, id: &str) -> std::io::Result<()> {
        match std::fs::remove_file(self.path(cwd, id)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    /// An id no file in this project has yet.
    pub fn mint(&self, cwd: &Path) -> String {
        std::iter::repeat_with(id::mint)
            .find(|id| !self.path(cwd, id).exists())
            .unwrap_or_else(id::mint)
    }

    fn key(&self, cwd: &Path) -> String {
        let mut cache = self.keys.lock().unwrap_or_else(|held| held.into_inner());
        cache
            .entry(cwd.to_path_buf())
            .or_insert_with(|| project::key(cwd))
            .clone()
    }
}

/// One file as an entry, or why it is not one. The stem is the id: it is
/// stored nowhere inside the file, so renaming the file renames the entry.
fn read(path: &Path) -> Result<Entry, String> {
    let id = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .ok_or_else(|| "no file name".to_string())?;
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    frontmatter::parse(&id, &text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Status, tests::entry};

    struct Project {
        home: tempfile::TempDir,
        library: Library,
    }

    impl Project {
        fn new() -> Self {
            let home = tempfile::tempdir().expect("a temp home");
            let library = Library::new(&home.path().join("config"));
            Self { home, library }
        }

        fn cwd(&self) -> PathBuf {
            self.home.path().to_path_buf()
        }
    }

    #[test]
    fn a_store_that_does_not_exist_is_an_empty_shelf() {
        let project = Project::new();
        assert!(!project.library.occupied(&project.cwd()));
        let shelf = project.library.load(&project.cwd());
        assert!(shelf.is_empty() && shelf.unreadable.is_empty());
    }

    #[test]
    fn an_entry_round_trips_through_the_directory() {
        let project = Project::new();
        let cwd = project.cwd();
        let mut written = entry();
        written.notes = "and mind the lockfile".into();
        let path = project.library.save(&cwd, &written).expect("saved");
        assert_eq!(path.file_name().expect("a name"), "abcd1234.md");
        assert!(project.library.occupied(&cwd));

        let shelf = project.library.load(&cwd);
        assert_eq!(shelf.entries, [written]);
        assert!(shelf.unreadable.is_empty());
        assert!(
            !path.with_file_name(".abcd1234.tmp").exists(),
            "the temporary file was left behind"
        );
    }

    #[test]
    fn the_shelf_is_in_id_order_whatever_the_directory_says() {
        let project = Project::new();
        let cwd = project.cwd();
        for id in ["ccc", "aaa", "bbb"] {
            let entry = Entry {
                id: id.into(),
                ..entry()
            };
            project.library.save(&cwd, &entry).expect("saved");
        }
        let shelf = project.library.load(&cwd);
        let ids: Vec<&str> = shelf.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["aaa", "bbb", "ccc"]);
    }

    #[test]
    fn a_file_nobody_can_parse_costs_one_entry_and_is_named() {
        let project = Project::new();
        let cwd = project.cwd();
        project.library.save(&cwd, &entry()).expect("saved");
        let dir = project.library.dir(&cwd);
        std::fs::write(dir.join("broken.md"), "not an entry at all\n").expect("wrote");
        std::fs::write(dir.join("README.txt"), "not an entry either\n").expect("wrote");

        let shelf = project.library.load(&cwd);
        assert_eq!(shelf.entries.len(), 1, "the good entry is still there");
        assert_eq!(shelf.unreadable.len(), 1);
        assert_eq!(shelf.unreadable[0].file, "broken.md");
        assert!(
            shelf.unreadable[0].why.contains("frontmatter"),
            "{:?}",
            shelf.unreadable[0]
        );
    }

    #[test]
    fn saving_the_same_id_replaces_the_file_and_forgetting_removes_it() {
        let project = Project::new();
        let cwd = project.cwd();
        project.library.save(&cwd, &entry()).expect("saved");
        let revised = Entry {
            summary: "clear the target directory, then rebuild".into(),
            status: Status::Retired,
            ..entry()
        };
        project.library.save(&cwd, &revised).expect("saved again");
        let shelf = project.library.load(&cwd);
        assert_eq!(shelf.entries, [revised]);
        assert_eq!(shelf.active().count(), 0);

        project.library.delete(&cwd, "abcd1234").expect("deleted");
        assert!(project.library.load(&cwd).is_empty());
        project
            .library
            .delete(&cwd, "abcd1234")
            .expect("forgetting twice is not an error");
    }

    #[test]
    fn a_minted_id_is_free_in_this_project() {
        let project = Project::new();
        let cwd = project.cwd();
        project.library.save(&cwd, &entry()).expect("saved");
        let minted = project.library.mint(&cwd);
        assert_ne!(minted, "abcd1234");
        assert!(
            !project
                .library
                .dir(&cwd)
                .join(format!("{minted}.md"))
                .exists()
        );
    }

    #[test]
    fn two_directories_are_two_stores() {
        let project = Project::new();
        let other = tempfile::tempdir().expect("another directory");
        project
            .library
            .save(&project.cwd(), &entry())
            .expect("saved");
        assert!(project.library.load(other.path()).is_empty());
        assert_ne!(
            project.library.dir(&project.cwd()),
            project.library.dir(other.path())
        );
    }
}
