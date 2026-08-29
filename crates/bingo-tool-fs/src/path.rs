//! How the filesystem tools see the tree: one path resolution, one traversal
//! policy, one glob dialect.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobMatcher};
use ignore::WalkBuilder;

/// An absolute path stands; a relative one hangs off the session's working
/// directory.
pub(crate) fn resolve(file_path: &str, cwd: &Path) -> PathBuf {
    let path = Path::new(file_path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// The walk `Glob` and `Grep` share: `.gitignore` obeyed whether or not the
/// root is a git checkout, hidden entries left out, symlinks not followed, and
/// a stable order so a truncated result is the same result twice.
pub(crate) fn walker(root: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_path(Path::cmp);
    builder
}

/// One glob dialect for both tools. `*` crosses directory boundaries, so
/// `*.rs` finds a file at any depth while `src/**/*.rs` anchors it.
pub(crate) fn matcher(pattern: &str) -> Result<GlobMatcher, globset::Error> {
    Ok(Glob::new(pattern)?.compile_matcher())
}

/// A pattern matches the path relative to the search root; a path outside the
/// root is matched whole.
pub(crate) fn matches(matcher: &GlobMatcher, root: &Path, path: &Path) -> bool {
    match path.strip_prefix(root) {
        Ok(relative) => matcher.is_match(relative),
        Err(_) => matcher.is_match(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_path_resolves_against_the_working_directory() {
        let cwd = Path::new("/work");
        assert_eq!(
            resolve("src/lib.rs", cwd),
            PathBuf::from("/work/src/lib.rs")
        );
        assert_eq!(resolve("/etc/hosts", cwd), PathBuf::from("/etc/hosts"));
    }

    #[test]
    fn a_pattern_without_a_slash_matches_at_any_depth() {
        let m = matcher("*.rs").expect("a valid pattern");
        let root = Path::new("/work");
        assert!(matches(&m, root, Path::new("/work/a.rs")));
        assert!(matches(&m, root, Path::new("/work/src/deep/a.rs")));
        assert!(!matches(&m, root, Path::new("/work/a.toml")));
    }

    #[test]
    fn a_pattern_with_a_slash_anchors_at_the_root() {
        let m = matcher("src/**/*.rs").expect("a valid pattern");
        let root = Path::new("/work");
        assert!(matches(&m, root, Path::new("/work/src/deep/a.rs")));
        assert!(!matches(&m, root, Path::new("/work/tests/a.rs")));
    }

    #[test]
    fn a_broken_pattern_is_an_error_not_a_panic() {
        assert!(matcher("[").is_err());
    }
}
