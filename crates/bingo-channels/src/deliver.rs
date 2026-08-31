//! Frames in, delivery ops out (ADR-0016 §2).
//!
//! The reducer holds no session state — that is `SessionState`'s, folded by
//! the caller — and does no I/O. What it does hold is the one thing the
//! journal cannot tell it: what the platform has already been shown. From the
//! difference between the two come five ops, and from the dual gate comes how
//! often they are emitted.
//!
//! Streaming is always `Replace` with the whole answer, never a delta: an
//! adapter that prefers deltas can diff for itself, while an adapter handed a
//! delta it cannot apply has lost the text for good.

use std::time::Instant;

use bingo_sdk::{
    Answer, Event, Frame, Interaction, InteractionId, InteractionKind, ItemBody, Level, ResolvedBy,
    SessionState, TurnId, TurnStatus,
};

use crate::gate::Gate;
use crate::limits::Limits;
use crate::question::{Question, ladder, withdrawn};

/// What a conversation is asked to do next.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// A message begins here; the answer streams into it.
    Open,
    /// The whole answer so far — never a delta.
    Replace { full: String },
    /// That message is finished. `question` is the one that stopped it.
    Finalize {
        text: String,
        question: Option<Question>,
    },
    /// A line beside the answer: a failure, an interruption, a notice.
    Status { text: String },
    /// A question this conversation showed is settled, wherever from.
    Resolved {
        question: InteractionId,
        outcome: String,
    },
}

/// The message being streamed into: what the platform has, what is held back,
/// and when it last changed. One pending snapshot, replaced by newer text and
/// drained before the message is finalized.
#[derive(Debug)]
struct Streaming {
    sent: String,
    pending: Option<String>,
    last: Instant,
}

impl Streaming {
    fn new(text: String, now: Instant) -> Self {
        Self {
            sent: text,
            pending: None,
            last: now,
        }
    }

    /// The text the platform should end up with, whether or not it has it yet.
    fn latest(&self) -> &str {
        self.pending.as_ref().unwrap_or(&self.sent)
    }

    /// Newer text: shown now if the gate opens, else held as the one pending
    /// snapshot, replacing whatever was held before.
    fn offer(&mut self, text: String, gate: &Gate, now: Instant) -> Option<String> {
        if text == self.sent {
            return None;
        }
        if !gate.opens(&self.sent, &text, now.saturating_duration_since(self.last)) {
            self.pending = Some(text);
            return None;
        }
        self.flush(text, now)
    }

    fn flush(&mut self, text: String, now: Instant) -> Option<String> {
        self.sent = text;
        self.pending = None;
        self.last = now;
        Some(self.sent.clone())
    }

    /// When the timer would show what is held, if anything is.
    fn due(&self, gate: &Gate) -> Option<Instant> {
        self.pending.as_ref().map(|_| self.last + gate.interval)
    }
}

pub struct Deliverer {
    limits: Limits,
    gate: Gate,
    /// This surface's own client name, so a decision made in this chat is not
    /// reported as having been made somewhere else.
    here: String,
    turn: Option<TurnId>,
    /// The raw text of this turn that earlier messages already carry.
    delivered: String,
    streaming: Option<Streaming>,
    /// The questions this conversation showed and nobody has settled.
    asked: Vec<Question>,
}

impl std::fmt::Debug for Deliverer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Deliverer")
            .field("turn", &self.turn)
            .field("streaming", &self.streaming.is_some())
            .field("asked", &self.asked.len())
            .finish_non_exhaustive()
    }
}

impl Deliverer {
    pub fn new(limits: Limits, gate: Gate, here: impl Into<String>) -> Self {
        Self {
            limits,
            gate,
            here: here.into(),
            turn: None,
            delivered: String::new(),
            streaming: None,
            asked: Vec::new(),
        }
    }

    /// One frame, already folded into `state` by the caller. For a frame from
    /// a sub-session `state` is that sub-session's, which is why nothing but
    /// the root's own turn is ever read out of it.
    pub fn apply(&mut self, frame: &Frame, state: &SessionState, now: Instant) -> Vec<Op> {
        match &frame.event {
            Event::TurnStarted { turn, .. } => self.started(turn.clone()),
            Event::ItemStarted { .. }
            | Event::ItemUpdated { .. }
            | Event::ItemCompleted { .. }
            | Event::ItemDelta { .. } => self.grew(state, now),
            Event::InteractionOpened { interaction } => self.opened(interaction),
            Event::InteractionResolved { id, answer, by } => self.settled(id, answer, by),
            Event::InteractionCancelled { id, reason } => {
                self.close(id, withdrawn(reason)).into_iter().collect()
            }
            Event::TurnCompleted { status, .. } => self.completed(state, status),
            Event::Notice { level, text, .. } if *level != Level::Info => vec![self.status(text)],
            _ => Vec::new(),
        }
    }

    /// The timer fired: whatever was held back, now.
    pub fn tick(&mut self, now: Instant) -> Vec<Op> {
        let gate = self.gate;
        let flushed = self.streaming.as_mut().and_then(|streaming| {
            let pending = streaming.pending.clone()?;
            let waited = now.saturating_duration_since(streaming.last) >= gate.interval;
            waited.then(|| streaming.flush(pending, now))?
        });
        flushed.map(|full| self.replace(full)).into_iter().collect()
    }

    /// When `tick` is worth calling.
    pub fn due(&self) -> Option<Instant> {
        self.streaming.as_ref().and_then(|s| s.due(&self.gate))
    }

    /// The question this conversation is showing, by id.
    pub fn question(&self, id: &InteractionId) -> Option<&Question> {
        self.asked.iter().find(|q| &q.id == id)
    }

    fn started(&mut self, turn: TurnId) -> Vec<Op> {
        self.turn = Some(turn);
        self.delivered.clear();
        self.streaming = None;
        Vec::new()
    }

    /// The answer grew. A message opens on the first words of it and is
    /// replaced from then on, as often as the gate allows.
    fn grew(&mut self, state: &SessionState, now: Instant) -> Vec<Op> {
        let Some(fresh) = self.fresh(state) else {
            return Vec::new();
        };
        let gate = self.gate;
        let (opened, flushed) = match self.streaming.as_mut() {
            Some(streaming) => (false, streaming.offer(fresh, &gate, now)),
            None => {
                self.streaming = Some(Streaming::new(fresh.clone(), now));
                (true, Some(fresh))
            }
        };
        let mut ops = Vec::new();
        if opened {
            ops.push(Op::Open);
        }
        if let Some(full) = flushed {
            ops.push(self.replace(full));
        }
        ops
    }

    /// A question stops the stream: a live message and a live button on the
    /// same card is what the platforms will not do (ADR-0016 §5 of the wire
    /// notes), and a person answering should not be watching text move.
    fn opened(&mut self, interaction: &Interaction) -> Vec<Op> {
        let Some(question) = ladder(interaction) else {
            return vec![self.status(&unanswerable(interaction))];
        };
        self.asked.push(question.clone());
        let text = self.drain();
        vec![Op::Finalize {
            text,
            question: Some(question),
        }]
    }

    fn settled(&mut self, id: &InteractionId, answer: &Answer, by: &ResolvedBy) -> Vec<Op> {
        let here = self.here.clone();
        let outcome = self
            .question(id)
            .map(|question| question.outcome(answer, by, &here));
        outcome
            .and_then(|outcome| self.close(id, outcome))
            .into_iter()
            .collect()
    }

    fn close(&mut self, id: &InteractionId, outcome: String) -> Option<Op> {
        let known = self.asked.iter().any(|q| &q.id == id);
        self.asked.retain(|q| &q.id != id);
        known.then(|| Op::Resolved {
            question: id.clone(),
            outcome,
        })
    }

    /// The turn ended: the pending snapshot is drained and the authoritative
    /// text — the one the completed items carry — is the last word.
    fn completed(&mut self, state: &SessionState, status: &TurnStatus) -> Vec<Op> {
        let mut ops = Vec::new();
        let fresh = self.fresh(state);
        if self.streaming.is_some() || fresh.is_some() {
            let text = fresh.unwrap_or_else(|| self.drain_raw());
            self.delivered.push_str(&text);
            self.streaming = None;
            ops.push(Op::Finalize {
                text: self.laid_out(&text),
                question: None,
            });
        }
        if let Some(text) = ended(status) {
            ops.push(self.status(&text));
        }
        self.turn = None;
        ops
    }

    /// The raw text of this turn the platform has not been shown yet.
    fn fresh(&self, state: &SessionState) -> Option<String> {
        let turn = self.turn.as_ref()?;
        let full = answer(state, turn);
        let fresh = full.strip_prefix(&self.delivered).unwrap_or(&full);
        (!fresh.trim().is_empty()).then(|| fresh.to_string())
    }

    /// Close the live message, counting its text as delivered.
    fn drain(&mut self) -> String {
        let raw = self.drain_raw();
        self.delivered.push_str(&raw);
        self.laid_out(&raw)
    }

    fn drain_raw(&mut self) -> String {
        self.streaming
            .take()
            .map(|streaming| streaming.latest().to_string())
            .unwrap_or_default()
    }

    fn replace(&self, raw: String) -> Op {
        Op::Replace {
            full: self.laid_out(&raw),
        }
    }

    fn status(&self, text: &str) -> Op {
        Op::Status {
            text: self.laid_out(text),
        }
    }

    /// The one place the platform's dialect and length are applied, so every
    /// adapter is handed text it can already carry.
    fn laid_out(&self, raw: &str) -> String {
        self.limits
            .clip(&self.limits.dialect.render(raw.trim()))
            .into_owned()
    }
}

/// This turn's assistant prose, in transcript order. Reasoning and tool calls
/// are not what a chat came for; a failure reaches it as a `Status`.
fn answer(state: &SessionState, turn: &TurnId) -> String {
    state
        .items
        .iter()
        .filter(|item| item.turn.as_ref() == Some(turn))
        .filter_map(|item| match &item.body {
            ItemBody::Assistant { text } if !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// What a turn's end is worth saying, if anything.
fn ended(status: &TurnStatus) -> Option<String> {
    match status {
        TurnStatus::Completed => None,
        TurnStatus::Failed { error } => Some(error.message.clone()),
        TurnStatus::Interrupted { reason } => {
            Some(format!("the turn was interrupted ({reason:?})"))
        }
    }
}

/// A question with no rung in a chat is still worth showing: somebody at
/// another surface has to answer it, and this one says so rather than going
/// quiet (ADR-0013's degrade, one level up).
fn unanswerable(interaction: &Interaction) -> String {
    match &interaction.kind {
        InteractionKind::Login { provider, .. } => {
            format!("{provider} is asking to be signed in; that has to happen where bingo runs")
        }
        _ => "a question is waiting at another surface".into(),
    }
}

#[cfg(test)]
mod tests;
