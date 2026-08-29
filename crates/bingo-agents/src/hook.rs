//! `@name` in the composer: the line goes to that agent instead of to the
//! session it was typed in (ADR-0010 §2). Everything else is left alone —
//! this is the one hook, and a line it does not recognise is not its
//! business.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{Hook, HookContext, HookMatcher, HookOutcome, HookPoint, Input};

use crate::handle::LateHost;
use crate::names;

/// `@name rest` → the name, and what follows it.
///
/// A line that is only a name addresses nobody: there would be nothing to
/// deliver, so it stays a prompt for this session. An `@` inside a sentence is
/// a handle or an address, not a redirect.
pub fn at_prefix(text: &str) -> Option<(&str, &str)> {
    let rest = text.trim_start().strip_prefix('@')?;
    let end = rest.find(char::is_whitespace)?;
    let (name, tail) = rest.split_at(end);
    let tail = tail.trim_start();
    (!name.is_empty() && !tail.is_empty()).then_some((name, tail))
}

/// Reads the first word of a submitted line and, when it names a child of
/// this session, sends the rest there.
#[derive(Debug)]
pub struct AtNameHook {
    host: Arc<LateHost>,
}

impl AtNameHook {
    pub fn new(host: Arc<LateHost>) -> Self {
        Self { host }
    }
}

/// The line as the agent should read it: the address is for the router, not
/// for the reader.
fn strip(input: &mut Input, rest: String) {
    if let Input::Text { text, .. } = input {
        *text = rest;
    }
}

#[async_trait]
impl Hook for AtNameHook {
    fn id(&self) -> &str {
        "agents.at-name"
    }

    fn matcher(&self) -> HookMatcher {
        HookMatcher {
            points: vec![HookPoint::Submit],
            tool: None,
        }
    }

    async fn on_submit(&self, input: &mut Input, cx: &HookContext) -> HookOutcome {
        // Before `start` there is no session tree to ask, and every line is
        // this session's own.
        let Some(host) = self.host.get() else {
            return HookOutcome::Continue;
        };
        let Input::Text { text, .. } = &*input else {
            return HookOutcome::Continue;
        };
        let Some((name, rest)) = at_prefix(text) else {
            return HookOutcome::Continue;
        };
        let (name, rest) = (name.to_string(), rest.to_string());
        match names::child(host, &cx.session, &name).await {
            Ok(session) => {
                strip(input, rest);
                HookOutcome::Redirect { session }
            }
            // A name that is nobody's is a line about an `@name`, not to one.
            Err(_) => HookOutcome::Continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fleet, hook_context};
    use bingo_sdk::Origin;

    fn typed(line: &str) -> Input {
        Input::text(line, Origin::surface("tui"))
    }

    fn text_of(input: &Input) -> &str {
        match input {
            Input::Text { text, .. } => text,
            _ => "",
        }
    }

    async fn submitted(line: &str) -> (HookOutcome, Input) {
        let fleet = Fleet::default();
        let root = fleet.root();
        fleet.child(&root, "reviewer");
        let hook = AtNameHook::new(fleet.late());
        let mut input = typed(line);
        let outcome = hook.on_submit(&mut input, &hook_context(&root)).await;
        (outcome, input)
    }

    #[tokio::test]
    async fn a_line_addressed_to_a_child_is_stripped_and_redirected() {
        let (outcome, input) = submitted("@reviewer look at the diff again").await;
        assert!(
            matches!(outcome, HookOutcome::Redirect { .. }),
            "{outcome:?}"
        );
        assert_eq!(text_of(&input), "look at the diff again");
    }

    #[tokio::test]
    async fn a_name_nobody_has_is_an_ordinary_prompt() {
        let (outcome, input) = submitted("@nobody are you there").await;
        assert_eq!(outcome, HookOutcome::Continue);
        assert_eq!(text_of(&input), "@nobody are you there");
    }

    #[tokio::test]
    async fn a_line_with_nothing_to_deliver_is_an_ordinary_prompt() {
        let (outcome, input) = submitted("@reviewer").await;
        assert_eq!(outcome, HookOutcome::Continue);
        assert_eq!(text_of(&input), "@reviewer");
    }

    #[tokio::test]
    async fn nothing_is_redirected_before_the_plugin_starts() {
        let hook = AtNameHook::new(Arc::new(LateHost::default()));
        let mut input = typed("@reviewer hello");
        let session = bingo_sdk::SessionId::from_raw("ses_root");
        let outcome = hook.on_submit(&mut input, &hook_context(&session)).await;
        assert_eq!(outcome, HookOutcome::Continue);
        assert_eq!(text_of(&input), "@reviewer hello");
    }

    #[test]
    fn what_counts_as_an_address() {
        assert_eq!(at_prefix("@reviewer fix it"), Some(("reviewer", "fix it")));
        assert_eq!(
            at_prefix("  @reviewer  fix it"),
            Some(("reviewer", "fix it"))
        );
        assert_eq!(at_prefix("@reviewer\nfix it"), Some(("reviewer", "fix it")));
        for plain in [
            "@reviewer",
            "@reviewer   ",
            "@",
            "mail me at a@b.c",
            "hello",
        ] {
            assert_eq!(at_prefix(plain), None, "{plain:?} was read as an address");
        }
    }

    #[test]
    fn it_asks_for_the_submit_point_only() {
        let hook = AtNameHook::new(Arc::new(LateHost::default()));
        assert_eq!(hook.id(), "agents.at-name");
        assert_eq!(hook.matcher().points, [HookPoint::Submit]);
        assert!(hook.matcher().tool.is_none());
    }
}
