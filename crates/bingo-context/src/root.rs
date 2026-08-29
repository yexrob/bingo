//! Which project a directory belongs to.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

/// The project root: the directory holding the repository's common git
/// directory, so every worktree of one checkout answers with the checkout it
/// was made from and they share one memory. A directory in no repository is
/// its own project.
/// The answer is always canonical, so one project spelled two ways — through
/// a symlink, through `..` — is still one project and one memory.
pub async fn of(cwd: &Path) -> PathBuf {
    match common_dir(cwd).await.and_then(|dir| parent(cwd, &dir)) {
        Some(root) => root,
        None => canonical(cwd),
    }
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

async fn common_dir(cwd: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    let path = path.trim();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// `--git-common-dir` answers relative to `cwd` in an ordinary checkout
/// (`.git`, `../../.git`) and absolutely from a linked worktree — which is
/// exactly what makes a worktree resolve to its main checkout.
fn parent(cwd: &Path, common: &Path) -> Option<PathBuf> {
    let dir = if common.is_absolute() {
        common.to_path_buf()
    } else {
        cwd.join(common)
    };
    dir.canonicalize().ok()?.parent().map(Path::to_path_buf)
}

/// Every directory from the project root down to `cwd`, oldest first. A `cwd`
/// outside its own root — a worktree checked out elsewhere on disk — has only
/// itself, because there is no path down to it to walk.
pub fn chain(root: &Path, cwd: &Path) -> Vec<PathBuf> {
    let cwd = canonical(cwd);
    let Ok(rest) = cwd.strip_prefix(root) else {
        return vec![cwd];
    };
    let mut dirs = vec![root.to_path_buf()];
    let mut at = root.to_path_buf();
    for part in rest {
        at = at.join(part);
        dirs.push(at.clone());
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{self, Repo};

    #[tokio::test]
    async fn a_directory_in_no_repository_is_its_own_root() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let cwd = dir.path().canonicalize().expect("a real path");
        assert_eq!(of(&cwd).await, cwd);
    }

    #[tokio::test]
    async fn a_subdirectory_answers_with_the_checkout_above_it() {
        let Some(repo) = Repo::init() else {
            return git::absent();
        };
        let deep = repo.dir("src/inner");
        assert_eq!(of(&deep).await, repo.root());
    }

    #[tokio::test]
    async fn a_worktree_answers_with_the_checkout_it_was_made_from() {
        let Some(repo) = Repo::init() else {
            return git::absent();
        };
        let Some(worktree) = repo.worktree("side") else {
            return git::absent();
        };
        assert_ne!(worktree, repo.root(), "the worktree is somewhere else");
        assert_eq!(of(&worktree).await, repo.root());
    }

    #[test]
    fn the_chain_runs_from_the_root_down_to_the_directory() {
        let root = Path::new("/work/project");
        assert_eq!(
            chain(root, Path::new("/work/project/a/b")),
            [
                PathBuf::from("/work/project"),
                PathBuf::from("/work/project/a"),
                PathBuf::from("/work/project/a/b"),
            ]
        );
        assert_eq!(chain(root, root), [PathBuf::from("/work/project")]);
    }

    #[test]
    fn a_directory_outside_the_root_walks_nothing() {
        let cwd = Path::new("/elsewhere/worktree");
        assert_eq!(chain(Path::new("/work/project"), cwd), [cwd.to_path_buf()]);
    }
}
