//! The narrowest rule that could cover this call, offered as "allow for the
//! session".
//!
//! Deriving a rule and matching one are two readings of the same grammar, so a
//! candidate is only ever a proposal: the caller runs the ladder again with it
//! installed and keeps it only if the answer turns into an allow. That is what
//! stops `cd /tmp && rm -rf /` being scoped to `Bash(cd:*)`.

use std::path::{MAIN_SEPARATOR, Path};

use bingo_sdk::Subject;

use crate::rule::Call;
use crate::{path, split, url};

pub fn narrowest(call: Call<'_>, cwd: &Path, home: Option<&Path>) -> Option<String> {
    let subject = match call.subjects {
        [] => return Some(call.name.to_string()),
        [only] => only,
        // Two subjects have no narrowest rule between them; a rule wide enough
        // for both is wider than the prompt the user answered.
        _ => return None,
    };
    match subject {
        Subject::Command { command } => command_rule(call.name, command),
        Subject::Path { path } => path_rule(call.name, path, cwd, home),
        Subject::Url { url } => url_rule(call.name, url),
        Subject::Name { name } => Some(format!("{}({name})", call.name)),
    }
}

/// Only a simple command: the head of `cd /tmp && rm -rf /` says nothing about
/// the `rm`, so a rule built from it would promise a silence it cannot keep.
fn command_rule(tool: &str, command: &str) -> Option<String> {
    let split = split::split(command);
    if !split.is_simple() {
        return None;
    }
    Some(format!("{tool}({}:*)", split.head()?))
}

/// A file tool is scoped to the directory it touches — but never to the root,
/// which is not a scope at all.
fn path_rule(tool: &str, target: &Path, cwd: &Path, home: Option<&Path>) -> Option<String> {
    let normalized = path::normalize_path(target, cwd, home);
    let parent = Path::new(&normalized).parent()?;
    // The root has no parent of its own, and neither does the empty parent of
    // a bare name: neither is a scope.
    parent.parent()?;
    let mut dir = parent.to_string_lossy().into_owned();
    if !dir.ends_with(MAIN_SEPARATOR) {
        dir.push(MAIN_SEPARATOR);
    }
    Some(format!("{tool}({dir})"))
}

fn url_rule(tool: &str, url: &str) -> Option<String> {
    Some(format!("{tool}(domain:{})", url::host(url)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cwd() -> PathBuf {
        PathBuf::from("/work/project")
    }

    fn scope(name: &str, subjects: &[Subject]) -> Option<String> {
        narrowest(Call { name, subjects }, &cwd(), None)
    }

    fn command(text: &str) -> Vec<Subject> {
        vec![Subject::Command {
            command: text.to_string(),
        }]
    }

    #[test]
    fn a_simple_command_is_scoped_to_its_first_word() {
        assert_eq!(
            scope("Bash", &command("cargo test --locked")),
            Some("Bash(cargo:*)".to_string())
        );
    }

    #[test]
    fn a_compound_or_unreadable_command_has_no_scope() {
        for text in ["cd /tmp && rm -rf /", "ls; rm -rf ~", "ls \"; rm", ""] {
            assert_eq!(scope("Bash", &command(text)), None, "{text:?}");
        }
    }

    #[test]
    fn a_file_tool_is_scoped_to_the_directory_it_touches() {
        let subjects = vec![Subject::Path {
            path: PathBuf::from("note.txt"),
        }];
        assert_eq!(
            scope("Write", &subjects),
            Some("Write(/work/project/)".to_string())
        );
    }

    #[test]
    fn a_file_at_the_root_has_no_directory_worth_scoping_to() {
        let subjects = vec![Subject::Path {
            path: PathBuf::from("/x"),
        }];
        assert_eq!(scope("Write", &subjects), None);
    }

    #[test]
    fn a_url_is_scoped_to_its_host_and_a_name_to_itself() {
        let url = vec![Subject::Url {
            url: "https://example.com/a/b".to_string(),
        }];
        assert_eq!(
            scope("WebFetch", &url),
            Some("WebFetch(domain:example.com)".to_string())
        );
        let name = vec![Subject::Name {
            name: "review-pr".to_string(),
        }];
        assert_eq!(scope("Skill", &name), Some("Skill(review-pr)".to_string()));
    }

    #[test]
    fn a_call_that_names_nothing_is_scoped_to_the_tool() {
        assert_eq!(scope("Write", &[]), Some("Write".to_string()));
    }

    #[test]
    fn a_call_that_names_two_things_has_no_narrowest_rule() {
        let subjects = vec![
            Subject::Path {
                path: PathBuf::from("/work/project/a"),
            },
            Subject::Path {
                path: PathBuf::from("/work/project/b"),
            },
        ];
        assert_eq!(scope("Write", &subjects), None);
    }
}
