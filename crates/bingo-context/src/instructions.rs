//! The instruction files a project leaves for whoever works in it.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bingo_sdk::{
    ContextContributor, ContextError, ContextPiece, ContextQuery, Placement, SystemBlock,
};

use crate::{baseline, files, root};

/// The file a directory speaks through, in order of preference: a project that
/// has written for this agent is not also asked what it told another one.
const NAMES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// Before everything else in the system prompt: instructions are the frame the
/// rest of the prompt is read in.
const ORDER: i32 = -10;

pub(crate) const ID: &str = "context:instructions";

/// Contributes the user's instructions and every project file from the root
/// down to the working directory.
#[derive(Debug, Clone)]
pub struct InstructionsContributor {
    config_dir: PathBuf,
}

impl InstructionsContributor {
    pub fn new(config_dir: PathBuf) -> Self {
        Self { config_dir }
    }

    /// The user's file first, then one file per directory from the root down:
    /// the nearer a file is to the work, the later it speaks.
    async fn paths(&self, cwd: &Path) -> Vec<PathBuf> {
        let root = root::of(cwd).await;
        let mut paths = vec![self.config_dir.join(NAMES[0])];
        for dir in root::chain(&root, cwd) {
            paths.extend(present(&dir).await);
        }
        paths
    }
}

/// The one file this directory speaks through, if it has one.
async fn present(dir: &Path) -> Option<PathBuf> {
    for name in NAMES {
        let path = dir.join(name);
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Some(path);
        }
    }
    None
}

/// A file that cannot be read is a file that is not there: an unreadable
/// AGENTS.md must not cost the turn.
async fn block(path: &Path) -> Option<SystemBlock> {
    let text = tokio::fs::read_to_string(path).await.ok()?;
    if text.trim().is_empty() {
        return None;
    }
    let heading = format!("# Instructions from {}", path.display());
    Some(files::block(&heading, &text, true, files::MAX_LINES))
}

#[async_trait]
impl ContextContributor for InstructionsContributor {
    fn id(&self) -> &str {
        ID
    }

    fn placement(&self) -> Placement {
        Placement::System { order: ORDER }
    }

    async fn contribute(&self, query: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError> {
        baseline::contribute(self.id(), query, async {
            let mut blocks = Vec::new();
            for path in self.paths(query.cwd).await {
                blocks.extend(block(&path).await);
            }
            blocks
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseline::testing::Journal;
    use crate::git::{self, Repo};

    fn headings(pieces: &[ContextPiece]) -> Vec<String> {
        pieces
            .iter()
            .filter_map(|p| match p {
                ContextPiece::System(block) => block.text.lines().next().map(str::to_string),
                _ => None,
            })
            .collect()
    }

    async fn contribute(config_dir: &Path, cwd: &Path) -> Vec<ContextPiece> {
        Journal::at(cwd)
            .contribute(&InstructionsContributor::new(config_dir.to_path_buf()), cwd)
            .await
    }

    #[test]
    fn it_is_the_first_thing_in_the_system_prompt() {
        let contributor = InstructionsContributor::new(PathBuf::from("/config"));
        assert_eq!(contributor.id(), "context:instructions");
        assert_eq!(contributor.placement(), Placement::System { order: -10 });
    }

    #[tokio::test]
    async fn every_directory_from_the_root_down_speaks_in_order() {
        let Some(repo) = Repo::init() else {
            return git::absent();
        };
        let config = tempfile::tempdir().expect("a config dir");
        std::fs::write(config.path().join("AGENTS.md"), "be brief\n").expect("the user file");
        repo.write("AGENTS.md", "the project is a kernel\n");
        repo.write("crates/inner/AGENTS.md", "this crate is the inner one\n");
        let cwd = repo.dir("crates/inner");

        let pieces = contribute(config.path(), &cwd).await;
        assert_eq!(
            headings(&pieces),
            [
                format!(
                    "# Instructions from {}",
                    config.path().join("AGENTS.md").display()
                ),
                format!(
                    "# Instructions from {}",
                    repo.root().join("AGENTS.md").display()
                ),
                format!("# Instructions from {}", cwd.join("AGENTS.md").display()),
            ]
        );
    }

    #[tokio::test]
    async fn a_directory_with_both_files_speaks_through_agents_md() {
        let Some(repo) = Repo::init() else {
            return git::absent();
        };
        let config = tempfile::tempdir().expect("a config dir");
        repo.write("AGENTS.md", "the agents file\n");
        repo.write("CLAUDE.md", "the claude file\n");

        let pieces = contribute(config.path(), &repo.root()).await;
        assert_eq!(pieces.len(), 1);
        assert!(text(&pieces[0]).contains("the agents file"));
    }

    #[tokio::test]
    async fn a_directory_with_only_claude_md_still_speaks() {
        let Some(repo) = Repo::init() else {
            return git::absent();
        };
        let config = tempfile::tempdir().expect("a config dir");
        repo.write("CLAUDE.md", "the claude file\n");

        let pieces = contribute(config.path(), &repo.root()).await;
        assert_eq!(pieces.len(), 1);
        assert!(text(&pieces[0]).contains("the claude file"));
    }

    #[tokio::test]
    async fn a_file_that_is_not_there_contributes_nothing() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let config = tempfile::tempdir().expect("a config dir");
        let pieces = contribute(config.path(), dir.path()).await;
        assert!(pieces.is_empty());
    }

    #[tokio::test]
    async fn a_long_file_contributes_its_newest_lines_and_says_so() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let config = tempfile::tempdir().expect("a config dir");
        let long: String = (1..=400).map(|i| format!("rule {i}\n")).collect();
        std::fs::write(dir.path().join("AGENTS.md"), long).expect("the file");

        let pieces = contribute(config.path(), dir.path()).await;
        let text = text(&pieces[0]);
        assert!(text.contains("[… 100 earlier lines not shown]"));
        assert!(text.contains("rule 101") && text.contains("rule 400"));
        assert!(!text.contains("rule 100\n"));
    }

    fn text(piece: &ContextPiece) -> String {
        match piece {
            ContextPiece::System(block) => block.text.clone(),
            ContextPiece::User { .. } => String::new(),
        }
    }
}
