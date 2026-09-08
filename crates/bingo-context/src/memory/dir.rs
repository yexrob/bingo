//! Where memories live: two directories under the data directory, one for the
//! person and one per project.

use std::path::{Path, PathBuf};

const ROOT: &str = "memory";

/// The index every scope keeps. Upper case because it is the first file a
/// person opening the directory should see.
const INDEX: &str = "MEMORY.md";

/// The scope that follows the person from project to project.
const USER: &str = "user";

/// What is true of the person wherever they are working.
pub fn user(data_dir: &Path) -> PathBuf {
    data_dir.join(ROOT).join(USER)
}

/// What this project taught the agent.
pub fn project(data_dir: &Path, root: &Path, commit: Option<&str>) -> PathBuf {
    data_dir.join(ROOT).join(key(root, commit))
}

pub fn index(dir: &Path) -> PathBuf {
    dir.join(INDEX)
}

#[cfg(test)]
pub fn file(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.md"))
}

/// How many characters of a commit id name it: what `git` shows, doubled.
const COMMIT_CHARS: usize = 16;

/// This project's directory name: a readable name and what the project is —
/// the commit its repository began with, so a checkout deleted and begun
/// again is a new project and a worktree is the same one; outside git, a
/// digest of the root's full path, because two directories both called
/// `web` are two projects.
pub fn key(root: &Path, commit: Option<&str>) -> String {
    let what = match commit {
        Some(commit) => commit.chars().take(COMMIT_CHARS).collect(),
        None => digest(root),
    };
    format!("{}-{what}", name(root))
}

fn name(root: &Path) -> String {
    match root.file_name() {
        Some(name) => name.to_string_lossy().chars().map(keepable).collect(),
        None => "root".to_string(),
    }
}

fn keepable(c: char) -> char {
    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
        c
    } else {
        '_'
    }
}

/// FNV-1a 64 over the path's bytes. A hasher from the standard library is
/// seeded per process, and a key that changed between runs would give one
/// project a new memory every morning.
fn digest(path: &Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEGAN: &str = "2bf6c26a7362cd1f9e0d4a5b6c7d8e9f00112233";

    #[test]
    fn a_repository_is_keyed_by_the_commit_it_began_with() {
        let root = Path::new("/work/alpha/web");
        assert_eq!(key(root, Some(BEGAN)), "web-2bf6c26a7362cd1f");
        assert_eq!(
            key(Path::new("/work/beta/web"), Some(BEGAN)),
            key(root, Some(BEGAN)),
            "a second clone is the same project"
        );
        assert_ne!(key(root, Some(BEGAN)), key(root, Some("ffff")));
    }

    #[test]
    fn a_directory_outside_git_is_keyed_by_its_path() {
        let root = Path::new("/work/alpha/web");
        assert_eq!(key(root, None), key(root, None));
        assert_ne!(key(root, None), key(Path::new("/work/beta/web"), None));
        assert!(key(root, None).starts_with("web-"), "{}", key(root, None));
        assert_eq!(key(root, None).len(), key(root, Some(BEGAN)).len());
    }

    #[test]
    fn a_name_keeps_only_what_a_file_name_may_hold() {
        assert_eq!(
            &key(Path::new("/work/my project.v2"), None)[.."my_project_v2".len()],
            "my_project_v2"
        );
    }

    #[test]
    fn the_two_scopes_are_two_directories_under_the_data_directory() {
        let data = Path::new("/data");
        let root = Path::new("/work/web");
        assert_eq!(user(data), Path::new("/data/memory/user"));
        assert_eq!(
            project(data, root, None),
            Path::new("/data/memory").join(key(root, None))
        );
        assert_ne!(user(data), project(data, root, None));
    }

    #[test]
    fn a_scope_holds_its_index_and_one_file_per_memory() {
        let dir = project(Path::new("/data"), Path::new("/work/web"), None);
        assert_eq!(index(&dir), dir.join("MEMORY.md"));
        assert_eq!(file(&dir, "a-fact"), dir.join("a-fact.md"));
    }
}
