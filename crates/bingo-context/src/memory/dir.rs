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
pub fn project(data_dir: &Path, root: &Path) -> PathBuf {
    data_dir.join(ROOT).join(key(root))
}

/// The one file a project's memory used to be, before it became a directory
/// (ADR-0006 §7, amended by ADR-0044).
pub fn legacy(data_dir: &Path, root: &Path) -> PathBuf {
    data_dir.join(ROOT).join(format!("{}.md", key(root)))
}

pub fn index(dir: &Path) -> PathBuf {
    dir.join(INDEX)
}

pub fn file(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.md"))
}

/// This project's directory name: a readable name and a digest of the root's
/// full path, because two checkouts both called `web` are two projects.
pub fn key(root: &Path) -> String {
    format!("{}-{}", name(root), digest(root))
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

    #[test]
    fn a_key_is_stable_and_belongs_to_one_root() {
        let root = Path::new("/work/alpha/web");
        assert_eq!(key(root), key(root));
        assert_ne!(key(root), key(Path::new("/work/beta/web")));
        assert!(key(root).starts_with("web-"), "{}", key(root));
        assert_eq!(key(root).len(), "web-".len() + 16);
    }

    #[test]
    fn a_name_keeps_only_what_a_file_name_may_hold() {
        assert_eq!(
            &key(Path::new("/work/my project.v2"))[.."my_project_v2".len()],
            "my_project_v2"
        );
    }

    #[test]
    fn the_two_scopes_are_two_directories_under_the_data_directory() {
        let data = Path::new("/data");
        let root = Path::new("/work/web");
        assert_eq!(user(data), Path::new("/data/memory/user"));
        assert_eq!(
            project(data, root),
            Path::new("/data/memory").join(key(root))
        );
        assert_ne!(user(data), project(data, root));
    }

    #[test]
    fn a_scope_holds_its_index_and_one_file_per_memory() {
        let dir = project(Path::new("/data"), Path::new("/work/web"));
        assert_eq!(index(&dir), dir.join("MEMORY.md"));
        assert_eq!(file(&dir, "a-fact"), dir.join("a-fact.md"));
    }

    #[test]
    fn the_old_file_sits_beside_the_directory_that_replaced_it() {
        let data = Path::new("/data");
        let root = Path::new("/work/web");
        assert_eq!(legacy(data, root), project(data, root).with_extension("md"),);
    }
}
