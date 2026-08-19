//! What a keystroke asked the core for, and when the answer is folded in.
//!
//! A key handler cannot wait. It runs on the render thread, and the frame it
//! belongs to is drawn the moment it returns — so every write the console makes
//! to the core used to block that thread on the actor's reply
//! ([`crate::app::answer::Answer::now`]). Bounded, and still the wrong shape: the
//! console is an attachment, and an attachment asks.
//!
//! So a key handler *records* what it asked for and returns. The loop performs
//! the intents it left, `.await`ing each, and folds what came back — all before
//! the next frame is assembled ([`super::Chat::settle_intents`]). That is the
//! whole difference: **this is folding, not delay**. Nothing a user can see
//! happens a frame later than it used to; what changed is that the thread that
//! draws does not stop while the actor answers.
//!
//! A test has no loop, so the test *is* the loop: [`super::Chat::intend`]
//! settles on the spot under `cfg(test)`, exactly as
//! [`super::Chat::settle_store`] takes the store's fold synchronously.

use std::time::Instant;

use crate::app::command::{Action, ComposerMode};
use crate::app::ids::InteractionId;
use crate::app::snapshot::{ActivationKind, InteractionCancelReason, InteractionDecision};
use crate::permission::PermissionMode;
use crate::ui::ConvKey;

/// One thing the console asked the core to do.
#[derive(Debug)]
pub(crate) enum Intent {
    /// One composer line, as the core reads it.
    Submit {
        /// The line as it was typed, envelope and all: the input history keeps
        /// what was typed, not what was delivered.
        raw: String,
        /// The same line with the terminal's own shorthand resolved.
        text: String,
        mode: ComposerMode,
        on: ConvKey,
        carries_attachments: bool,
    },
    /// Prose the console submits on the user's behalf: an error screen's retry,
    /// a skill invocation's marker.
    Resubmit(String),
    /// One action, applied by the core's own table.
    Execute(Box<Action>),
    /// A finished background run left a notification in main's context.
    Wake,
    /// The mail debounce, asked once a frame.
    Digest,
    /// Whatever reached main's inbox since the last look, for the sender dots.
    Arrivals,
    /// Join or leave a room, as the user.
    Room { name: String, join: bool },
    /// Stop one instance.
    StopAgent(String),
    /// Cycle one instance's own permission mode.
    AgentMode { name: String, next: PermissionMode },
    /// Pull the last queued line back into the composer.
    ReclaimTail(ConvKey),
    /// Close the prompts that can no longer be answered.
    CancelAsks { dead_only: bool },
    /// Answer the prompt on screen, with the receipt to leave if the core takes
    /// it. The receipt travels with the answer because it is only earned by an
    /// answer the core accepted — D81's guard refuses one that came too fast,
    /// and a receipt for it would be a lie.
    AnswerAsk {
        id: InteractionId,
        activation: ActivationKind,
        at: Instant,
        decision: InteractionDecision,
        receipt: Option<String>,
    },
}

/// How many fold rounds one settle is allowed.
///
/// Performing an intent can leave another — a wake the core's own stream asked
/// for, most often — and each round is bounded work, so a handful of rounds is
/// the difference between "settled" and a loop nobody can end.
const ROUNDS: usize = 8;

impl super::Chat {
    /// Record what this keystroke asked for.
    pub(crate) fn intend(&mut self, intent: Intent) {
        self.intents.push_back(intent);
        self.dirty = true;
        // A test has no loop to perform it, so the test is the loop.
        #[cfg(test)]
        self.settle_intents_now();
    }

    /// The same, at most once per settle, and without marking the screen dirty:
    /// the frame's own polls are asked every frame, answer one question, and
    /// change nothing by themselves. A second copy would only ask it twice, and
    /// a repaint for asking would make an idle console never rest.
    pub(crate) fn intend_once(&mut self, intent: Intent) {
        let already = std::mem::discriminant(&intent);
        if self
            .intents
            .iter()
            .any(|held| std::mem::discriminant(held) == already)
        {
            return;
        }
        self.intents.push_back(intent);
        #[cfg(test)]
        self.settle_intents_now();
    }

    /// Perform everything the frame asked for, and fold what came back.
    ///
    /// Returns whether the screen moved. Re-entrant calls answer `false` rather
    /// than draining a queue somebody else is already draining: the fold routes
    /// events, and routing one can leave an intent behind.
    pub async fn settle_intents(&mut self) -> bool {
        if self.draining {
            return false;
        }
        self.draining = true;
        let mut moved = false;
        for _ in 0..ROUNDS {
            while let Some(intent) = self.intents.pop_front() {
                self.perform(intent).await;
            }
            self.pump_store();
            moved |= self.drain_frames();
            if self.intents.is_empty() {
                break;
            }
        }
        self.draining = false;
        moved
    }

    /// The same, on a thread with no runtime under it: what a test uses, and the
    /// reason `Answer::now` still exists at all.
    #[cfg(test)]
    pub(crate) fn settle_intents_now(&mut self) {
        if self.draining {
            return;
        }
        crate::app::answer::block_on(self.settle_intents());
    }

    /// One intent, performed against the core, and the console's own half of
    /// what came back.
    async fn perform(&mut self, intent: Intent) {
        match intent {
            Intent::Submit {
                raw,
                text,
                mode,
                on,
                carries_attachments,
            } => {
                let performed = self
                    .session
                    .submit
                    .submit(crate::app::submit::SubmitRequest {
                        conversation: on,
                        input: crate::app::command::Submission::Composer {
                            mode,
                            text: text.clone(),
                            attachments: Vec::new(),
                        },
                        carries_attachments,
                    })
                    .await;
                self.drew(performed, raw, text);
            }
            Intent::Resubmit(text) => {
                let performed = self
                    .session
                    .submit
                    .submit(crate::app::submit::SubmitRequest {
                        conversation: ConvKey::Main,
                        input: crate::app::command::Submission::SendProse {
                            text: text.clone(),
                            attachments: Vec::new(),
                        },
                        carries_attachments: !self.resolve_images(&text).is_empty(),
                    })
                    .await;
                if matches!(performed, crate::app::submit::Performed::Turn { .. }) {
                    self.last_prompt = text;
                }
                self.dirty = true;
            }
            Intent::Execute(action) => {
                let _ = self.session.core.execute(ConvKey::Main, *action).await;
            }
            Intent::Wake => {
                let interrupted = self.main_conv().interrupted;
                let _ = self.session.mail.notified(interrupted).await;
            }
            Intent::Digest => {
                // Taken rather than read, and before anything else: the drain and
                // the bell are separate readers, and a turn already running can
                // absorb the message before this ever sees the mail that asked
                // for the ring.
                if self.session.channels.take_main_mail_urgent().await {
                    self.notify
                        .attention(crate::tui::notify::Attention::AgentNotice);
                }
                let interrupted = self.main_conv().interrupted;
                self.digest_woke = self.session.mail.digest(interrupted).await;
            }
            Intent::Arrivals => {
                for arrival in self.session.channels.drain_main_arrivals().await {
                    *self.agent_mail.entry(arrival.from).or_insert(0) += 1;
                    self.dirty = true;
                }
            }
            Intent::Room { name, join } => {
                let outcome = if join {
                    self.session
                        .channels
                        .invite(&name, crate::channels::USER_NAME)
                        .await
                } else {
                    self.session
                        .channels
                        .kick(&name, crate::channels::USER_NAME)
                        .await
                };
                match outcome {
                    Ok(()) => {
                        self.refresh_conversations();
                        self.push_slash_info(if join {
                            format!("joined #{name}")
                        } else {
                            format!("left #{name}")
                        });
                    }
                    Err(why) => self.push_slash_info(why),
                }
            }
            Intent::StopAgent(name) => {
                let stopped = self.session.agents.stop(&name).await;
                self.stopped_agent(&name, stopped);
            }
            Intent::AgentMode { name, next } => {
                self.session.agents.set_permission_mode(&name, next).await;
                self.dirty = true;
            }
            Intent::ReclaimTail(on) => {
                // The turn may take this one first (D83). Whichever reached the
                // actor first wins, and a pull-back that lost is a no-op: the
                // text is in the request by then, so bringing it back into the
                // composer would send it twice.
                if let crate::app::queue::Reclaim::Pulled(entry) =
                    self.session.queue.reclaim_tail(on).await
                {
                    self.set_input(entry.text);
                }
                self.dirty = true;
            }
            Intent::CancelAsks { dead_only } => {
                let reason = if dead_only {
                    InteractionCancelReason::TurnEnded
                } else {
                    InteractionCancelReason::Interrupted
                };
                if self
                    .session
                    .interactions
                    .cancel_all(reason, dead_only)
                    .await
                {
                    self.asks_cancelled();
                }
            }
            Intent::AnswerAsk {
                id,
                activation,
                at,
                decision,
                receipt,
            } => {
                if self
                    .session
                    .interactions
                    .respond_at(id, activation, at, decision)
                    .await
                    .is_ok()
                {
                    self.ask_answered(receipt);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::chat_at;

    /// The frame's own polls are asked once per settle, and asking is not a
    /// reason to repaint: an idle console that queued a poll every frame would
    /// never rest.
    #[test]
    fn a_frame_poll_queues_once_and_leaves_the_screen_alone() {
        let mut chat = chat_at(80, 24);
        chat.dirty = false;
        // Held open so the queue can be read: a test settles on the spot, which
        // is exactly what makes the queue invisible everywhere else.
        chat.draining = true;
        chat.intend_once(Intent::Digest);
        chat.intend_once(Intent::Digest);
        chat.intend_once(Intent::Arrivals);
        assert_eq!(chat.intents.len(), 2, "one of each, however often asked");
        assert!(!chat.dirty, "asking changes nothing on screen");
        chat.draining = false;
        chat.settle_intents_now();
        assert!(chat.intents.is_empty(), "and the settle empties it");
    }

    /// Folding, not delay: the line is recorded by the key handler and its rows
    /// are on the page by the time the caller looks — which is what the loop
    /// does one step later, before the frame.
    #[test]
    fn a_recorded_line_is_on_the_page_by_the_time_the_frame_is_built() {
        let mut chat = chat_at(80, 24);
        chat.set_input("run the tests");
        chat.submit();
        assert!(
            chat.intents.is_empty(),
            "the intent was performed rather than left waiting"
        );
        assert!(
            chat.conv
                .messages
                .iter()
                .any(|message| message.text == "run the tests"),
            "and the row the core recorded is drawn: {:?}",
            chat.conv
                .messages
                .iter()
                .map(|message| &message.text)
                .collect::<Vec<_>>()
        );
    }
}
