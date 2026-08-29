//! `BINGO_ENV_FILE`: what a `SessionStart` hook leaves behind for every later
//! hook in the session.
//!
//! Claude Code's `CLAUDE_ENV_FILE` is a shell fragment a `SessionStart` hook
//! appends `export KEY=value` lines to. This reads the same file as data rather
//! than sourcing it: a settings file is not a licence to run arbitrary shell in
//! bingo's own process, and the only thing the contract promises is assignments.
//! A leading `export`, surrounding quotes and `#` comments are all understood;
//! anything else on a line is skipped with a warning, never guessed at.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bingo_sdk::SessionId;

/// Where this session's hooks write their exports.
pub fn path(dir: &Path, session: &SessionId) -> PathBuf {
    dir.join(format!("{session}.env"))
}

/// Read `KEY=VALUE` (or `export KEY=VALUE`) lines into assignments, in file order.
pub fn parse(text: &str) -> BTreeMap<String, String> {
    text.lines().filter_map(assignment).collect()
}

fn assignment(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
    let Some((key, value)) = line.split_once('=') else {
        tracing::warn!(line = %line, "BINGO_ENV_FILE line is not an assignment; skipping it");
        return None;
    };
    let key = key.trim();
    if key.is_empty() || !key.chars().all(|c| c.is_alphanumeric() || c == '_') {
        tracing::warn!(key = %key, "BINGO_ENV_FILE name is not an environment name; skipping it");
        return None;
    }
    Some((key.to_string(), unquote(value.trim()).to_string()))
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> Vec<(String, String)> {
        parse(text).into_iter().collect()
    }

    #[test]
    fn plain_and_exported_assignments_are_both_read() {
        assert_eq!(
            parsed("FOO=bar\nexport BAZ=qux"),
            [
                ("BAZ".to_string(), "qux".to_string()),
                ("FOO".to_string(), "bar".to_string()),
            ]
        );
    }

    #[test]
    fn quotes_are_stripped_and_a_value_may_hold_anything() {
        assert_eq!(
            parsed(r#"PATH="/a:/b""#),
            [("PATH".to_string(), "/a:/b".to_string())]
        );
        assert_eq!(
            parsed("MSG='a = b'"),
            [("MSG".to_string(), "a = b".to_string())]
        );
        assert_eq!(
            parsed("URL=http://x/?a=1"),
            [("URL".to_string(), "http://x/?a=1".to_string())]
        );
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        assert_eq!(
            parsed("# a note\n\n  \nFOO=bar"),
            [("FOO".to_string(), "bar".to_string())]
        );
    }

    #[test]
    fn a_later_line_wins_over_an_earlier_one() {
        assert_eq!(
            parsed("FOO=one\nFOO=two"),
            [("FOO".to_string(), "two".to_string())]
        );
    }

    #[test]
    fn a_line_that_is_not_an_assignment_is_skipped_not_guessed_at() {
        assert!(parsed("source /etc/profile").is_empty());
        assert!(parsed("=value").is_empty());
        assert!(parsed("not a name=value").is_empty());
    }

    #[test]
    fn an_empty_value_is_an_assignment() {
        assert_eq!(parsed("FOO="), [("FOO".to_string(), String::new())]);
    }

    #[test]
    fn the_path_names_the_session() {
        let session = SessionId::from_raw("ses_01");
        assert!(
            path(Path::new("/data/hooks"), &session).ends_with("ses_01.env"),
            "the file is not named for the session"
        );
    }
}
