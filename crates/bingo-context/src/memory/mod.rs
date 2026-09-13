//! What the agent remembers: one fact per file, in two directories, with each
//! directory's index in the prompt and the bodies only when the model opens
//! one (ADR-0044). The model is the one writer (ADR-0049).

mod command;
pub(crate) mod dir;
pub(crate) mod file;
pub(crate) mod store;
mod teach;

pub use command::MemoryCommand;

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bingo_sdk::{
    ContextContributor, ContextError, ContextPiece, ContextQuery, HostHandle, Placement, SessionId,
    SystemBlock,
};

use crate::{baseline, files, root};

/// Lines an index may spend in the prompt. Past it the newest are kept and
/// the cut is said: a memory written this morning outranks one from last
/// month, and an index is read newest-last. Sixty is a hint; two hundred was
/// a document, and the lines that do not fit are the lines to merge.
pub const INDEX_LINES: usize = 60;

/// After the instructions, before anything a turn adds: what the agent
/// remembers is context, not a rule.
const ORDER: i32 = -5;

pub(crate) const ID: &str = "context:memory";

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
        ID
    }

    fn placement(&self) -> Placement {
        Placement::System { order: ORDER }
    }

    async fn contribute(&self, query: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError> {
        baseline::contribute(self.id(), query, async {
            vec![
                teach::block(),
                scope("the user", &dir::user(&self.data_dir)).await,
                scope(
                    "this project",
                    &project_dir(&self.data_dir, query.cwd).await,
                )
                .await,
            ]
        })
        .await
    }
}

/// The kind the two directories are published under, beside the baselines:
/// `{ "user": <dir>, "project": <dir> }`, the same paths the headings carry
/// for the model, as data for a surface that draws a call on a memory file as
/// what it is (M84). A session's directories are fixed by its cwd, so they are
/// written once, when the session starts; a surface that cannot read them
/// draws the call as any other.
pub(crate) const DIRECTORIES: &str = "memory";

pub(crate) async fn publish(host: &HostHandle, session: &SessionId, data_dir: &Path, cwd: &Path) {
    let payload = serde_json::json!({
        "user": dir::user(data_dir).display().to_string(),
        "project": project_dir(data_dir, cwd).await.display().to_string(),
    });
    if let Err(error) = host
        .extend(session, baseline::PLUGIN, DIRECTORIES, payload)
        .await
    {
        tracing::warn!(%error, "memory: the directories were not published");
    }
}

/// Where this project's memories are: the root the directory belongs to and
/// the commit its repository began with, asked once here so the contributor
/// and the command answer the same directory.
pub(crate) async fn project_dir(data_dir: &Path, cwd: &Path) -> PathBuf {
    let root = root::of(cwd).await;
    let commit = root::commit(&root).await;
    dir::project(data_dir, &root, commit.as_deref())
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
    use crate::baseline::testing::Journal;
    use crate::memory::file::{Kind, Memory};

    fn contributor(data: &tempfile::TempDir) -> MemoryContributor {
        MemoryContributor::new(data.path().to_path_buf())
    }

    async fn blocks(data: &tempfile::TempDir, cwd: &Path) -> Vec<String> {
        Journal::at(cwd)
            .contribute(&contributor(data), cwd)
            .await
            .iter()
            .map(text)
            .collect()
    }

    fn text(piece: &ContextPiece) -> String {
        match piece {
            ContextPiece::System(block) => block.text.clone(),
            ContextPiece::User { .. } => panic!("memory is system context, not a user item"),
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

    #[tokio::test]
    async fn indexes_are_retained_until_the_history_generation_changes() {
        let data = tempfile::tempdir().expect("a data dir");
        let cwd = tempfile::tempdir().expect("a cwd");
        let journal = Journal::at(cwd.path());
        let contributor = contributor(&data);
        let first = journal.contribute(&contributor, cwd.path()).await;
        assert_eq!(first.len(), 3);
        let user = dir::user(data.path());
        store::save(&user, &a_fact("a-habit", "new preference"))
            .await
            .expect("a memory");
        assert_eq!(journal.contribute(&contributor, cwd.path()).await, first);

        journal.state().history_generation += 1;
        let changed = journal.contribute(&contributor, cwd.path()).await;
        assert_eq!(text(&changed[0]), text(&first[0]));
        assert!(text(&changed[1]).contains("new preference"));
        std::fs::write(dir::index(&user), "").expect("clear index");
        assert_eq!(journal.contribute(&contributor, cwd.path()).await, changed);

        journal.state().history_generation += 1;
        let cleared = journal.contribute(&contributor, cwd.path()).await;
        assert_eq!(cleared, first);
        assert_eq!(journal.state().seq.0, 3, "one capture per generation");
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
        let at = project_dir(data.path(), cwd.path()).await;
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
        let at = project_dir(data.path(), cwd.path()).await;
        let long: String = (1..=INDEX_LINES + 10)
            .map(|i| format!("- [Fact {i}](fact-{i}.md) — line {i}\n"))
            .collect();
        std::fs::create_dir_all(&at).expect("the scope");
        std::fs::write(dir::index(&at), long).expect("the index");

        let blocks = blocks(&data, cwd.path()).await;
        assert!(blocks[2].contains("[… 10 earlier lines not shown]"));
        let oldest_kept = format!("fact-{}.md", 11);
        let newest = format!("fact-{}.md", INDEX_LINES + 10);
        assert!(blocks[2].contains(&oldest_kept) && blocks[2].contains(&newest));
        assert!(!blocks[2].contains("fact-10.md"));
    }
}
