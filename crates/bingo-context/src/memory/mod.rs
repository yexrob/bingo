//! What the agent remembers: one fact per file, in two directories, with each
//! directory's index in the prompt and the bodies only when the model opens
//! one (ADR-0044).

mod command;
pub(crate) mod dir;
pub(crate) mod file;
pub(crate) mod index;
pub(crate) mod migrate;
pub(crate) mod store;
mod teach;

pub use command::MemoryCommand;

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bingo_sdk::{
    ContextContributor, ContextError, ContextPiece, ContextQuery, Placement, SystemBlock,
};

use crate::{files, root};

/// Lines an index may spend in the prompt. Past it the newest are kept and
/// the cut is said: a memory written this morning outranks one from last
/// month, and an index is read newest-last.
pub const INDEX_LINES: usize = 200;

/// After the instructions, before anything a turn adds: what the agent
/// remembers is context, not a rule.
const ORDER: i32 = -5;

/// What an empty scope says, so a directory that is not there yet is still a
/// directory the model knows to write in.
const EMPTY: &str = "(nothing remembered yet)";

/// Contributes the teaching and the two indexes, and never a body.
#[derive(Debug, Clone)]
pub struct MemoryContributor {
    data_dir: PathBuf,
}

impl MemoryContributor {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }
}

#[async_trait]
impl ContextContributor for MemoryContributor {
    fn id(&self) -> &str {
        "context:memory"
    }

    fn placement(&self) -> Placement {
        Placement::System { order: ORDER }
    }

    /// The teaching is cached and the indexes are not: the words never change,
    /// and the hook writes an index at the end of every working turn while the
    /// model may write one in the middle of it.
    async fn contribute(&self, query: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError> {
        let root = root::of(query.cwd).await;
        migrate::once(&self.data_dir, &root).await;
        Ok(vec![
            ContextPiece::System(teach::block()),
            ContextPiece::System(scope("the user", &dir::user(&self.data_dir)).await),
            ContextPiece::System(scope("this project", &dir::project(&self.data_dir, &root)).await),
        ])
    }
}

/// One scope's index, under a heading that says where its directory is: the
/// model reaches the files with the tools it already has, which need the path.
async fn scope(whose: &str, at: &Path) -> SystemBlock {
    let index = store::index_text(at).await;
    let body = if index.trim().is_empty() {
        EMPTY
    } else {
        index.as_str()
    };
    let heading = format!("# Memories about {whose} — {}", at.display());
    files::block(&heading, body, false, INDEX_LINES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::file::{Kind, Memory};
    use crate::query::Asked;

    fn contributor(data: &tempfile::TempDir) -> MemoryContributor {
        MemoryContributor::new(data.path().to_path_buf())
    }

    async fn blocks(data: &tempfile::TempDir, cwd: &Path) -> Vec<String> {
        let asked = Asked::at(cwd);
        contributor(data)
            .contribute(asked.query())
            .await
            .expect("memory never fails a turn")
            .iter()
            .map(text)
            .collect()
    }

    fn text(piece: &ContextPiece) -> String {
        match piece {
            ContextPiece::System(block) => block.text.clone(),
            ContextPiece::User { .. } => String::new(),
        }
    }

    fn a_fact(name: &str, description: &str) -> Memory {
        Memory {
            name: name.into(),
            description: description.into(),
            kind: Kind::Project,
            body: "a body no prompt ever carries\n".into(),
        }
    }

    #[test]
    fn it_speaks_after_the_instructions() {
        let contributor = MemoryContributor::new(PathBuf::from("/data"));
        assert_eq!(contributor.id(), "context:memory");
        assert_eq!(contributor.placement(), Placement::System { order: -5 });
    }

    #[tokio::test]
    async fn an_empty_memory_still_says_where_the_directories_are() {
        let data = tempfile::tempdir().expect("a data dir");
        let cwd = tempfile::tempdir().expect("a cwd");
        let blocks = blocks(&data, cwd.path()).await;
        assert_eq!(blocks.len(), 3);
        assert!(blocks[0].starts_with("# Memory\n"));
        assert!(blocks[1].starts_with("# Memories about the user — "));
        assert!(blocks[1].contains(EMPTY));
        assert!(blocks[2].starts_with("# Memories about this project — "));
        assert!(blocks[2].contains(EMPTY));
    }

    #[tokio::test]
    async fn the_prompt_carries_the_index_and_never_a_body() {
        let data = tempfile::tempdir().expect("a data dir");
        let cwd = tempfile::tempdir().expect("a cwd");
        let root = cwd.path().canonicalize().expect("a real path");
        let at = dir::project(data.path(), &root);
        store::save(&at, &a_fact("a-fact", "one line"))
            .await
            .expect("a memory");
        store::save(&dir::user(data.path()), &a_fact("a-habit", "how they work"))
            .await
            .expect("a memory");

        let blocks = blocks(&data, cwd.path()).await;
        assert!(blocks[1].contains("- [A habit](a-habit.md) — how they work"));
        assert!(blocks[2].contains("- [A fact](a-fact.md) — one line"));
        for block in &blocks {
            assert!(!block.contains("a body no prompt ever carries"), "{block}");
        }
    }

    #[tokio::test]
    async fn a_long_index_contributes_its_newest_lines_and_says_so() {
        let data = tempfile::tempdir().expect("a data dir");
        let cwd = tempfile::tempdir().expect("a cwd");
        let root = cwd.path().canonicalize().expect("a real path");
        let at = dir::project(data.path(), &root);
        let long: String = (1..=INDEX_LINES + 10)
            .map(|i| format!("- [Fact {i}](fact-{i}.md) — line {i}\n"))
            .collect();
        std::fs::create_dir_all(&at).expect("the scope");
        std::fs::write(dir::index(&at), long).expect("the index");

        let blocks = blocks(&data, cwd.path()).await;
        assert!(blocks[2].contains("[… 10 earlier lines not shown]"));
        assert!(blocks[2].contains("fact-11.md") && blocks[2].contains("fact-210.md"));
        assert!(!blocks[2].contains("fact-10.md"));
    }

    #[tokio::test]
    async fn the_old_single_file_is_migrated_before_it_is_read() {
        let data = tempfile::tempdir().expect("a data dir");
        let cwd = tempfile::tempdir().expect("a cwd");
        let root = cwd.path().canonicalize().expect("a real path");
        let old = dir::legacy(data.path(), &root);
        std::fs::create_dir_all(old.parent().expect("a parent")).expect("the memory dir");
        std::fs::write(&old, "the tests run with cargo test\n").expect("the old file");

        let blocks = blocks(&data, cwd.path()).await;
        assert!(blocks[2].contains("imported.md"), "{}", blocks[2]);
        assert!(!old.exists());
    }
}
