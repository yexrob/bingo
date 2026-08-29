//! The project memory file: where it lives, what goes in it, and how it
//! reaches the prompt.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bingo_sdk::{ContextContributor, ContextError, ContextPiece, ContextQuery, Placement};

use crate::{files, root};

/// Lines a memory keeps. Past this the oldest go: a fact learned this morning
/// outranks one learned last month, and the file is read newest-last.
pub const MAX_LINES: usize = 300;

const DIR: &str = "memory";

/// After the instructions, before anything a turn adds: what this project
/// taught the agent is context, not a rule.
const ORDER: i32 = -5;

/// This project's file name: a readable directory name and a digest of the
/// root's full path, because two checkouts both called `web` are two projects.
pub fn key(root: &Path) -> String {
    format!("{}-{}", name(root), digest(root))
}

pub fn path(data_dir: &Path, root: &Path) -> PathBuf {
    data_dir.join(DIR).join(format!("{}.md", key(root)))
}

fn name(root: &Path) -> String {
    match root.file_name() {
        Some(name) => name
            .to_string_lossy()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect(),
        None => "root".to_string(),
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

/// The file plus the facts it does not already hold, oldest lines evicted past
/// the cap. `None` when nothing said was new, so a turn that learned nothing
/// does not rewrite the file.
pub fn merged(existing: &str, facts: &str) -> Option<String> {
    let mut lines: Vec<&str> = existing.lines().map(str::trim).filter(not_empty).collect();
    let mut added = 0;
    for fact in facts.lines().map(str::trim).filter(not_empty) {
        if lines.contains(&fact) {
            continue;
        }
        lines.push(fact);
        added += 1;
    }
    if added == 0 {
        return None;
    }
    let first = lines.len().saturating_sub(MAX_LINES);
    Some(lines[first..].join("\n") + "\n")
}

fn not_empty(line: &&str) -> bool {
    !line.is_empty()
}

/// Contributes what this project taught the agent.
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

    /// Never cached: the hook appends to this file at the end of every working
    /// turn, so a cached copy would be stale within the session that wrote it.
    async fn contribute(&self, query: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError> {
        let root = root::of(query.cwd).await;
        let path = path(&self.data_dir, &root);
        let Ok(text) = tokio::fs::read_to_string(&path).await else {
            return Ok(Vec::new());
        };
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![ContextPiece::System(files::block(
            "# Project memory",
            &text,
            false,
        ))])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::Asked;

    fn facts(n: usize) -> String {
        (1..=n).map(|i| format!("fact {i}\n")).collect()
    }

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
    fn the_file_sits_under_the_data_directory() {
        let path = path(Path::new("/data"), Path::new("/work/web"));
        assert!(path.starts_with("/data/memory"));
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("md"));
    }

    #[test]
    fn new_facts_are_appended_and_repeats_are_not() {
        let next = merged("fact 1\n", "fact 1\nfact 2\n").expect("one new fact");
        assert_eq!(next, "fact 1\nfact 2\n");
        assert_eq!(merged("fact 1\n", "  fact 1  \n"), None);
        assert_eq!(merged("fact 1\n", "\n\n"), None);
    }

    #[test]
    fn the_oldest_lines_go_when_the_file_is_full() {
        let next = merged(&facts(300), "fact 301\n").expect("one new fact");
        assert_eq!(next.lines().count(), MAX_LINES);
        assert_eq!(next.lines().next(), Some("fact 2"));
        assert_eq!(next.lines().last(), Some("fact 301"));
    }

    #[tokio::test]
    async fn an_absent_memory_contributes_nothing() {
        let data = tempfile::tempdir().expect("a data dir");
        let cwd = tempfile::tempdir().expect("a cwd");
        let asked = Asked::at(cwd.path());
        let pieces = MemoryContributor::new(data.path().to_path_buf())
            .contribute(asked.query())
            .await
            .expect("memory never fails a turn");
        assert!(pieces.is_empty());
    }

    #[tokio::test]
    async fn a_memory_reaches_the_prompt_uncached() {
        let data = tempfile::tempdir().expect("a data dir");
        let cwd = tempfile::tempdir().expect("a cwd");
        let root = cwd.path().canonicalize().expect("a real path");
        let path = path(data.path(), &root);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("the memory dir");
        std::fs::write(&path, "the build runs cargo test\n").expect("the memory");

        let asked = Asked::at(cwd.path());
        let pieces = MemoryContributor::new(data.path().to_path_buf())
            .contribute(asked.query())
            .await
            .expect("memory never fails a turn");
        let ContextPiece::System(block) = &pieces[0] else {
            panic!("a system block");
        };
        assert_eq!(
            block.text,
            "# Project memory\n\nthe build runs cargo test\n"
        );
        assert!(!block.cache);
    }
}
