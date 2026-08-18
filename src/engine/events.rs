//! What a run reports, and who answers what it cannot answer itself.
//!
//! These are the two halves of the old `UiHooks`, split because they were never
//! one thing:
//!
//! - [`EngineEvent`] is the report. It travels one way, it is complete, and what
//!   a receiver does with a field — render it, count it, drop it — is the
//!   receiver's business. The engine no longer decides that a frontend "only
//!   needs the text": a reasoning delta, a tool's argument JSON, and the
//!   provider's own output-token count all cross this boundary intact.
//! - [`EngineRequests`] is the question. Each of its four handles blocks a run
//!   on an answer produced somewhere else — a permission modal, the composer, a
//!   terminal surface — so they stay call-and-answer rather than becoming
//!   variants on the one-way channel. Putting them there would mean a oneshot in
//!   every variant plus a loop to service it, which is what the actor does in B3
//!   when interactions become `interaction/respond`; inventing a smaller version
//!   of that now would be a second answer to the same question.
//!
//! `EngineEvent` is the layer below `AppEvent`: the actor turns these into
//! sequenced application state (B2b/B3). Until it does, the frontends translate
//! them into `UiEvent` at the three adapters, which are marked as the shims they
//! are.

use std::sync::Arc;

use crate::api::contract::StreamEvent;
use crate::query::{AskFn, AskQuestionFn, SteerFn, ToolCallDone};

/// One thing that happened inside a run.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// Prose from the model. `index` is the content block it belongs to.
    TextDelta {
        index: usize,
        text: String,
    },
    /// Reasoning from the model, which is not prose and is never spliced into
    /// it.
    ThinkingDelta {
        index: usize,
        thinking: String,
    },
    /// A tool call's arguments arriving in pieces. No surface renders it, and it
    /// still travels: it is the only sign of work while the model is otherwise
    /// silent, so a receiver that wants an honest token count needs it.
    ToolInputDelta {
        index: usize,
        partial_json: String,
    },
    /// The model started calling a tool. The arguments are not complete yet;
    /// [`EngineEvent::ToolReady`] carries them when they are. The block index and
    /// the call's id travel because the report is complete: today's two frontends
    /// render the name alone, and B3's item model addresses the call by id.
    ToolUseStarted {
        #[allow(dead_code)]
        index: usize,
        #[allow(dead_code)]
        id: String,
        name: String,
    },
    /// One model response ended. `output_tokens` is the provider's own count and
    /// outranks any local estimate.
    StopReason {
        /// Why the model stopped. `query_loop` branches on its own copy of this;
        /// it travels so a frontend can say "cut off at the output limit"
        /// without inferring it from the text.
        #[allow(dead_code)]
        stop_reason: Option<String>,
        output_tokens: Option<u64>,
    },
    /// The stream failed and is being retried from the last committed round.
    /// Everything the failed attempt produced is withdrawn.
    ///
    /// It travels on every attempt, including one that failed before saying
    /// anything: the attempt happened, and a client counting them must not be
    /// short one. `discarded_output` is what says whether there is a live tail to
    /// withdraw — only a surface that drew something has anything to undo.
    StreamRetry {
        /// Attempts made so far, including the one that just failed.
        attempt: u32,
        max_attempts: u32,
        /// How long the run waits before the next attempt.
        delay_ms: u64,
        discarded_output: bool,
        code: Option<String>,
        reason: Option<String>,
    },
    /// The turn's context measurement: used tokens, the window the footer shows,
    /// and the trigger the compactor obeys. One measurement travels — a receiver
    /// that recomputed any of them from its own model handle would be a second
    /// ruler.
    ContextUsage(crate::context_usage::ContextUsage),
    /// A tool call is complete down to its input, which the fold decision needs.
    /// `standalone` marks a call the model did not make (the `!` command), which
    /// belongs to no batch.
    ToolReady {
        tool_call_id: String,
        name: String,
        input: serde_json::Value,
        standalone: bool,
    },
    ToolDone(ToolCallDone),
    /// One model response and all of its tools finished.
    RoundEnd,
    /// Something went wrong that did not end the run.
    Warning(String),
    /// Prose entering this conversation from outside its own turn: the prompt a
    /// run opens with, and every batch of mail a running instance absorbs at a
    /// round boundary. Without it an agent's page would show the answers and
    /// never the questions.
    Inbound(String),
}

impl EngineEvent {
    /// The application-level reading of one provider stream event.
    ///
    /// `None` for the stream's own scaffolding — block starts and stops,
    /// signatures, the message envelope, the terminator. Those describe how the
    /// bytes were framed, not what happened in the run; the accumulator in
    /// `query_turn` is their reader, and an error is raised as an error rather
    /// than reported as progress.
    pub fn from_stream(event: &StreamEvent) -> Option<Self> {
        match event {
            StreamEvent::TextDelta { index, text } => Some(Self::TextDelta {
                index: *index,
                text: text.clone(),
            }),
            StreamEvent::ThinkingDelta { index, thinking } => Some(Self::ThinkingDelta {
                index: *index,
                thinking: thinking.clone(),
            }),
            StreamEvent::InputJsonDelta {
                index,
                partial_json,
            } => Some(Self::ToolInputDelta {
                index: *index,
                partial_json: partial_json.clone(),
            }),
            StreamEvent::ToolUseStart { index, id, name } => Some(Self::ToolUseStarted {
                index: *index,
                id: id.clone(),
                name: name.clone(),
            }),
            StreamEvent::StopReason {
                stop_reason,
                output_tokens,
            } => Some(Self::StopReason {
                stop_reason: stop_reason.clone(),
                output_tokens: *output_tokens,
            }),
            StreamEvent::MessageStart { .. }
            | StreamEvent::TextStart { .. }
            | StreamEvent::ThinkingStart { .. }
            | StreamEvent::SignatureDelta { .. }
            | StreamEvent::BlockStop { .. }
            | StreamEvent::Done
            | StreamEvent::ApiError { .. } => None,
        }
    }
}

/// Where a run's events go.
///
/// Two places, and in this order: the session actor, which is where the report
/// becomes application state, and then the frontend sink it was written against.
/// The actor goes first because it is the ordering point — what it sequences is
/// what happened — and the sink is a shim the terminal front end still needs
/// until B7 reads `AppFrame` instead.
///
/// The sink is a plain function rather than a channel: the engine calls it where
/// the thing happened, so an event cannot be reordered against anything the same
/// task sends by another route.
#[derive(Clone)]
pub struct EngineEvents {
    sink: Arc<dyn Fn(EngineEvent) + Send + Sync>,
    /// The turn this run reports into. `None` for a run with no session behind
    /// it: a headless `--print`, an embedded call, a test.
    turn: Option<RunBinding>,
}

/// What ties one run to the turn it reports into.
#[derive(Clone)]
pub struct RunBinding {
    turns: crate::app::turn::TurnHandle,
    turn: crate::app::ids::TurnId,
}

impl EngineEvents {
    pub fn new(sink: impl Fn(EngineEvent) + Send + Sync + 'static) -> Self {
        Self {
            sink: Arc::new(sink),
            turn: None,
        }
    }

    /// A sink for a run nobody is watching. Production always attaches one;
    /// this is what a test uses when the run's reports are not what it asserts.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn detached() -> Self {
        Self::new(|_| {})
    }

    pub fn emit(&self, event: EngineEvent) {
        if let Some(binding) = &self.turn {
            binding
                .turns
                .report_event(binding.turn.clone(), event.clone());
        }
        (self.sink)(event);
    }

    /// Report one provider stream event, if it says anything at this layer.
    pub fn emit_stream(&self, event: &StreamEvent) {
        if let Some(event) = EngineEvent::from_stream(event) {
            self.emit(event);
        }
    }

    pub fn warn(&self, message: impl Into<String>) {
        self.emit(EngineEvent::Warning(message.into()));
    }

    /// The warning callback tool assembly and the compactor were written
    /// against. They take a callback because they are called from outside a run
    /// as well; a shim, and B3 removes this when they move behind the actor.
    pub fn warn_sink(&self) -> impl Fn(String) + Send + 'static + use<> {
        let sink = self.sink.clone();
        move |message| (sink)(EngineEvent::Warning(message))
    }
}

impl std::fmt::Debug for EngineEvents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EngineEvents")
    }
}

/// Who answers what a run cannot answer itself.
#[derive(Clone)]
pub struct EngineRequests {
    /// Permission prompt: one pending request → the user's verdict.
    pub ask: Arc<AskFn>,
    /// AskUserQuestion: the model's question → the user's answer.
    pub ask_question: Arc<AskQuestionFn>,
    /// Mid-turn steering (D83): what the composer has queued for the turn that
    /// is running right now, taken at each tool barrier.
    pub steer: Arc<SteerFn>,
    /// Foreground liveness (D84): the running shell command's output tail, and
    /// the key that moves it to the background.
    pub live: Arc<crate::live::LiveBash>,
}

/// What hosts one run: where its events go, and who answers its questions.
///
/// One host, one run. That was a convention until B3 and is a fact now: a host
/// bound to a turn claims it at the start of the run, and the actor refuses the
/// second claim — two runs reporting into one turn's item stream would interleave
/// two attempts into one history.
#[derive(Clone)]
pub struct EngineHost {
    pub events: EngineEvents,
    pub requests: EngineRequests,
    turn: Option<RunBinding>,
}

impl EngineHost {
    pub fn new(events: EngineEvents, requests: EngineRequests) -> Self {
        Self {
            events,
            requests,
            turn: None,
        }
    }

    /// Bind this host to the turn its run reports into.
    pub fn bound(
        mut self,
        turns: crate::app::turn::TurnHandle,
        turn: crate::app::ids::TurnId,
    ) -> Self {
        let binding = RunBinding { turns, turn };
        self.events.turn = Some(binding.clone());
        self.turn = Some(binding);
        self
    }

    /// Take this host's one run. `false` means the turn already has one, and the
    /// caller must not start a second.
    pub async fn begin_run(&self) -> bool {
        match &self.turn {
            // An unbound host reports to nobody, so there is no shared turn state
            // for a second run to corrupt: the claim is the actor's to enforce,
            // and there is no actor here.
            None => true,
            Some(binding) => binding.turns.claim(binding.turn.clone()).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn the_stream_crosses_the_boundary_whole() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        let events = EngineEvents::new(move |event| {
            recorder
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(event);
        });
        events.emit_stream(&StreamEvent::ThinkingDelta {
            index: 2,
            thinking: "weighing it".to_string(),
        });
        events.emit_stream(&StreamEvent::InputJsonDelta {
            index: 3,
            partial_json: "{\"path\":".to_string(),
        });
        events.emit_stream(&StreamEvent::StopReason {
            stop_reason: Some("max_tokens".to_string()),
            output_tokens: Some(4096),
        });

        let seen = seen.lock().unwrap_or_else(|error| error.into_inner());
        match &seen[..] {
            [
                EngineEvent::ThinkingDelta { index, thinking },
                EngineEvent::ToolInputDelta {
                    index: json_index,
                    partial_json,
                },
                EngineEvent::StopReason {
                    stop_reason,
                    output_tokens,
                },
            ] => {
                assert_eq!((*index, thinking.as_str()), (2, "weighing it"));
                assert_eq!((*json_index, partial_json.as_str()), (3, "{\"path\":"));
                assert_eq!(stop_reason.as_deref(), Some("max_tokens"));
                assert_eq!(
                    *output_tokens,
                    Some(4096),
                    "the provider's own count crosses; estimating it again downstream is a \
                     second ruler"
                );
            }
            other => panic!("the reasoning, the argument JSON and the count all travel: {other:?}"),
        }
    }

    /// Framing is not progress: the events that describe how the bytes arrived
    /// stop at the accumulator that reads them.
    #[test]
    fn stream_scaffolding_is_not_an_engine_event() {
        for event in [
            StreamEvent::MessageStart {
                id: "msg_1".to_string(),
                model: "sonnet".to_string(),
            },
            StreamEvent::TextStart { index: 0 },
            StreamEvent::ThinkingStart { index: 0 },
            StreamEvent::SignatureDelta {
                index: 0,
                signature: "sig".to_string(),
            },
            StreamEvent::BlockStop { index: 0 },
            StreamEvent::Done,
        ] {
            assert!(
                EngineEvent::from_stream(&event).is_none(),
                "{event:?} describes the frame, not the run"
            );
        }
    }

    #[test]
    fn a_detached_sink_drops_everything_and_says_nothing() {
        let events = EngineEvents::detached();
        events.warn("nobody is listening");
        events.emit(EngineEvent::RoundEnd);
    }
}
