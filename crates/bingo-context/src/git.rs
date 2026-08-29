//! A real repository for the tests that ask a real `git` where a project
//! starts. Every step is fallible: a machine without `git` skips instead of
//! failing.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

pub struct Repo {
    dir: TempDir,
}

impl Repo {
    /// A checkout with one commit, or `None` when `git` cannot be run.
    pub fn init() -> Option<Self> {
        let dir = tempfile::tempdir().ok()?;
        let root = dir.path().join("main");
        std::fs::create_dir_all(&root).ok()?;
        run(&root, &["init", "--quiet"])?;
        run(&root, &["config", "user.email", "test@bingo"])?;
        run(&root, &["config", "user.name", "test"])?;
        std::fs::write(root.join("seed"), "seed\n").ok()?;
        run(&root, &["add", "-A"])?;
        run(&root, &["commit", "--quiet", "-m", "seed"])?;
        Some(Self { dir })
    }

    pub fn root(&self) -> PathBuf {
        canonical(&self.dir.path().join("main"))
    }

    /// A directory inside the checkout, created if it is not there yet.
    pub fn dir(&self, relative: &str) -> PathBuf {
        let path = self.root().join(relative);
        let _ = std::fs::create_dir_all(&path);
        canonical(&path)
    }

    pub fn write(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.root().join(relative);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, text);
        path
    }

    /// A linked worktree beside the checkout, so it is not a directory under it.
    pub fn worktree(&self, name: &str) -> Option<PathBuf> {
        let path = self.dir.path().join(name);
        let at = path.to_str()?;
        run(
            &self.root(),
            &["worktree", "add", "--quiet", at, "-b", name],
        )?;
        Some(canonical(&path))
    }
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn run(cwd: &Path, args: &[&str]) -> Option<()> {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    status.success().then_some(())
}

/// What a test does when there is no `git` to ask.
pub fn absent() {
    eprintln!("skipped: git is not available");
}
