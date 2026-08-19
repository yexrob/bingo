//! Prompt history, persisted per working directory.
//!
//! Stored next to the transcripts (`~/.local/share/bingo/history/`) as JSONL —
//! one JSON string per line — because prompts may contain newlines and a raw
//! line format could not round-trip them. Every write is best effort: when the
//! file cannot be written the history simply stays in memory for the session.

use std::path::{Path, PathBuf};

/// How many entries are kept per directory (oldest are dropped on write).
pub const HISTORY_MAX: usize = 500;

/// `~/.local/share/bingo/history` (mirrors [`crate::transcript::transcripts_dir`]).
pub fn history_dir(home: &Path) -> PathBuf {
    crate::storage::history_dir(home)
}

/// FNV-1a of the absolute cwd: keeps same-named projects in different parents
/// from sharing a history file.
fn path_hash(cwd: &Path) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in cwd.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// History file for `cwd`: `<basename>-<hash>.jsonl`.
pub fn history_path(home: &Path, cwd: &Path) -> PathBuf {
    let name = cwd
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let slug = if slug.is_empty() {
        "root".to_string()
    } else {
        slug
    };
    history_dir(home).join(format!("{slug}-{:016x}.jsonl", path_hash(cwd)))
}

/// Read the persisted prompts, oldest first. Any unreadable file (missing,
/// truncated, corrupt line) degrades to whatever parsed.
pub fn load(home: &Path, cwd: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(history_path(home, cwd)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<String>(line).ok())
        .filter(|entry| !entry.trim().is_empty())
        .collect()
}

/// Rewrite the file with `entries` (tail-capped at [`HISTORY_MAX`]).
/// Errors are returned so the caller can decide — the TUI ignores them.
pub fn save(home: &Path, cwd: &Path, entries: &[String]) -> std::io::Result<()> {
    let path = history_path(home, cwd);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // The sidecar serializes concurrent writers; the file itself is never locked, so a
    // reader in another process is never failed by a write in progress (D72).
    let lock_path = path.with_extension("jsonl.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    lock_file.lock()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)?;
    let start = entries.len().saturating_sub(HISTORY_MAX);
    let body: String = entries[start..]
        .iter()
        .filter_map(|entry| serde_json::to_string(entry).ok())
        .map(|line| line + "\n")
        .collect();
    file.set_len(0)?;
    use std::io::Write;
    (&file).write_all(body.as_bytes())
}

/// Prompt history with a navigation cursor and the draft it displaced.
#[derive(Debug, Default, Clone)]
pub struct History {
    /// Oldest first.
    entries: Vec<String>,
    /// Position while browsing (`None` = at the live draft).
    cursor: Option<usize>,
    /// Text that was in the prompt when browsing started.
    draft: String,
}

impl History {
    pub fn new(entries: Vec<String>) -> Self {
        Self {
            entries,
            cursor: None,
            draft: String::new(),
        }
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Whether the prompt currently shows a history entry rather than the
    /// live draft.
    #[cfg(test)]
    pub fn is_browsing(&self) -> bool {
        self.cursor.is_some()
    }

    /// Record a submitted prompt. Consecutive duplicates collapse into one.
    /// Returns whether the entry list changed (i.e. worth persisting).
    pub fn record(&mut self, entry: &str) -> bool {
        self.cursor = None;
        self.draft.clear();
        if entry.trim().is_empty() {
            return false;
        }
        if self.entries.last().is_some_and(|last| last == entry) {
            return false;
        }
        self.entries.push(entry.to_string());
        if self.entries.len() > HISTORY_MAX {
            let overflow = self.entries.len() - HISTORY_MAX;
            self.entries.drain(..overflow);
        }
        true
    }

    /// Step to an older entry; `current` is stashed as the draft on the first
    /// step. `None` = nothing older to show.
    pub fn older(&mut self, current: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let next = match self.cursor {
            None => {
                self.draft = current.to_string();
                self.entries.len() - 1
            }
            Some(0) => return None,
            Some(i) => i - 1,
        };
        self.cursor = Some(next);
        self.entries.get(next).cloned()
    }

    /// Step to a newer entry; past the newest it restores the stashed draft.
    pub fn newer(&mut self) -> Option<String> {
        let i = self.cursor?;
        if i + 1 < self.entries.len() {
            self.cursor = Some(i + 1);
            return self.entries.get(i + 1).cloned();
        }
        self.cursor = None;
        Some(std::mem::take(&mut self.draft))
    }

    /// Stop browsing (the user edited the buffer) without touching the text.
    pub fn detach(&mut self) {
        self.cursor = None;
        self.draft.clear();
    }

    /// Reverse search (ctrl+r): newest entry containing `query`, starting
    /// strictly older than `before` when given. Returns (index, entry).
    pub fn search(&self, query: &str, before: Option<usize>) -> Option<(usize, String)> {
        let end = before.unwrap_or(self.entries.len());
        self.entries[..end.min(self.entries.len())]
            .iter()
            .enumerate()
            .rev()
            .find(|(_, entry)| query.is_empty() || entry.contains(query))
            .map(|(i, entry)| (i, entry.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bingo-history-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn round_trips_through_disk() {
        let home = tmp_home("roundtrip");
        let cwd = Path::new("/tmp/project");
        assert!(load(&home, cwd).is_empty(), "missing file = empty history");
        let entries = vec!["first".to_string(), "multi\nline".to_string()];
        save(&home, cwd, &entries).expect("write history");
        assert_eq!(load(&home, cwd), entries, "newlines survive JSONL");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn same_basename_in_different_parents_stays_separate() {
        let home = tmp_home("distinct");
        let a = history_path(&home, Path::new("/a/project"));
        let b = history_path(&home, Path::new("/b/project"));
        assert_ne!(a, b);
        assert!(
            a.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("project-"))
        );
    }

    #[test]
    fn save_caps_the_file() {
        let home = tmp_home("cap");
        let cwd = Path::new("/tmp/capped");
        let entries: Vec<String> = (0..HISTORY_MAX + 20).map(|i| format!("p{i}")).collect();
        save(&home, cwd, &entries).expect("write");
        let loaded = load(&home, cwd);
        assert_eq!(loaded.len(), HISTORY_MAX);
        assert_eq!(loaded.last().map(String::as_str), Some("p519"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn navigation_stashes_and_restores_the_draft() {
        let mut history = History::new(vec!["one".into(), "two".into()]);
        assert_eq!(history.older("draft").as_deref(), Some("two"));
        assert_eq!(history.older("ignored").as_deref(), Some("one"));
        assert_eq!(history.older("ignored"), None, "oldest entry stays put");
        assert_eq!(history.newer().as_deref(), Some("two"));
        assert_eq!(history.newer().as_deref(), Some("draft"), "draft restored");
        assert_eq!(history.newer(), None, "already at the draft");
        assert!(!history.is_browsing());
    }

    #[test]
    fn record_skips_blanks_and_consecutive_duplicates() {
        let mut history = History::new(Vec::new());
        assert!(history.record("hello"));
        assert!(!history.record("hello"), "consecutive duplicate");
        assert!(!history.record("   "), "blank");
        assert!(history.record("world"));
        assert!(history.record("hello"), "non-consecutive repeat is kept");
        assert_eq!(history.entries(), ["hello", "world", "hello"]);
    }

    #[test]
    fn record_resets_browsing() {
        let mut history = History::new(vec!["one".into()]);
        assert_eq!(history.older("draft").as_deref(), Some("one"));
        history.record("two");
        assert!(!history.is_browsing());
        assert_eq!(history.older("draft").as_deref(), Some("two"));
    }

    #[test]
    fn search_walks_backwards_through_matches() {
        let history = History::new(vec![
            "cargo test".into(),
            "git status".into(),
            "cargo build".into(),
        ]);
        let (i, entry) = history.search("cargo", None).expect("newest match");
        assert_eq!((i, entry.as_str()), (2, "cargo build"));
        let (i, entry) = history.search("cargo", Some(i)).expect("older match");
        assert_eq!((i, entry.as_str()), (0, "cargo test"));
        assert_eq!(history.search("cargo", Some(0)), None, "nothing older");
        assert_eq!(history.search("nope", None), None);
        // An empty query matches the newest entry (classic ctrl+r behaviour).
        assert_eq!(history.search("", None).map(|(i, _)| i), Some(2));
    }
}
