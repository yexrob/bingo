//! The one file a project's memory used to be, moved into the directory it is
//! now (ADR-0006 §7, amended by ADR-0044).

use std::path::Path;

use crate::memory::dir;
use crate::memory::file::{Kind, Memory};
use crate::memory::store;

/// The name the old file takes inside the directory that replaced it.
const NAME: &str = "imported";

/// The old file was a heap of lines nobody named, so the only honest
/// description of it is where it came from.
const DESCRIPTION: &str = "what the old memory file held";

/// Once, the first time anything looks at a project whose directory is not
/// there yet: a directory that exists has been through this, and a project
/// that never had a file has nothing to do.
///
/// Never fails a turn. A file that could not be moved is reported and left
/// exactly where it is, so the next session tries again.
pub async fn once(data_dir: &Path, root: &Path) {
    let to = dir::project(data_dir, root);
    if tokio::fs::try_exists(&to).await.unwrap_or(true) {
        return;
    }
    let from = dir::legacy(data_dir, root);
    let Ok(text) = tokio::fs::read_to_string(&from).await else {
        return;
    };
    if !text.trim().is_empty()
        && let Err(error) = store::save(&to, &imported(text)).await
    {
        tracing::warn!(%error, path = %from.display(), "memory: the old file stays where it is");
        return;
    }
    if let Err(error) = tokio::fs::remove_file(&from).await {
        tracing::warn!(%error, path = %from.display(), "memory: the old file is still there");
    }
}

fn imported(text: String) -> Memory {
    Memory {
        name: NAME.to_string(),
        description: DESCRIPTION.to_string(),
        kind: Kind::Project,
        body: text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project memory in the shape the old file had: bare lines, no names.
    const OLD: &str = "\
the tests run with cargo test
the kernel never imports a plugin
";

    struct Project {
        data: tempfile::TempDir,
        root: std::path::PathBuf,
    }

    impl Project {
        fn new() -> Self {
            Self {
                data: tempfile::tempdir().expect("a data dir"),
                root: std::path::PathBuf::from("/work/web"),
            }
        }

        fn data_dir(&self) -> &Path {
            self.data.path()
        }

        fn dir(&self) -> std::path::PathBuf {
            dir::project(self.data_dir(), &self.root)
        }

        fn old(&self) -> std::path::PathBuf {
            dir::legacy(self.data_dir(), &self.root)
        }

        fn write_old(&self, text: &str) {
            let path = self.old();
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("the memory dir");
            std::fs::write(path, text).expect("the old file");
        }

        async fn migrate(&self) {
            once(self.data_dir(), &self.root).await;
        }
    }

    #[tokio::test]
    async fn the_old_file_becomes_one_memory_and_is_gone() {
        let project = Project::new();
        project.write_old(OLD);
        project.migrate().await;

        let memories = store::list(&project.dir()).await;
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].name, NAME);
        assert_eq!(memories[0].description, DESCRIPTION);
        assert_eq!(memories[0].kind, Kind::Project);
        assert_eq!(memories[0].body, OLD);
        assert!(
            store::index_text(&project.dir())
                .await
                .contains("imported.md"),
        );
        assert!(!project.old().exists(), "the old file is gone");
    }

    #[tokio::test]
    async fn it_happens_once_and_never_again() {
        let project = Project::new();
        project.write_old(OLD);
        project.migrate().await;
        std::fs::write(dir::file(&project.dir(), "imported"), "edited by hand").expect("an edit");

        project.write_old("a file somebody put back");
        project.migrate().await;
        assert_eq!(
            std::fs::read_to_string(dir::file(&project.dir(), "imported")).expect("the memory"),
            "edited by hand",
            "a directory that exists has been through this",
        );
    }

    #[tokio::test]
    async fn a_project_that_never_had_a_file_gets_no_directory() {
        let project = Project::new();
        project.migrate().await;
        assert!(!project.dir().exists());
    }

    #[tokio::test]
    async fn an_empty_old_file_is_removed_and_remembers_nothing() {
        let project = Project::new();
        project.write_old("  \n\n");
        project.migrate().await;
        assert!(!project.old().exists());
        assert!(store::list(&project.dir()).await.is_empty());
    }
}
