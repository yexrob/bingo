//! One plugin hook as a bingo hook.
//!
//! The kernel keeps seeing `Arc<dyn Hook>` and never learns which of them are
//! processes: this struct implements the sdk's own trait and its methods are
//! wire calls (ADR-0030 §1, ADR-0032 §1). The matcher is not one of them — it
//! is handshake data, asked once — so an event this hook did not claim is
//! never sent, and a plugin that watches one tool costs nothing on every other.
//!
//! The two kinds of point differ in what happens to the answer. A decision
//! point waits, bounded by [`deadline::HOOK`], and a hook that errors or
//! misses it never gets to decide: the host continues and a notice names it,
//! which is hooks-shell's precedent for a hook that did not run (ADR-0032 §5).
//! An observation point sends a notification and returns at once, so watching
//! a turn costs the turn nothing.
//!
//! What comes back can only tighten. `HookOutcome` has no `Allow`, and the
//! rewritten value is applied at exactly the two points that own a mutable
//! argument — anywhere else the shape carries no variant to apply.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Frame, Hook, HookContext, HookMatcher, HookOutcome, Input, Item, Phase, ToolCall, ToolOutput,
    TurnId,
};

use crate::connection::Connection;
use crate::deadline;
use crate::notice::{Notice, Notices};
use crate::wire::{
    HookDecideParams, HookDecideResult, HookDecision, HookObservation, HookObserveParams, HookSite,
    HookSpec, HookValue, name,
};

/// The kernel-visible id of a plugin's hook: the plugin's name and the hook's
/// own. Two plugins may both declare a `guard`, and a refusal still says which
/// one refused.
pub fn hook_id(plugin: &str, hook: &str) -> String {
    format!("{plugin}:{hook}")
}

/// A hook a plugin process declared, bound to the pipe that answers it.
pub struct RemoteHook {
    /// The id the kernel sees, the plugin's name in it; the process is asked
    /// by [`HookSpec::id`].
    id: String,
    spec: HookSpec,
    connection: Arc<Connection>,
    notices: Arc<Notices>,
}

impl RemoteHook {
    pub fn new(
        plugin: &str,
        spec: HookSpec,
        connection: Arc<Connection>,
        notices: Arc<Notices>,
    ) -> Self {
        Self {
            id: hook_id(plugin, &spec.id),
            spec,
            connection,
            notices,
        }
    }

    /// What the process decided, or nothing — which is not a decision and is
    /// never read as one.
    async fn decide(&self, cx: &HookContext, decision: HookDecision) -> Option<HookDecideResult> {
        let params = HookDecideParams {
            id: self.spec.id.clone(),
            site: HookSite::from(cx),
            decision,
        };
        match self.ask(params).await {
            Ok(result) => Some(result),
            Err(why) => {
                self.never_decided(&why);
                None
            }
        }
    }

    async fn ask(&self, params: HookDecideParams) -> Result<HookDecideResult, String> {
        let value = serde_json::to_value(params).map_err(|e| e.to_string())?;
        let answered = tokio::time::timeout(
            deadline::HOOK,
            self.connection.request(name::HOOK_DECIDE, value),
        )
        .await;
        match answered {
            Ok(Ok(value)) => serde_json::from_value(value).map_err(|e| e.to_string()),
            Ok(Err(error)) => Err(error.message),
            Err(_) => Err(format!("nothing within {}s", deadline::HOOK.as_secs())),
        }
    }

    /// A hook that did not answer is one the person should hear about: it was
    /// installed to decide something and did not.
    fn never_decided(&self, why: &str) {
        self.notices.push(Notice::warn(
            "HOOK_UNANSWERED",
            format!(
                "the hook {} did not decide: {why}; the host went on",
                self.id
            ),
        ));
    }

    /// A point that owns no mutable argument: the outcome alone, and
    /// `Continue` where there was none.
    async fn judged(&self, cx: &HookContext, decision: HookDecision) -> HookOutcome {
        self.decide(cx, decision)
            .await
            .map(|decided| decided.outcome)
            .unwrap_or(HookOutcome::Continue)
    }

    /// Tell the process, and do not wait: nothing here has an answer to read.
    async fn observe(&self, cx: &HookContext, observation: HookObservation) {
        let params = HookObserveParams {
            id: self.spec.id.clone(),
            site: HookSite::from(cx),
            observation,
        };
        let Ok(value) = serde_json::to_value(params) else {
            return;
        };
        self.connection.notify(name::HOOK_OBSERVE, value).await;
    }
}

#[async_trait]
impl Hook for RemoteHook {
    fn id(&self) -> &str {
        &self.id
    }

    /// Handshake data: asked once, when the process said what it claims.
    fn matcher(&self) -> HookMatcher {
        self.spec.matcher.clone()
    }

    async fn on_submit(&self, input: &mut Input, cx: &HookContext) -> HookOutcome {
        let decision = HookDecision::Submit {
            input: input.clone(),
        };
        let Some(decided) = self.decide(cx, decision).await else {
            return HookOutcome::Continue;
        };
        if let Some(HookValue::Input { input: rewritten }) = decided.value {
            *input = rewritten;
        }
        decided.outcome
    }

    async fn before_tool(&self, call: &mut ToolCall, cx: &HookContext) -> HookOutcome {
        let decision = HookDecision::BeforeTool { call: call.clone() };
        let Some(decided) = self.decide(cx, decision).await else {
            return HookOutcome::Continue;
        };
        if let Some(HookValue::Call { call: rewritten }) = decided.value {
            *call = rewritten;
        }
        decided.outcome
    }

    async fn after_tool(
        &self,
        call: &ToolCall,
        output: &ToolOutput,
        cx: &HookContext,
    ) -> HookOutcome {
        let decision = HookDecision::AfterTool {
            call: call.clone(),
            output: output.clone(),
        };
        self.judged(cx, decision).await
    }

    async fn on_stop(&self, cx: &HookContext) -> HookOutcome {
        self.judged(cx, HookDecision::Stop).await
    }

    async fn on_turn(&self, phase: Phase, turn: &TurnId, items: &[Item], cx: &HookContext) {
        self.observe(
            cx,
            HookObservation::Turn {
                phase,
                turn: turn.clone(),
                items: items.to_vec(),
            },
        )
        .await;
    }

    async fn on_compact(&self, phase: Phase, cx: &HookContext) {
        self.observe(cx, HookObservation::Compact { phase }).await;
    }

    async fn on_session(&self, phase: Phase, cx: &HookContext) {
        self.observe(cx, HookObservation::Session { phase }).await;
    }

    async fn on_event(&self, frame: &Frame, cx: &HookContext) {
        self.observe(
            cx,
            HookObservation::Event {
                frame: Box::new(frame.clone()),
            },
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{hook_context, unanswering};
    use bingo_sdk::{HookPoint, Origin};
    use serde_json::json;

    fn declared(points: Vec<HookPoint>, tool: Option<&str>) -> HookSpec {
        HookSpec {
            id: "guard".into(),
            matcher: HookMatcher {
                points,
                tool: tool.map(str::to_string),
            },
        }
    }

    fn silent(spec: HookSpec) -> RemoteHook {
        RemoteHook::new("notes", spec, unanswering(), Arc::new(Notices::default()))
    }

    #[test]
    fn a_hook_is_named_for_its_plugin_and_itself() {
        assert_eq!(hook_id("notes", "guard"), "notes:guard");
    }

    /// The matcher is the declaration's, asked once and never over the wire:
    /// it is what keeps an unmatched event off the pipe at all.
    #[tokio::test]
    async fn the_matcher_is_the_one_the_handshake_declared() {
        let remote = silent(declared(vec![HookPoint::BeforeTool], Some("Bash")));
        assert_eq!(remote.id(), "notes:guard");
        assert_eq!(
            remote.matcher(),
            HookMatcher {
                points: vec![HookPoint::BeforeTool],
                tool: Some("Bash".into()),
            }
        );
    }

    /// The whole protection of the hot path, on a clock that does not tick: a
    /// process that says nothing decides nothing, the call goes through
    /// untouched, and a notice names the hook that did not answer.
    #[tokio::test(start_paused = true)]
    async fn a_hook_past_its_deadline_never_decides_and_a_notice_names_it() {
        let notices = Arc::new(Notices::default());
        let remote = RemoteHook::new(
            "notes",
            declared(vec![HookPoint::BeforeTool], None),
            unanswering(),
            Arc::clone(&notices),
        );
        let mut call = ToolCall {
            call_id: "c1".into(),
            name: "Bash".into(),
            input: json!({ "command": "ls" }),
        };
        let outcome = remote.before_tool(&mut call, &hook_context()).await;
        assert_eq!(outcome, HookOutcome::Continue);
        assert_eq!(
            call.input,
            json!({ "command": "ls" }),
            "nothing was rewritten"
        );
        let said = notices.drain();
        assert_eq!(said.len(), 1, "{said:?}");
        assert_eq!(said[0].code, "HOOK_UNANSWERED");
        assert!(
            said[0].text.contains("notes:guard") && said[0].text.contains("within 5s"),
            "{}",
            said[0].text
        );
    }

    /// The same on the submission path: the input reaches the turn as it was.
    #[tokio::test(start_paused = true)]
    async fn a_submission_a_hook_never_answered_for_goes_on_unchanged() {
        let remote = silent(declared(vec![HookPoint::Submit], None));
        let mut input = Input::text("hello", Origin::surface("test"));
        let outcome = remote.on_submit(&mut input, &hook_context()).await;
        assert_eq!(outcome, HookOutcome::Continue);
        assert_eq!(input, Input::text("hello", Origin::surface("test")));
    }

    /// An observation waits for nothing, so a process that answers nothing
    /// costs nothing: this returns on a clock that never advances.
    #[tokio::test(start_paused = true)]
    async fn an_observation_returns_at_once_however_silent_the_process_is() {
        let notices = Arc::new(Notices::default());
        let remote = RemoteHook::new(
            "notes",
            declared(vec![HookPoint::Session], None),
            unanswering(),
            Arc::clone(&notices),
        );
        remote.on_session(Phase::Start, &hook_context()).await;
        assert!(
            notices.drain().is_empty(),
            "nothing was awaited, so nothing was missed"
        );
    }
}
