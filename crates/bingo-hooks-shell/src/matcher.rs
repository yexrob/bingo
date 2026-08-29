//! A rule's `matcher`, compiled.
//!
//! Claude Code reads a matcher as a whole-string regex over the event's subject
//! — the tool name for the tool events, the trigger or source for the rest — so
//! `Edit` does not select `EditNotebook` and `mcp__.*` selects every server tool.
//! A pattern that is not a regex is far more likely to be a literal tool name
//! than a mistake worth refusing at startup, so it falls back to equality with
//! one warning, said once because a pattern is compiled once.

use regex::Regex;

#[derive(Debug)]
pub struct Matcher {
    pattern: String,
    /// `None` when the pattern is not a regex: equality is the fallback.
    anchored: Option<Regex>,
}

impl Matcher {
    /// Compile one matcher. An absent or empty pattern matches every subject.
    pub fn compile(pattern: Option<&str>) -> Self {
        let pattern = pattern.unwrap_or_default().to_string();
        if pattern.is_empty() {
            return Self {
                pattern,
                anchored: None,
            };
        }
        let anchored = match Regex::new(&format!("^(?:{pattern})$")) {
            Ok(regex) => Some(regex),
            Err(error) => {
                tracing::warn!(
                    matcher = %pattern,
                    %error,
                    "hook matcher is not a regex; matching the subject exactly instead"
                );
                None
            }
        };
        Self { pattern, anchored }
    }

    pub fn matches(&self, subject: &str) -> bool {
        match &self.anchored {
            _ if self.pattern.is_empty() => true,
            Some(regex) => regex.is_match(subject),
            None => self.pattern == subject,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: Option<&str>, subject: &str) -> bool {
        Matcher::compile(pattern).matches(subject)
    }

    #[test]
    fn the_matcher_table() {
        let table = [
            // pattern, subject, matches
            (Some("Edit"), "Edit", true),
            (Some("Edit"), "EditNotebook", false),
            (Some("Edit"), "PreEdit", false),
            (Some("Edit|Write"), "Write", true),
            (Some("Edit|Write"), "Bash", false),
            (Some("mcp__.*"), "mcp__test__echo", true),
            (Some("mcp__.*"), "Bash", false),
            // Empty and absent select every subject.
            (Some(""), "anything", true),
            (None, "anything", true),
            (None, "", true),
            // A pattern that will not compile is read as a literal.
            (Some("Edit("), "Edit(", true),
            (Some("Edit("), "Edit", false),
            (Some("*Write"), "*Write", true),
        ];
        for (pattern, subject, expected) in table {
            assert_eq!(
                matches(pattern, subject),
                expected,
                "matcher {pattern:?} against {subject:?}"
            );
        }
    }

    #[test]
    fn a_pattern_is_compiled_once_and_reused() {
        let matcher = Matcher::compile(Some("Edit|Write"));
        assert!(matcher.matches("Edit"));
        assert!(matcher.matches("Write"));
        assert!(!matcher.matches("Read"));
    }

    #[test]
    fn an_anchored_pattern_stays_anchored_when_it_alternates() {
        // `^Edit|Write$` would match `xWritey`; `^(?:Edit|Write)$` does not.
        assert!(!matches(Some("Edit|Write"), "xWritey"));
    }
}
