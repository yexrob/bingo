//! Prompt history: the file under the surface's data directory, and the cursor
//! that walks it. A history that cannot be written is a warning, never an
//! error — nobody loses a turn because a directory is read-only.

use std::path::{Path, PathBuf};

/// How many past prompts are loaded at start.
const KEPT: usize = 1000;

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("history.jsonl")
}

/// The last [`KEPT`] prompts, oldest first. Unreadable lines are skipped.
pub fn load(data_dir: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path(data_dir)) else {
        return Vec::new();
    };
    let lines: Vec<String> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<String>(line).ok())
        .collect();
    let start = lines.len().saturating_sub(KEPT);
    lines[start..].to_vec()
}

/// Append one prompt as a JSON string, creating the directory if it is missing.
pub fn append(data_dir: &Path, line: &str) {
    if let Err(e) = write(data_dir, line) {
        tracing::warn!(error = %e, "could not append to the prompt history");
    }
}

fn write(data_dir: &Path, line: &str) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(data_dir)?;
    let record = serde_json::to_string(line)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path(data_dir))?;
    writeln!(file, "{record}")
}

/// Where the up/down keys are in the history, and the draft they left behind.
#[derive(Clone, Debug, Default)]
pub struct PromptHistory {
    entries: Vec<String>,
    cursor: Option<usize>,
    draft: Option<String>,
}

impl PromptHistory {
    pub fn new(entries: Vec<String>) -> Self {
        Self {
            entries,
            cursor: None,
            draft: None,
        }
    }

    /// One step towards the oldest prompt. `current` becomes the draft the
    /// walk returns to.
    pub fn older(&mut self, current: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let next = match self.cursor {
            None => {
                self.draft = Some(current.to_string());
                self.entries.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.cursor = Some(next);
        self.entries.get(next).cloned()
    }

    /// One step towards the present; past the newest entry is the draft again.
    pub fn newer(&mut self) -> Option<String> {
        let i = self.cursor?;
        if i + 1 < self.entries.len() {
            self.cursor = Some(i + 1);
            return self.entries.get(i + 1).cloned();
        }
        self.cursor = None;
        Some(self.draft.take().unwrap_or_default())
    }

    /// A submitted prompt joins the history and ends the walk. Repeating the
    /// last line does not add a second copy.
    pub fn remember(&mut self, line: &str) {
        self.reset();
        if line.trim().is_empty() || self.entries.last().is_some_and(|last| last == line) {
            return;
        }
        self.entries.push(line.to_string());
    }

    /// Editing the buffer means the caret is no longer walking.
    pub fn reset(&mut self) {
        self.cursor = None;
        self.draft = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walking_back_and_forth_returns_to_the_draft() {
        let mut h = PromptHistory::new(vec!["one".into(), "two".into()]);
        assert_eq!(h.older("draft").as_deref(), Some("two"));
        assert_eq!(h.older("draft").as_deref(), Some("one"));
        assert_eq!(h.older("draft").as_deref(), Some("one"), "the oldest holds");
        assert_eq!(h.newer().as_deref(), Some("two"));
        assert_eq!(h.newer().as_deref(), Some("draft"));
        assert_eq!(h.newer(), None, "past the draft there is nothing");
    }

    #[test]
    fn an_empty_history_never_moves() {
        assert_eq!(PromptHistory::default().older("x"), None);
    }

    #[test]
    fn a_repeated_prompt_is_not_stored_twice() {
        let mut h = PromptHistory::default();
        h.remember("ls");
        h.remember("ls");
        h.remember("  ");
        assert_eq!(h.entries, vec!["ls"]);
    }

    #[test]
    fn the_file_round_trips_through_load_and_append() {
        let dir = tempfile::tempdir().unwrap();
        append(dir.path(), "hello \"world\"");
        append(dir.path(), "line\ntwo");
        assert_eq!(load(dir.path()), vec!["hello \"world\"", "line\ntwo"]);
    }

    #[test]
    fn a_missing_file_is_an_empty_history() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(&dir.path().join("nope")).is_empty());
    }
}
