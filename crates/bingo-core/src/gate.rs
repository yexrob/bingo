//! The permission gate: hooks, then the one policy, then a person when the
//! answer is `Ask`. The policy never resolves `Ask`; the gate does, so the
//! "unreachable" arm of the old design cannot exist.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::*;
use jiff::Timestamp;

use crate::executor::Gate;

/// With no policy registered: read-only trusted tools run, everything else asks.
#[derive(Debug, Default)]
pub struct DefaultPolicy;

#[async_trait]
impl PermissionPolicy for DefaultPolicy {
    fn id(&self) -> &str {
        "bingo.default"
    }

    async fn decide(&self, input: PolicyInput<'_>) -> Decision {
        if input.traits.trusted && input.traits.read_only && input.confirm.is_none() {
            Decision::Allow {
                reason: Reason::ReadOnly,
            }
        } else {
            Decision::Ask {
                reason: Reason::Default,
                scope: None,
            }
        }
    }
}

/// Whether a hook wants this point for this tool. `tool` is an exact name or a `prefix*`.
pub fn hook_applies(matcher: &HookMatcher, point: HookPoint, tool: Option<&str>) -> bool {
    if !matcher.points.is_empty() && !matcher.points.contains(&point) {
        return false;
    }
    match (&matcher.tool, tool) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(pattern), Some(name)) => match pattern.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => pattern == name,
        },
    }
}

pub struct GateInput<'a> {
    pub session: &'a SessionId,
    pub cwd: &'a Path,
    pub item: &'a ItemId,
    pub call: ToolCall,
    pub tool: Option<Arc<dyn Tool>>,
    pub policy: &'a dyn PermissionPolicy,
    pub hooks: &'a [Arc<dyn Hook>],
    pub hook_cx: &'a HookContext,
}

pub struct Gated {
    pub call: ToolCall,
    pub traits: ToolTraits,
    pub gate: Gate,
    /// The receipt to record when a person decided.
    pub receipt: Option<ItemBody>,
}

/// Decide one call. Hooks may rewrite the input; the first non-`Continue` wins.
pub async fn gate_call(input: GateInput<'_>, prompter: &dyn Prompter) -> Gated {
    let GateInput {
        session,
        cwd,
        item: _,
        mut call,
        tool,
        policy,
        hooks,
        hook_cx,
    } = input;
    let mut forced_ask: Option<String> = None;
    let name = call.name.clone();
    let applicable: Vec<&Arc<dyn Hook>> = hooks
        .iter()
        .filter(|h| hook_applies(&h.matcher(), HookPoint::BeforeTool, Some(&name)))
        .collect();
    for hook in applicable {
        match hook.before_tool(&mut call, hook_cx).await {
            HookOutcome::Continue => {}
            HookOutcome::Deny { reason } | HookOutcome::Block { reason } => {
                let traits = tool
                    .as_ref()
                    .map(|t| t.traits(&call.input))
                    .unwrap_or_default();
                return Gated {
                    call,
                    traits,
                    gate: Gate::Denied {
                        message: format!("Denied by hook {}: {reason}", hook.id()),
                    },
                    receipt: None,
                };
            }
            HookOutcome::Ask { reason } => {
                forced_ask = Some(reason);
                break;
            }
            HookOutcome::Redirect { .. } => {}
        }
    }
    let Some(tool) = tool else {
        return Gated {
            call,
            traits: ToolTraits::default(),
            gate: Gate::Allowed,
            receipt: None,
        };
    };
    let traits = tool.traits(&call.input);
    let subjects = tool.subjects(&call.input, cwd);
    let confirm = tool.confirm(&call.input).or(forced_ask);
    let policy_input = PolicyInput {
        call: &call,
        traits: &traits,
        subjects: &subjects,
        confirm: confirm.as_deref(),
        session,
        cwd,
    };
    let decision = policy.decide(policy_input).await;
    let (scope, reason) = match decision {
        Decision::Deny { reason } => {
            return Gated {
                call,
                traits,
                gate: Gate::Denied {
                    message: format!("Permission denied ({})", describe(&reason)),
                },
                receipt: None,
            };
        }
        Decision::Allow { reason } if confirm.is_none() => {
            let _ = reason;
            return Gated {
                call,
                traits,
                gate: Gate::Allowed,
                receipt: None,
            };
        }
        Decision::Allow { reason } => (None, reason),
        Decision::Ask { reason, scope } => (scope, reason),
    };
    let _ = reason;
    let preview = tool.preview(&call.input, cwd);
    let kind = InteractionKind::Permission {
        tool: call.name.clone(),
        summary: confirm.clone().unwrap_or_else(|| summarize(&call)),
        preview,
        session_scope: scope.clone(),
    };
    let mut answers = vec![AnswerSpec::AllowOnce];
    if scope.is_some() {
        answers.push(AnswerSpec::AllowSession);
    }
    answers.push(AnswerSpec::Deny);
    let answer = prompter.ask(kind, answers).await.unwrap_or(Answer::Cancel);
    let (gate, decision, feedback) = match answer {
        Answer::AllowOnce | Answer::Confirm => (Gate::Allowed, DecisionKind::Allow, None),
        Answer::AllowSession { scope } => {
            policy
                .on_verdict(policy_input, &Verdict::Allow { scope: Some(scope) })
                .await;
            (Gate::Allowed, DecisionKind::AllowSession, None)
        }
        Answer::Deny { feedback } => {
            policy
                .on_verdict(
                    policy_input,
                    &Verdict::Deny {
                        feedback: feedback.clone(),
                    },
                )
                .await;
            let mut message = "Permission denied by the user".to_string();
            if let Some(f) = &feedback {
                message.push_str(": ");
                message.push_str(f);
            }
            (Gate::Denied { message }, DecisionKind::Deny, feedback)
        }
        Answer::Cancel | Answer::Choice { .. } | Answer::Text { .. } => (
            Gate::Denied {
                message: "Permission request cancelled".into(),
            },
            DecisionKind::Deny,
            None,
        ),
    };
    let receipt = ItemBody::PermissionReceipt {
        interaction: InteractionId::from_raw("pending"),
        tool: call.name.clone(),
        decision,
        feedback,
    };
    Gated {
        call,
        traits,
        gate,
        receipt: Some(receipt),
    }
}

fn describe(reason: &Reason) -> String {
    match reason {
        Reason::Rule { rule } => format!("rule {rule}"),
        Reason::Mode { mode } => format!("mode {mode}"),
        Reason::Hook { hook } => format!("hook {hook}"),
        Reason::Safety { detail } => format!("safety: {detail}"),
        Reason::ReadOnly => "read-only".into(),
        Reason::Confirm { detail } => detail.clone(),
        Reason::Default => "default".into(),
    }
}

/// `Name {"k":"v"}` clipped to one line for the prompt.
pub fn summarize(call: &ToolCall) -> String {
    let input = call.input.to_string();
    let input: String = input.chars().take(120).collect();
    format!("{} {input}", call.name)
}

pub fn now() -> Timestamp {
    Timestamp::now()
}
