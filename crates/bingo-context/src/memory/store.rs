//! The memory directory on disk: what it holds, and how a memory gets in.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::memory::dir;
use crate::memory::file::{self, Memory};
use crate::memory::index;

/// The index as it stands, or nothing at all when the directory is new.
pub async fn index_text(scope: &Path) -> String {
    tokio::fs::read_to_string(dir::index(scope))
        .await
        .unwrap_or_default()
}

/// Whether the scope already holds a memory of this name.
pub async fn holds(scope: &Path, name: &str) -> bool {
    tokio::fs::try_exists(dir::file(scope, name))
        .await
        .unwrap_or(false)
}

/// The memory, and then its line in the index. Both files are written under a
/// name of their own and renamed into place, and the index is read in the
/// moment before it is rewritten, so two sessions writing at once cost at
/// worst one line — never a file, and never half of one.
pub async fn save(scope: &Path, memory: &Memory) -> std::io::Result<()> {
    tokio::fs::create_dir_all(scope).await?;
    swap(&dir::file(scope, &memory.name), &file::print(memory)).await?;
    let text = index::with(&index_text(scope).await, &index::of(memory));
    swap(&dir::index(scope), &text).await
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

/// One file replaced whole: written beside itself and renamed over, so a
/// reader sees the old bytes or the new ones and never half of either.
async fn swap(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = beside(path);
    tokio::fs::write(&tmp, text).await?;
    if let Err(error) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(error);
    }
    Ok(())
}

/// A name no other writer will pick: this process, and one number for every
/// file this process writes.
fn beside(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    path.with_file_name(name)
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
        assert!(holds(&at, "a-fact").await);
        assert!(!holds(&at, "another-fact").await);
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
    async fn saving_a_memory_again_corrects_its_line() {
        let scope = scope();
        let at = scope.path().join("project");
        save(&at, &memory("a-fact", "one line")).await.expect("one");
        save(&at, &memory("a-fact", "corrected"))
            .await
            .expect("again");
        assert_eq!(index_text(&at).await.lines().count(), 1);
        assert!(index_text(&at).await.contains("corrected"));
        assert_eq!(list(&at).await.len(), 1);
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
        assert!(!holds(&at, "a-fact").await);
    }

    #[tokio::test]
    async fn two_writers_leave_two_files_and_no_temporary_ones() {
        let scope = scope();
        let at = scope.path().join("project");
        let (one, two) = (memory("a-fact", "one line"), memory("b-fact", "two lines"));
        let (first, second) = tokio::join!(save(&at, &one), save(&at, &two));
        first.expect("one");
        second.expect("two");
        assert_eq!(list(&at).await.len(), 2);
        let names: Vec<String> = std::fs::read_dir(&at)
            .expect("the scope")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(!names.iter().any(|n| n.ends_with(".tmp")), "{names:?}");
    }

    #[test]
    fn a_temporary_name_belongs_to_one_writer_and_one_file() {
        let path = Path::new("/data/memory/user/a-fact.md");
        let first = beside(path);
        assert_ne!(first, beside(path));
        assert_eq!(first.parent(), path.parent());
        let name = first.file_name().unwrap_or_default().to_string_lossy();
        assert!(name.starts_with("a-fact.md."), "{name}");
        assert!(name.ends_with(".tmp"), "{name}");
    }
}
