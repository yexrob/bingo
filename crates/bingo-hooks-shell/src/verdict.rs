//! What a hook writes on stdout, read into one answer.
//!
//! Claude Code has moved a hook's decision from the top-level `decision`/`reason`
//! pair into `hookSpecificOutput`, and both dialects are in the wild. Both are
//! accepted and collapsed here into one [`Decision`], so nothing downstream has
//! to know there were ever two spellings; `hookSpecificOutput` wins where they
//! disagree, because it is the one the reference documents.

use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    decision: Option<String>,
    reason: Option<String>,
    /// `false` asks bingo to stop, whatever the decision said.
    #[serde(rename = "continue")]
    proceed: Option<bool>,
    stop_reason: Option<String>,
    hook_specific_output: Option<Specific>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Specific {
    permission_decision: Option<String>,
    permission_decision_reason: Option<String>,
    updated_input: Option<Value>,
    additional_context: Option<String>,
}

/// What a hook asked for, in the one spelling the rest of this crate reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
    Ask,
    Block,
}

impl Decision {
    fn parse(word: &str) -> Option<Self> {
        match word {
            "allow" | "approve" => Some(Self::Allow),
            "deny" => Some(Self::Deny),
            "ask" => Some(Self::Ask),
            "block" => Some(Self::Block),
            _ => None,
        }
    }
}

impl Verdict {
    /// Read one hook's stdout. Output that does not open with `{` is not meant as
    /// JSON at all (the reference reads it as plain text), so it is no verdict
    /// rather than a complaint; output that opens with `{` and does not parse is
    /// a broken hook, and says so.
    pub fn read(stdout: &str) -> Option<Self> {
        let text = stdout.trim();
        if !text.starts_with('{') {
            return None;
        }
        match serde_json::from_str(text) {
            Ok(verdict) => Some(verdict),
            Err(error) => {
                tracing::warn!(%error, "hook printed JSON this plugin cannot read; ignoring it");
                None
            }
        }
    }

    /// The decision and the reason it gave, in either dialect.
    pub fn decision(&self) -> Option<(Decision, String)> {
        let specific = self.hook_specific_output.as_ref();
        let word = specific
            .and_then(|s| s.permission_decision.as_deref())
            .or(self.decision.as_deref())?;
        let Some(decision) = Decision::parse(word) else {
            tracing::warn!(decision = %word, "hook asked for a decision this plugin does not know");
            return None;
        };
        let reason = specific
            .and_then(|s| s.permission_decision_reason.as_deref())
            .or(self.reason.as_deref())
            .unwrap_or_default()
            .to_string();
        Some((decision, reason))
    }

    /// `"continue": false` — the hook asked bingo to stop here, for this reason.
    pub fn halt(&self) -> Option<String> {
        (self.proceed == Some(false)).then(|| {
            self.stop_reason
                .clone()
                .unwrap_or_else(|| "stopped by a hook".to_string())
        })
    }

    /// The fields the hook rewrote on the tool's input.
    pub fn updated_input(&self) -> Option<&Value> {
        self.hook_specific_output.as_ref()?.updated_input.as_ref()
    }

    pub fn additional_context(&self) -> Option<&str> {
        self.hook_specific_output
            .as_ref()?
            .additional_context
            .as_deref()
    }
}

/// Apply one hook's `updatedInput`. Two objects merge field by field, so a hook
/// that rewrites `command` does not erase the `timeout` an earlier one set;
/// anything else replaces, because there are no fields to merge.
pub fn apply(input: &mut Value, update: &Value) {
    match (input, update) {
        (Value::Object(target), Value::Object(fields)) => {
            for (key, value) in fields {
                target.insert(key.clone(), value.clone());
            }
        }
        (target, update) => *target = update.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read(stdout: &str) -> Verdict {
        Verdict::read(stdout).expect("the hook spoke")
    }

    #[test]
    fn the_documented_dialect_is_read() {
        let verdict = read(
            r#"{"hookSpecificOutput": {"hookEventName": "PreToolUse",
                "permissionDecision": "deny", "permissionDecisionReason": "no"}}"#,
        );
        assert_eq!(verdict.decision(), Some((Decision::Deny, "no".into())));
    }

    #[test]
    fn the_older_dialect_is_read_too() {
        let verdict = read(r#"{"decision": "block", "reason": "not yet"}"#);
        assert_eq!(
            verdict.decision(),
            Some((Decision::Block, "not yet".into()))
        );
    }

    #[test]
    fn hook_specific_output_wins_where_the_two_disagree() {
        let verdict = read(
            r#"{"decision": "allow", "reason": "old",
                "hookSpecificOutput": {"permissionDecision": "deny"}}"#,
        );
        assert_eq!(verdict.decision(), Some((Decision::Deny, "old".into())));
    }

    #[test]
    fn a_decision_this_plugin_does_not_know_is_no_decision() {
        assert_eq!(read(r#"{"decision": "denied"}"#).decision(), None);
    }

    #[test]
    fn a_hook_that_says_nothing_decides_nothing() {
        let verdict = read("{}");
        assert_eq!(verdict.decision(), None);
        assert_eq!(verdict.halt(), None);
        assert_eq!(verdict.updated_input(), None);
        assert_eq!(verdict.additional_context(), None);
    }

    #[test]
    fn continue_false_halts_with_its_reason() {
        assert_eq!(
            read(r#"{"continue": false, "stopReason": "budget"}"#).halt(),
            Some("budget".into())
        );
        assert_eq!(
            read(r#"{"continue": false}"#).halt(),
            Some("stopped by a hook".into())
        );
        assert_eq!(read(r#"{"continue": true}"#).halt(), None);
    }

    #[test]
    fn plain_text_on_stdout_is_not_a_verdict() {
        assert!(Verdict::read("looks fine to me").is_none());
        assert!(Verdict::read("").is_none());
        assert!(Verdict::read("   \n ").is_none());
    }

    #[test]
    fn json_that_does_not_parse_is_not_a_verdict() {
        assert!(Verdict::read("{not json}").is_none());
    }

    #[test]
    fn updated_input_merges_field_by_field() {
        let mut input = json!({"command": "rm -rf /", "timeout": 5});
        apply(&mut input, &json!({"command": "echo no"}));
        assert_eq!(input, json!({"command": "echo no", "timeout": 5}));
    }

    #[test]
    fn updated_input_that_is_not_an_object_replaces() {
        let mut input = json!({"command": "ls"});
        apply(&mut input, &json!("nonsense"));
        assert_eq!(input, json!("nonsense"));
    }

    #[test]
    fn additional_context_is_read_from_hook_specific_output() {
        let verdict = read(r#"{"hookSpecificOutput": {"additionalContext": "mind the tests"}}"#);
        assert_eq!(verdict.additional_context(), Some("mind the tests"));
    }
}
