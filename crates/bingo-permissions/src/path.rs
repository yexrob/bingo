//! Lexical path normalisation.
//!
//! A rule has to hold for a file that does not exist yet, so nothing here
//! touches the filesystem: `~` expands against the home directory, a relative
//! path against the session's cwd, and `.`/`..` collapse as text. With an
//! absolute `cwd` — the only kind the kernel hands out — normalising twice
//! changes nothing.

use std::path::{Component, MAIN_SEPARATOR, Path, PathBuf};

/// Directories whose contents decide what the tools themselves may do; a write
/// into one is never silently allowed.
const SENSITIVE: &[&str] = &[".git", ".claude", ".vscode", ".idea"];

pub fn normalize(raw: &str, cwd: &Path, home: Option<&Path>) -> String {
    let expanded = expand_home(raw, home);
    let collapsed = collapse(&absolutize(Path::new(&expanded), cwd));
    keep_trailing_separator(collapsed.to_string_lossy().into_owned(), raw)
}

pub fn normalize_path(raw: &Path, cwd: &Path, home: Option<&Path>) -> String {
    normalize(&raw.to_string_lossy(), cwd, home)
}

/// Whether a normalised path is the root or sits under it. `/srcs` is not
/// inside `/src`; only a whole component boundary counts.
pub fn is_within(target: &str, root: &str) -> bool {
    let root = root.trim_end_matches(MAIN_SEPARATOR);
    if root.is_empty() {
        return target.starts_with(MAIN_SEPARATOR);
    }
    target == root
        || target
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with(MAIN_SEPARATOR))
}

/// Whether any component of a normalised path names a sensitive directory.
pub fn is_sensitive(normalized: &str) -> bool {
    Path::new(normalized).components().any(|component| {
        matches!(component, Component::Normal(name)
            if name.to_str().is_some_and(|name| SENSITIVE.contains(&name)))
    })
}

fn expand_home(raw: &str, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return raw.to_string();
    };
    if raw == "~" {
        return home.to_string_lossy().into_owned();
    }
    match raw.strip_prefix("~/") {
        Some(rest) => home.join(rest).to_string_lossy().into_owned(),
        None => raw.to_string(),
    }
}

fn absolutize(raw: &Path, cwd: &Path) -> PathBuf {
    if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        cwd.join(raw)
    }
}

fn collapse(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `Read(/etc/)` and `Read(/etc)` are different rules, and collapsing eats the
/// separator that tells them apart.
fn keep_trailing_separator(normalized: String, raw: &str) -> String {
    let wanted = raw.ends_with('/') || raw.ends_with(MAIN_SEPARATOR);
    if wanted && !normalized.ends_with(MAIN_SEPARATOR) {
        format!("{normalized}{MAIN_SEPARATOR}")
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cwd() -> PathBuf {
        PathBuf::from("/work/project")
    }

    fn home() -> PathBuf {
        PathBuf::from("/home/user")
    }

    fn norm(raw: &str) -> String {
        normalize(raw, &cwd(), Some(&home()))
    }

    #[test]
    fn a_relative_path_resolves_against_the_session_cwd() {
        assert_eq!(norm("src/main.rs"), "/work/project/src/main.rs");
        assert_eq!(norm("./src/main.rs"), "/work/project/src/main.rs");
    }

    #[test]
    fn parent_components_collapse_without_touching_the_disk() {
        assert_eq!(norm("/etc/../etc/passwd"), "/etc/passwd");
        assert_eq!(norm("/etc/./ssh/../passwd"), "/etc/passwd");
        assert_eq!(norm("../other/x"), "/work/other/x");
        assert_eq!(norm("/.."), "/");
    }

    #[test]
    fn a_tilde_expands_to_the_home_directory() {
        assert_eq!(norm("~"), "/home/user");
        assert_eq!(norm("~/.ssh/id_rsa"), "/home/user/.ssh/id_rsa");
        // Only a leading `~/` is a home reference.
        assert_eq!(norm("x~/y"), "/work/project/x~/y");
    }

    #[test]
    fn without_a_home_a_tilde_is_just_a_name() {
        assert_eq!(
            normalize("~/x", &cwd(), None),
            "/work/project/~/x",
            "no home is not an excuse to guess one"
        );
    }

    #[test]
    fn a_trailing_separator_survives() {
        assert_eq!(norm("/etc/"), "/etc/");
        assert_eq!(norm("/etc"), "/etc");
    }

    #[test]
    fn normalising_twice_changes_nothing() {
        for raw in [
            "src/main.rs",
            "/etc/../etc/passwd",
            "~/.ssh/",
            "..",
            "/",
            "",
            "./",
        ] {
            let once = norm(raw);
            assert_eq!(norm(&once), once, "{raw:?}");
        }
    }

    #[test]
    fn containment_stops_at_a_component_boundary() {
        assert!(is_within("/work/project/src", "/work/project"));
        assert!(is_within("/work/project", "/work/project"));
        assert!(is_within("/work/project/src", "/work/project/"));
        assert!(!is_within("/work/project-other/src", "/work/project"));
        assert!(!is_within("/work", "/work/project"));
        assert!(is_within("/anything", "/"));
    }

    #[test]
    fn a_sensitive_directory_is_recognised_anywhere_in_the_path() {
        assert!(is_sensitive("/work/project/.git/config"));
        assert!(is_sensitive("/work/.claude/settings.json"));
        assert!(is_sensitive("/work/.vscode/tasks.json"));
        assert!(is_sensitive("/work/.idea/x"));
        assert!(!is_sensitive("/work/project/src/git/config"));
        assert!(!is_sensitive("/work/project/.gitignore"));
    }
}
