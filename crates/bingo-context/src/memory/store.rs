//! The memory directory on disk, read: the index as it stands and the files
//! that parse. Nothing here writes one — the model does, with `Write` and
//! `Edit`, which is the whole of ADR-0049.

use std::path::Path;

use crate::memory::dir;
use crate::memory::file::{self, Memory};

/// The index as it stands, or nothing at all when the directory is new.
pub async fn index_text(scope: &Path) -> String {
    tokio::fs::read_to_string(dir::index(scope))
        .await
        .unwrap_or_default()
}

/// Every memory the scope holds, in name order. A file that is not a memory —
/// the index, a note somebody dropped in, a frontmatter nobody finished — is
/// not one, and is passed over rather than guessed at.
pub async fn list(scope: &Path) -> Vec<Memory> {
    let mut memories = Vec::new();
    let Ok(mut reading) = tokio::fs::read_dir(scope).await else {
        return memories;
    };
    while let Ok(Some(entry)) = reading.next_entry().await {
        if let Some(memory) = read(&entry.path()).await {
            memories.push(memory);
        }
    }
    memories.sort_by(|a, b| a.name.cmp(&b.name));
    memories
}

async fn read(path: &Path) -> Option<Memory> {
    let name = path.file_stem()?.to_str()?;
    if path.extension()?.to_str()? != "md" {
        return None;
    }
    let text = tokio::fs::read_to_string(path).await.ok()?;
    file::parse(name, &text).ok()
}

/// A memory written the way the model is taught to write one: the file, and
/// its line appended to the index. For the tests that need a scope with
/// something in it.
#[cfg(test)]
pub async fn save(scope: &Path, memory: &Memory) -> std::io::Result<()> {
    tokio::fs::create_dir_all(scope).await?;
    tokio::fs::write(dir::file(scope, &memory.name), file::print(memory)).await?;
    let line = format!(
        "- [{}]({}.md) — {}\n",
        title(&memory.name),
        memory.name,
        memory.description
    );
    let index = index_text(scope).await + &line;
    tokio::fs::write(dir::index(scope), index).await
}

/// A slug as a person reads it: the hyphens are spaces and the first letter
/// is a capital.
#[cfg(test)]
fn title(name: &str) -> String {
    let words = name.replace('-', " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => words,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::file::Kind;

    fn memory(name: &str, description: &str) -> Memory {
        Memory {
            name: name.into(),
            description: description.into(),
            kind: Kind::Project,
            body: format!("the whole of {name}\n"),
        }
    }

    fn scope() -> tempfile::TempDir {
        tempfile::tempdir().expect("a scope")
    }

    #[tokio::test]
    async fn a_saved_memory_is_a_file_and_a_line() {
        let scope = scope();
        let at = scope.path().join("project");
        save(&at, &memory("a-fact", "one line"))
            .await
            .expect("a memory");
        assert_eq!(index_text(&at).await, "- [A fact](a-fact.md) — one line\n");
        assert_eq!(list(&at).await, [memory("a-fact", "one line")]);
    }

    #[tokio::test]
    async fn a_second_memory_is_a_second_line_not_a_second_index() {
        let scope = scope();
        let at = scope.path().join("project");
        save(&at, &memory("a-fact", "one line")).await.expect("one");
        save(&at, &memory("b-fact", "two lines"))
            .await
            .expect("two");
        assert_eq!(index_text(&at).await.lines().count(), 2);
        assert_eq!(list(&at).await.len(), 2);
    }

    #[tokio::test]
    async fn nothing_a_directory_holds_but_memories_is_read_as_one() {
        let scope = scope();
        let at = scope.path().join("project");
        save(&at, &memory("a-fact", "one line")).await.expect("one");
        std::fs::write(at.join("notes.txt"), "not a memory").expect("a note");
        std::fs::write(at.join("half.md"), "---\nname: half\n").expect("a half");
        assert_eq!(list(&at).await, [memory("a-fact", "one line")]);
    }

    #[tokio::test]
    async fn a_directory_that_is_not_there_holds_nothing() {
        let scope = scope();
        let at = scope.path().join("never-written");
        assert!(list(&at).await.is_empty());
        assert_eq!(index_text(&at).await, "");
    }
}
