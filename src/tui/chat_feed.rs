//! The console's rows, built from what the core published.
//!
//! Before B7c a run's report reached the screen twice: once into the actor,
//! where it became a sequenced `AppEvent`, and once into a frontend sink that
//! translated it into a `UiEvent` on the way past. The second path is gone. What
//! is left is this: the store folds the one ordered stream, and the console
//! reads the *change* out of it — the same pair a subscriber to a JavaScript
//! store gets, and the reason a GUI can draw this conversation without a second
//! producer being written for it.
//!
//! [`UiEvent`] survives the move and is now what it always was underneath: the
//! console's own render vocabulary. Nothing outside `src/tui/` produces one.
//!
//! **What is translated here is only what the core knows.** The transient tiers
//! — a warning, a slash command's output, a pinned panel, an image that finished
//! loading — are the console's own and reach [`super::Chat::route`] on its local
//! channel, because a terminal's feedback tiers are not session state and the
//! contract deliberately does not carry them.

use super::*;

use crate::app::event::{AppEvent, AppEventPayload};
use crate::app::snapshot::{ItemBody, ItemStatus, TurnStatus};
use crate::query::{ToolCallDone, ToolCallStatus};

/// What a turn that died without a word of its own leaves behind.
///
/// A task that panics executes none of its own code; what closes its turn is the
/// guard's `Drop`, which has a status and no reason. The console said this in
/// its own supervisor before the turn was the core's, and it says the same thing
/// here so that the one path is not two.
const LOST_TURN: &str = "The turn ended unexpectedly; retry or go back.";

impl super::Chat {
    /// Fold everything the core has said since the last look into the rows.
    ///
    /// Returns whether anything the *screen* is showing moved — the same answer
    /// the local channel's drain gives, and for the same reason: a background
    /// instance's stream changes nothing on a page it is not on, and must not
    /// repaint or reset the stall baseline.
    pub(crate) fn drain_frames(&mut self) -> bool {
        let mut seen = false;
        for event in self.store.take_folded() {
            for addressed in self.read_frame(&event) {
                seen |= self.route(addressed);
            }
        }
        seen
    }

    /// One published event, as rows.
    ///
    /// A conversation the console has no key for is one the core named and this
    /// side has not read a summary for yet; there is nothing to draw it on, and
    /// the cut that follows states it.
    fn read_frame(&mut self, event: &AppEvent) -> Vec<crate::ui::Addressed> {
        let mut out = Vec::new();
        let mut send = |to: &crate::ui::ConvKey, event: UiEvent| {
            out.push(crate::ui::Addressed {
                to: to.clone(),
                event,
            })
        };
        match &event.payload {
            AppEventPayload::TurnStarted(changed) => {
                let Some(to) = self.key_of(&changed.conversation_id) else {
                    return out;
                };
                self.stream_units.insert(to.clone(), 0);
                send(&to, UiEvent::TurnStart);
            }
            AppEventPayload::TurnCompleted(changed) => {
                let Some(to) = self.key_of(&changed.conversation_id) else {
                    return out;
                };
                self.stream_units.remove(&to);
                // A failure is not a second ending — it is *the* ending, said
                // with a reason. The console has always drawn a turn-level error
                // instead of a completion row, and the core's terminal state is
                // where that decision now reads from.
                match changed.turn.status {
                    TurnStatus::Failed => {
                        let (code, msg) = match &changed.turn.error {
                            Some(error) => (
                                error.code.clone(),
                                self.auth_hint(&error.code, error.message.clone()),
                            ),
                            None => (crate::error::TURN_LOST.to_string(), LOST_TURN.to_string()),
                        };
                        send(
                            &to,
                            UiEvent::Error {
                                code,
                                msg,
                                level: crate::error::ErrorLevel::Full,
                                context: crate::error::ErrorContext::LongTurn,
                            },
                        );
                    }
                    _ => send(&to, UiEvent::TurnEnd),
                }
            }
            AppEventPayload::TurnRetrying(retry) => {
                let Some(to) = self.key_of(&retry.conversation_id) else {
                    return out;
                };
                // An attempt that never got a word out has no live tail to
                // withdraw, and the console keeps the rows it drew for the round
                // because it drew none.
                if retry.removed_item_ids.is_empty() {
                    return out;
                }
                self.stream_units.insert(to.clone(), 0);
                send(&to, UiEvent::StreamRetry);
            }
            AppEventPayload::TurnRoundCompleted(round) => {
                let Some(to) = self.key_of(&round.conversation_id) else {
                    return out;
                };
                self.stream_units.insert(to.clone(), 0);
                send(&to, UiEvent::RoundEnd);
            }
            AppEventPayload::TurnUsageUpdated(usage) => {
                let Some(to) = self.key_of(&usage.conversation_id) else {
                    return out;
                };
                if let Some(context) = usage.context_usage {
                    send(
                        &to,
                        UiEvent::ContextUsage(crate::context_usage::ContextUsage::new(
                            context.used,
                            context.window,
                            context.trigger,
                        )),
                    );
                }
                if usage.usage.authoritative {
                    // The provider's own count outranks the estimate this side
                    // has been keeping, so the estimate is set to agree with it
                    // rather than carrying on from where it had got to.
                    self.stream_units
                        .insert(to.clone(), usage.usage.output_tokens.saturating_mul(4));
                    send(
                        &to,
                        UiEvent::OutputTokens {
                            tokens: usage.usage.output_tokens,
                            authoritative: true,
                        },
                    );
                }
            }
            AppEventPayload::ItemStarted(changed) => {
                let Some(to) = self.key_of(&changed.conversation_id) else {
                    return out;
                };
                if let ItemBody::ToolCall { name, .. } = &changed.item.body {
                    send(&to, UiEvent::ToolStart { name: name.clone() });
                }
            }
            AppEventPayload::ItemTextDelta(delta) => {
                let Some(to) = self.key_of(&delta.conversation_id) else {
                    return out;
                };
                let tokens = self.count(&to, &delta.delta);
                send(&to, UiEvent::TextDelta(delta.delta.clone()));
                send(
                    &to,
                    UiEvent::OutputTokens {
                        tokens,
                        authoritative: false,
                    },
                );
            }
            AppEventPayload::ItemReasoningDelta(delta) => {
                let Some(to) = self.key_of(&delta.conversation_id) else {
                    return out;
                };
                let tokens = self.count(&to, &delta.delta);
                send(&to, UiEvent::ThinkingDelta(delta.delta.clone()));
                send(
                    &to,
                    UiEvent::OutputTokens {
                        tokens,
                        authoritative: false,
                    },
                );
            }
            // The call's resolved input, which is the fold decision's one input.
            AppEventPayload::ItemUpdated(changed) => {
                let Some(to) = self.key_of(&changed.conversation_id) else {
                    return out;
                };
                if let ItemBody::ToolCall {
                    tool_call_id,
                    name,
                    input,
                    ..
                } = &changed.item.body
                {
                    send(
                        &to,
                        UiEvent::ToolReady {
                            tool_call_id: tool_call_id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                            standalone: standalone_shell(tool_call_id),
                        },
                    );
                }
            }
            AppEventPayload::ItemCommandTailUpdated(updated) => {
                let Some(to) = self.key_of(&updated.conversation_id) else {
                    return out;
                };
                // The tail is the console's one foreground command, and the
                // console's shell runs in main's context wherever it was typed.
                // An instance's shell runs detached and owns no row here (D84).
                if to.is_main() {
                    send(
                        &to,
                        UiEvent::BashTail(crate::live::LiveTail {
                            lines: updated.tail.lines.clone(),
                            total_lines: updated.tail.total_lines as usize,
                        }),
                    );
                }
            }
            // A warning a run raised. The console has one shared warning tier, so
            // an unattributed "Reconnecting…" about somebody else's stream would
            // read as its own — the name comes off the conversation the core
            // named, because which page a warning is about is the core's answer
            // and how to spell it is the console's (D134a).
            AppEventPayload::FeedbackRaised(raised) => {
                let message = match raised
                    .feedback
                    .conversation_id
                    .as_ref()
                    .and_then(|id| self.key_of(id))
                    .filter(|key| !key.is_main())
                {
                    Some(key) => format!("{} · {}", key.title(), raised.feedback.message),
                    None => raised.feedback.message.clone(),
                };
                send(&crate::ui::ConvKey::Main, UiEvent::Warning(message));
            }
            AppEventPayload::ItemCompleted(changed) => {
                let Some(to) = self.key_of(&changed.conversation_id) else {
                    return out;
                };
                self.read_item(&to, &changed.item, &mut send);
            }
            _ => {}
        }
        out
    }

    /// One completed item, as rows.
    fn read_item(
        &self,
        to: &crate::ui::ConvKey,
        item: &crate::app::snapshot::Item,
        send: &mut impl FnMut(&crate::ui::ConvKey, UiEvent),
    ) {
        match &item.body {
            ItemBody::ToolCall {
                tool_call_id,
                name,
                summary,
                output,
                duration_ms,
                diff,
                ..
            } => send(
                to,
                UiEvent::ToolDone(ToolCallDone {
                    tool_call_id: tool_call_id.clone(),
                    name: name.clone(),
                    summary: summary.clone(),
                    output: output.clone(),
                    status: match item.status {
                        ItemStatus::Failed => ToolCallStatus::Error,
                        ItemStatus::Cancelled => ToolCallStatus::Interrupted,
                        _ => ToolCallStatus::Done,
                    },
                    diff: diff.clone(),
                    duration_ms: *duration_ms,
                }),
            ),
            ItemBody::Interruption { marker } => {
                send(to, UiEvent::Interrupted(marker.clone()));
            }
            // A message that arrived from somebody else. The private lane's
            // target is the sender's own page, and the console draws the arrival
            // on the page it landed on.
            ItemBody::PeerMessage { from, text, .. } if !to.is_main() => send(
                to,
                UiEvent::Mail {
                    from: from.clone(),
                    text: text.clone(),
                },
            ),
            _ => {}
        }
    }

    /// The console's key for a conversation the core named.
    fn key_of(&self, id: &crate::app::ids::ConversationId) -> Option<crate::ui::ConvKey> {
        self.store.view().key_of(id).cloned()
    }

    /// The running output estimate for this page's round, in tokens.
    ///
    /// The provider's own count replaces it whenever one arrives; until then it
    /// is what the footer's rate meter has to read, because a round that is
    /// still streaming has no authoritative number yet.
    fn count(&mut self, to: &crate::ui::ConvKey, text: &str) -> u64 {
        let units = self.stream_units.entry(to.clone()).or_default();
        *units = units.saturating_add(crate::compact::text_units(text));
        units.div_ceil(4)
    }
}

/// Whether this call is the console's own `!` line rather than one the model
/// made.
///
/// A standalone call belongs to no fold group — the output is shown directly,
/// the way `BashModeProgress` shows it. `query::run_bash_command` mints the
/// identifier, and the shape of it is the fact that travels: the contract
/// carries the call's id, and the id says who made the call.
fn standalone_shell(tool_call_id: &str) -> bool {
    tool_call_id.starts_with(crate::query::BASH_CALL_PREFIX)
}
