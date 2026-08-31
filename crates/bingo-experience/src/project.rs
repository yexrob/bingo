//! Which project a directory belongs to (ADR-0014 §3). The key is the git
//! remote when there is one, so the store survives a move and follows a
//! checkout to another machine; else the repository root, else the directory
//! itself. Whatever it is, it is sanitized to exactly one directory name.
//!
//! The two `git` processes this costs are why the caller caches it per cwd.

use std::path::Path;
use std::process::Command;

/// This directory's project key.
pub fn key(cwd: &Path) -> String {
    if let Some(remote) = git(cwd, &["config", "--get", "remote.origin.url"]) {
        let normalized = normalize_remote(&remote);
        if !normalized.is_empty() {
            return sanitize(&normalized);
        }
    }
    if let Some(root) = git(cwd, &["rev-parse", "--show-toplevel"]) {
        return sanitize(&root);
    }
    sanitize(&cwd.to_string_lossy())
}

/// One line of `git` output, or nothing at all: no repository, no git, no key.
fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// Scheme, user and `.git` suffix are noise: `https://github.com/o/r.git`,
/// `git@github.com:o/r` and `ssh://github.com/o/r/` are one project.
fn normalize_remote(url: &str) -> String {
    let lower = url.trim().to_lowercase();
    let scp = lower.starts_with("git@");
    let mut rest: &str = &lower;
    for prefix in ["https://", "http://", "ssh://", "git://", "git@"] {
        if let Some(stripped) = rest.strip_prefix(prefix) {
            rest = stripped;
        }
    }
    let owned = match scp.then(|| rest.split_once(':')).flatten() {
        Some((host, path)) => format!("{host}/{path}"),
        None => rest.to_string(),
    };
    owned
        .strip_suffix(".git")
        .unwrap_or(&owned)
        .trim_end_matches('/')
        .to_string()
}

/// One directory level: anything that is not a letter, a digit, `-`, `_` or
/// `.` becomes `-`, separators included.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_of_one_remote_is_one_key() {
        let wanted = "github.com/owner/repo";
        for url in [
            "https://github.com/owner/repo.git",
            "https://GitHub.com/Owner/Repo",
            "git@github.com:owner/repo.git",
            "ssh://github.com/owner/repo/",
            "git://github.com/owner/repo.git",
        ] {
            assert_eq!(normalize_remote(url), wanted, "{url}");
        }
        assert_eq!(normalize_remote("   "), "");
    }

    #[test]
    fn a_key_is_one_directory_name() {
        assert_eq!(sanitize("github.com/owner/repo"), "github.com-owner-repo");
        assert_eq!(sanitize("/Users/x/My Project"), "-Users-x-My-Project");
        assert_eq!(sanitize("项目/一"), "项目-一");
    }

    #[test]
    fn a_directory_outside_a_repository_is_its_own_key() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let first = key(dir.path());
        assert!(!first.is_empty());
        assert!(
            !first.contains(std::path::MAIN_SEPARATOR),
            "a key is one level: {first}"
        );
        assert_eq!(first, key(dir.path()), "the same directory, the same key");
    }
}
