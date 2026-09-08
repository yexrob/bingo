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

/// The commit a repository began with, which is what the repository *is*: a
/// checkout deleted and begun again on the same path is another project, and
/// a worktree or a second clone is the same one. A history with two roots
/// answers the lowest, so it answers one thing. `None` outside git and
/// before the first commit.
pub async fn commit(root: &Path) -> Option<String> {
    let roots = git(root, &["rev-list", "--max-parents=0", "HEAD"]).await?;
    roots
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .min()
        .map(str::to_string)
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

async fn common_dir(cwd: &Path) -> Option<PathBuf> {
    let path = git(cwd, &["rev-parse", "--git-common-dir"]).await?;
    let path = path.trim();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// What `git` says from `cwd`, or nothing: no git, no repository, no answer.
async fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
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

    #[tokio::test]
    async fn a_worktree_and_its_checkout_began_with_the_same_commit() {
        let Some(repo) = Repo::init() else {
            return git::absent();
        };
        let Some(worktree) = repo.worktree("side") else {
            return git::absent();
        };
        let began = commit(&repo.root()).await.expect("a root commit");
        assert_eq!(began, repo.root_commit().expect("git answers"));
        assert_eq!(commit(&worktree).await.as_ref(), Some(&began));
    }

    #[tokio::test]
    async fn two_repositories_began_differently_and_an_empty_one_has_not_begun() {
        let (Some(one), Some(two)) = (Repo::init(), Repo::begun_with("another\n")) else {
            return git::absent();
        };
        assert_ne!(commit(&one.root()).await, commit(&two.root()).await);

        let Some(empty) = Repo::empty() else {
            return git::absent();
        };
        assert_eq!(commit(&empty.root()).await, None);
        let dir = tempfile::tempdir().expect("a temp dir");
        assert_eq!(commit(dir.path()).await, None, "no repository");
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
