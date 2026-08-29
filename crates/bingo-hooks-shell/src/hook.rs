//! The one [`Hook`]: which sdk lifecycle point is which Claude Code event, and
//! what a hook's answer means to the kernel.
//!
//! | point | event | what an answer can do |
//! |---|---|---|
//! | `before_tool` | `PreToolUse` | rewrite the input, deny, ask |
//! | `after_tool` | `PostToolUse` / `PostToolUseFailure` | end the turn after this round |
//! | `on_submit` | `UserPromptSubmit` | reject the input, append context to it |
//! | `on_stop` | `Stop` | one more turn, with a reason |
//! | `on_compact(Start)` | `PreCompact` | nothing |
//! | `on_session` | `SessionStart` / `SessionEnd` | nothing (`SessionStart` exports) |
//! | `on_event` | `Notification` / `PermissionRequest` | nothing |
//!
//! `on_turn` is not claimed: no Claude Code event marks a turn's edges, and
//! `Stop` — which does — is `on_stop`. `PostCompact` is not served either: the
//! plan's brick 4 stops at `PreCompact`.

use std::path::Path;

use async_trait::async_trait;
use bingo_sdk::{
    Event, Frame, Hook, HookContext, HookMatcher, HookOutcome, HookPoint, Input, InteractionKind,
    Level, Phase, ToolCall, ToolOutput,
};
use serde_json::Value;

use crate::config::{HookEntry, HookEvent, Hooks};
use crate::dispatch::{Dispatch, Said};
use crate::events;
use crate::verdict::{self, Decision, Verdict};

/// The subject a matcher is tested against for the events that carry no name of
/// their own; the reference matches `SessionStart` on its source and `PreCompact`
/// on its trigger, and bingo can only ever report these two.
const STARTUP: &str = "startup";
const AUTO: &str = "auto";
const OTHER: &str = "other";
/// `UserPromptSubmit` and `Stop` support no matcher: only an empty one selects them.
const NONE: &str = "";

#[derive(Debug)]
pub struct ShellHooks {
    dispatch: Dispatch,
}

impl ShellHooks {
    pub fn new(hooks: &Hooks, data_dir: &Path) -> Self {
        Self {
            dispatch: Dispatch::new(hooks, data_dir),
        }
    }

    /// Run every hook for an event whose answer the kernel does not read.
    async fn observe(
        &self,
        event: HookEvent,
        entries: Vec<&HookEntry>,
        payload: Value,
        cx: &HookContext,
    ) {
        for entry in entries {
            let _ = self.dispatch.speak(event, entry, &payload, cx).await;
        }
    }

    async fn session_start(&self, cx: &HookContext) {
        let entries = self.dispatch.select(HookEvent::SessionStart, STARTUP);
        if entries.is_empty() {
            return;
        }
        self.dispatch.sessions().open(&cx.session);
        let payload = events::session_start(events::common(cx));
        for entry in entries {
            let _ = self
                .dispatch
                .speak(HookEvent::SessionStart, entry, &payload, cx)
                .await;
            // Read after each one, so a later hook both sees what an earlier one
            // exported and can add to it.
            self.dispatch.sessions().absorb(&cx.session);
        }
    }

    async fn session_end(&self, cx: &HookContext) {
        let entries = self.dispatch.select(HookEvent::SessionEnd, OTHER);
        if !entries.is_empty() {
            let payload = events::session_end(events::common(cx));
            self.observe(HookEvent::SessionEnd, entries, payload, cx)
                .await;
        }
        self.dispatch.sessions().close(&cx.session);
    }

    async fn notification(&self, level: Level, code: &str, text: &str, cx: &HookContext) {
        let entries = self.dispatch.select(HookEvent::Notification, code);
        if entries.is_empty() {
            return;
        }
        let payload = events::notification(events::common(cx), level, code, text);
        self.observe(HookEvent::Notification, entries, payload, cx)
            .await;
    }

    async fn permission_request(&self, tool: &str, summary: &str, cx: &HookContext) {
        let entries = self.dispatch.select(HookEvent::PermissionRequest, tool);
        if entries.is_empty() {
            return;
        }
        let payload = events::permission_request(events::common(cx), tool, summary);
        self.observe(HookEvent::PermissionRequest, entries, payload, cx)
            .await;
    }
}

#[async_trait]
impl Hook for ShellHooks {
    fn id(&self) -> &str {
        "bingo.hooks.shell"
    }

    fn matcher(&self) -> HookMatcher {
        HookMatcher {
            points: vec![
                HookPoint::Submit,
                HookPoint::BeforeTool,
                HookPoint::AfterTool,
                HookPoint::Stop,
                HookPoint::Compact,
                HookPoint::Session,
                HookPoint::Event,
            ],
            // The tool name is matched here, against each rule's own regex.
            tool: None,
        }
    }

    async fn on_submit(&self, input: &mut Input, cx: &HookContext) -> HookOutcome {
        let Input::Text { text, .. } = input else {
            // An action carries no prompt for a hook to read or rewrite.
            return HookOutcome::Continue;
        };
        let entries = self.dispatch.select(HookEvent::UserPromptSubmit, NONE);
        if entries.is_empty() {
            return HookOutcome::Continue;
        }
        let payload = events::user_prompt_submit(events::common(cx), text);
        let mut added: Vec<String> = Vec::new();
        for entry in entries {
            let said = self
                .dispatch
                .speak(HookEvent::UserPromptSubmit, entry, &payload, cx)
                .await;
            match submitted(said, &mut added) {
                Some(reason) => return HookOutcome::Deny { reason },
                None => continue,
            }
        }
        for context in added {
            text.push('\n');
            text.push_str(&context);
        }
        HookOutcome::Continue
    }

    async fn before_tool(&self, call: &mut ToolCall, cx: &HookContext) -> HookOutcome {
        let entries = self.dispatch.select(HookEvent::PreToolUse, &call.name);
        for entry in entries {
            // Rebuilt per hook, so the next one sees what the last one rewrote.
            let payload = events::pre_tool_use(events::common(cx), call);
            let said = self
                .dispatch
                .speak(HookEvent::PreToolUse, entry, &payload, cx)
                .await;
            if let Some(outcome) = asked(said, call) {
                return outcome;
            }
        }
        HookOutcome::Continue
    }

    async fn after_tool(
        &self,
        call: &ToolCall,
        output: &ToolOutput,
        cx: &HookContext,
    ) -> HookOutcome {
        let event = match output.is_error {
            true => HookEvent::PostToolUseFailure,
            false => HookEvent::PostToolUse,
        };
        let entries = self.dispatch.select(event, &call.name);
        if entries.is_empty() {
            return HookOutcome::Continue;
        }
        let payload = result_payload(event, call, output, cx);
        let mut block: Option<String> = None;
        for entry in entries {
            // Every hook runs: the later ones are usually formatters, and one
            // hook's objection is no reason to skip another's work.
            let said = self.dispatch.speak(event, entry, &payload, cx).await;
            if let Some(reason) = blocked(said) {
                block.get_or_insert(because(reason, event));
            }
        }
        block.map_or(HookOutcome::Continue, |reason| HookOutcome::Block {
            reason,
        })
    }

    async fn on_stop(&self, cx: &HookContext) -> HookOutcome {
        let entries = self.dispatch.select(HookEvent::Stop, NONE);
        if entries.is_empty() {
            return HookOutcome::Continue;
        }
        let payload = events::stop(events::common(cx));
        for entry in entries {
            let said = self
                .dispatch
                .speak(HookEvent::Stop, entry, &payload, cx)
                .await;
            if let Some(reason) = blocked(said) {
                return HookOutcome::Block {
                    reason: because(reason, HookEvent::Stop),
                };
            }
        }
        HookOutcome::Continue
    }

    async fn on_compact(&self, phase: Phase, cx: &HookContext) {
        if phase != Phase::Start {
            return;
        }
        let entries = self.dispatch.select(HookEvent::PreCompact, AUTO);
        if entries.is_empty() {
            return;
        }
        let payload = events::pre_compact(events::common(cx));
        self.observe(HookEvent::PreCompact, entries, payload, cx)
            .await;
    }

    async fn on_session(&self, phase: Phase, cx: &HookContext) {
        match phase {
            Phase::Start => self.session_start(cx).await,
            Phase::End => self.session_end(cx).await,
        }
    }

    async fn on_event(&self, frame: &Frame, cx: &HookContext) {
        match &frame.event {
            Event::Notice { level, code, text } => self.notification(*level, code, text, cx).await,
            Event::InteractionOpened { interaction } => {
                if let InteractionKind::Permission { tool, summary, .. } = &interaction.kind {
                    self.permission_request(tool, summary, cx).await;
                }
            }
            _ => {}
        }
    }
}

fn result_payload(
    event: HookEvent,
    call: &ToolCall,
    output: &ToolOutput,
    cx: &HookContext,
) -> Value {
    match event {
        HookEvent::PostToolUseFailure => {
            events::post_tool_use_failure(events::common(cx), call, output)
        }
        _ => events::post_tool_use(events::common(cx), call, output),
    }
}

/// What one `PreToolUse` hook did to the call, and whether it settled it.
fn asked(said: Said, call: &mut ToolCall) -> Option<HookOutcome> {
    let verdict = match said {
        Said::Blocked(reason) => {
            return Some(HookOutcome::Deny {
                reason: because(reason, HookEvent::PreToolUse),
            });
        }
        Said::Nothing => return None,
        Said::Spoke(verdict) => verdict,
    };
    if let Some(update) = verdict.updated_input() {
        verdict::apply(&mut call.input, update);
    }
    if let Some(reason) = verdict.halt() {
        return Some(HookOutcome::Deny { reason });
    }
    let (decision, reason) = verdict.decision()?;
    let reason = because(reason, HookEvent::PreToolUse);
    match decision {
        // `allow` cannot skip the gate: the kernel's one permission path is the
        // policy, and a hook is not it.
        Decision::Allow => None,
        Decision::Ask => Some(HookOutcome::Ask { reason }),
        Decision::Deny | Decision::Block => Some(HookOutcome::Deny { reason }),
    }
}

/// What one `UserPromptSubmit` hook said: a refusal, or context to append.
fn submitted(said: Said, added: &mut Vec<String>) -> Option<String> {
    match said {
        Said::Blocked(reason) => Some(because(reason, HookEvent::UserPromptSubmit)),
        Said::Nothing => None,
        Said::Spoke(verdict) => {
            if let Some(reason) = refusal(&verdict) {
                return Some(because(reason, HookEvent::UserPromptSubmit));
            }
            if let Some(context) = verdict.additional_context() {
                added.push(context.to_string());
            }
            None
        }
    }
}

/// The reason one hook gave for stopping, whichever way it gave it.
fn blocked(said: Said) -> Option<String> {
    match said {
        Said::Blocked(reason) => Some(reason),
        Said::Nothing => None,
        Said::Spoke(verdict) => refusal(&verdict),
    }
}

/// The reason a hook that ran cleanly still wants everything to stop.
fn refusal(verdict: &Verdict) -> Option<String> {
    verdict.halt().or_else(|| match verdict.decision() {
        Some((Decision::Block | Decision::Deny, reason)) => Some(reason),
        _ => None,
    })
}

/// A hook that objected without saying why still owes the model a sentence.
fn because(reason: String, event: HookEvent) -> String {
    match reason.is_empty() {
        true => format!("a {} hook said so", event.name()),
        false => reason,
    }
}

#[cfg(test)]
mod tests;
