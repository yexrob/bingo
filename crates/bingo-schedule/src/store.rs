//! The store on disk: `<data_dir>/schedules/<id>.json`, rebuilt from the
//! directory on every read (ADR-0019 §1). There is no index to drift — `ls`
//! and `rm` work on it — and a file nobody can parse costs one entry and is
//! said out loud, never silently skipped.
//!
//! The reads are synchronous: a permission card's `preview` is a synchronous
//! call, and one shelf of small files read two ways would be two
//! representations of one store.

use std::path::{Path, PathBuf};

use crate::entry::Entry;
use crate::id;

pub const DIRECTORY: &str = "schedules";

const EXTENSION: &str = "json";

/// Every entry the store has, and every file that was meant to be one.
#[derive(Debug, Default)]
pub struct Shelf {
    /// In id order: what `/schedule` lists and what a prefix resolves
    /// against are the same order for everyone.
    pub entries: Vec<Entry>,
    pub unreadable: Vec<Unreadable>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Unreadable {
    pub file: String,
    pub why: String,
}

impl Shelf {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn ids(&self) -> Vec<&str> {
        self.entries.iter().map(|entry| entry.id.as_str()).collect()
    }
}

/// Where the entries live. One directory for the whole store: a schedule
/// names its own working directory, so it is not filed under one.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join(DIRECTORY),
        }
    }

    /// The directory. It may not exist: nothing here creates it until
    /// something is written.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where one entry lives. The file may not exist; the id is the name.
    pub fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.{EXTENSION}"))
    }

    pub fn load(&self) -> Shelf {
        let Ok(files) = std::fs::read_dir(&self.dir) else {
            return Shelf::default();
        };
        let mut shelf = Shelf::default();
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_none_or(|ext| ext != EXTENSION) {
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
    pub fn save(&self, entry: &Entry) -> std::io::Result<PathBuf> {
        let document = entry.document().map_err(std::io::Error::other)?;
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path(&entry.id);
        let tmp = self.dir.join(format!(".{}.tmp", entry.id));
        std::fs::write(&tmp, document)?;
        match std::fs::rename(&tmp, &path) {
            Ok(()) => Ok(path),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        }
    }

    /// Forget one entry. A file that is already gone is not an error.
    pub fn delete(&self, id: &str) -> std::io::Result<()> {
        match std::fs::remove_file(self.path(id)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    /// An id no file in this store has yet.
    pub fn mint(&self) -> String {
        std::iter::repeat_with(id::mint)
            .find(|id| !self.path(id).exists())
            .unwrap_or_else(id::mint)
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
    let mut entry: Entry = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    entry.id = id;
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::tests::entry;

    struct Data {
        home: tempfile::TempDir,
    }

    impl Data {
        fn new() -> Self {
            Self {
                home: tempfile::tempdir().expect("a temp home"),
            }
        }

        fn store(&self) -> Store {
            Store::new(self.home.path())
        }
    }

    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("a directory")
            .flatten()
            .map(|f| f.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_store_that_does_not_exist_is_an_empty_shelf() {
        let data = Data::new();
        let shelf = data.store().load();
        assert!(shelf.is_empty() && shelf.unreadable.is_empty());
    }

    #[test]
    fn an_entry_round_trips_through_the_directory_under_its_id() {
        let data = Data::new();
        let store = data.store();
        let written = entry();
        let path = store.save(&written).expect("saved");
        assert_eq!(path.file_name().expect("a name"), "abcd1234.json");
        assert_eq!(
            path.parent(),
            Some(data.home.path().join("schedules").as_path())
        );

        let shelf = store.load();
        assert_eq!(shelf.entries, [written]);
        assert!(shelf.unreadable.is_empty());
        assert_eq!(
            files(store.dir()),
            ["abcd1234.json"],
            "no tmp file survives"
        );
    }

    #[test]
    fn the_shelf_is_in_id_order_whatever_the_directory_says() {
        let data = Data::new();
        let store = data.store();
        for id in ["ccc", "aaa", "bbb"] {
            store
                .save(&Entry {
                    id: id.into(),
                    ..entry()
                })
                .expect("saved");
        }
        assert_eq!(store.load().ids(), ["aaa", "bbb", "ccc"]);
    }

    #[test]
    fn a_file_nobody_can_parse_costs_one_entry_and_is_named() {
        let data = Data::new();
        let store = data.store();
        store.save(&entry()).expect("saved");
        std::fs::write(store.dir().join("broken.json"), "{ not json at all\n").expect("wrote");
        std::fs::write(store.dir().join("cron.json"), r#"{"spec":"* * * * *"}"#).expect("wrote");
        std::fs::write(store.dir().join("runner.lock"), "4242").expect("wrote");
        std::fs::write(store.dir().join("README.txt"), "not an entry\n").expect("wrote");

        let shelf = store.load();
        assert_eq!(shelf.ids(), ["abcd1234"], "the good entry is still there");
        let named: Vec<&str> = shelf.unreadable.iter().map(|u| u.file.as_str()).collect();
        assert_eq!(
            named,
            ["broken.json", "cron.json"],
            "the lock and the readme are not entries; a broken entry is"
        );
        assert!(
            shelf.unreadable[1].why.contains("every"),
            "the reason names the grammar: {:?}",
            shelf.unreadable[1]
        );
    }

    #[test]
    fn saving_the_same_id_replaces_the_file_and_forgetting_removes_it() {
        let data = Data::new();
        let store = data.store();
        store.save(&entry()).expect("saved");
        let revised = Entry {
            enabled: false,
            ..entry()
        };
        store.save(&revised).expect("saved again");
        assert_eq!(store.load().entries, [revised]);

        store.delete("abcd1234").expect("deleted");
        assert!(store.load().is_empty());
        store
            .delete("abcd1234")
            .expect("forgetting twice is not an error");
    }

    #[test]
    fn a_minted_id_is_free_in_this_store() {
        let data = Data::new();
        let store = data.store();
        store.save(&entry()).expect("saved");
        let minted = store.mint();
        assert_ne!(minted, "abcd1234");
        assert!(!store.path(&minted).exists());
    }
}
