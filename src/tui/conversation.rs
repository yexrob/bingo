//! One conversation's transcript and the turn writing to it.

use super::chat::{QueuedInput, UiMessage};
use crate::tui::activities::{ActivityKind, ThinkingState};

/// One conversation: the transcript, and the turn producing it.
///
/// The line is drawn at *what there is one of*. The console has one terminal,
/// one composer, one input history, one theme, one tick — those stay on
/// [`Chat`](super::chat::Chat). A conversation has one transcript and at most
/// one turn writing into it, and every field here is either that transcript or
/// something the turn is doing to it: where the stream is writing, what it has
/// thought, what it has spent, when it started, what is queued behind it.
///
/// `Chat` holds N of these, keyed by [`crate::ui::ConvKey`], and points at one
/// (D134). Every one of them is fed the same way — `UiEvent`s addressed to it —
/// so an agent's page is main's page pointed at a different store, which is the
/// whole ruling. The projection that used to rebuild one per frame is gone with
/// the disagreements it produced (D130 lost the tool results, D132 drew the
/// running turn twice).
///
/// A room's store is the exception, and it is not a turn loop: nothing streams
/// into a room, so its transcript is filled by projecting the channel log
/// ([`crate::tui::conv::room_tail`]) and its turn fields stay at rest forever.
pub struct Conversation {
    pub messages: Vec<UiMessage>,
    /// Messages queued while busy (submitted one by one after TurnEnd, or absorbed
    /// earlier by the running turn through [`Chat::steer`](super::chat::Chat::steer)).
    pub queued: Vec<QueuedInput>,
    /// Id of the next queued entry.
    pub(crate) next_queue_id: u64,
    pub busy: bool,
    /// Esc/Ctrl+C interrupted the current turn: background-task completion no longer auto-starts
    /// a new turn (interrupt semantics: wait for the user to submit), reset in start_turn.
    pub interrupted: bool,
    /// Index of the current assistant message.
    pub stream_msg: Option<usize>,
    /// Current response-attempt start within the live message. Retrying restores this snapshot,
    /// preserving completed tool rounds even when the failed attempt mutated an existing group.
    pub(super) stream_attempt_checkpoint: Option<UiMessage>,
    /// Message opened by [`Chat::open_continuation_message`](super::chat::Chat::open_continuation_message)
    /// to carry what the model says after a mid-turn answer. Recorded so a turn that ends without
    /// using it can drop it again — inferring that from "empty assistant message" would also catch
    /// messages nobody opened here.
    pub(crate) continuation_msg: Option<usize>,
    /// Tool activity indices waiting to be classified on ToolReady (full input) (FIFO).
    ///
    /// They index into `messages[stream_msg]`, so they belong to the same
    /// conversation those two do: with a second stream on the channel, a
    /// `ToolReady` matched against a console-wide queue would classify one
    /// agent's call into another's transcript.
    pub(crate) pending_tools: Vec<usize>,
    pub(crate) thinking_buf: String,
    /// Whether the current thinking segment is open for continuation: closed after ToolStart/TextDelta
    /// (segment boundaries); deltas in the same segment continue without paragraph breaks; new segments (fresh reasoning after a tool) are aggregated with \n\n.
    pub(crate) thinking_seg_open: bool,
    pub(crate) output_tokens: u64,
    pub(super) output_round_tokens: u64,
    pub(super) token_rate: crate::token_rate::TokenRateSampler,
    pub(super) context_usage: crate::context_usage::ContextUsage,
    /// Tick at TurnStart: the relative timing baseline for running-state thinking.
    pub(super) turn_start_tick: u64,
    /// Real clock at TurnStart (baseline for the status-row elapsed time; cleared at TurnEnd).
    pub(super) turn_started: Option<std::time::Instant>,
    /// Tick the last turn ended on — the origin of the `settle` blink window.
    pub(crate) settle_at: Option<u64>,
    /// The running verb, pinned for the whole turn: a second reasoning segment
    /// used to re-roll it, so the status row changed its mind mid-thought.
    pub(crate) turn_verb: &'static str,
    /// How much of the source this store was projected from is already in
    /// `messages` — a room's log position. Zero and unread for a conversation
    /// fed by events, which is every conversation with a turn loop.
    pub(crate) projected: usize,
    /// Whether the task that created this instance is already in `messages`.
    ///
    /// The very first user text in an instance's record is *intake* — the job it
    /// was dispatched with — and every one after it is somebody talking to it
    /// ([`crate::tui::perspective::split_user_text`]). "First" cannot be read off
    /// the transcript, because `TurnStart` opens the turn's own message before
    /// the prompt arrives; a store that guessed from emptiness would file a
    /// spawn task as main speaking, and the same run would then render one way
    /// live and another way re-read from history.
    pub(crate) intake_seen: bool,
}

impl Conversation {
    /// An empty conversation, with the context window the console was built
    /// against.
    ///
    /// D133 deferred this deliberately: with one conversation there was nothing
    /// to construct twice. D134 opens one per page.
    pub(crate) fn new(context_usage: crate::context_usage::ContextUsage) -> Self {
        Self {
            messages: Vec::new(),
            queued: Vec::new(),
            next_queue_id: 0,
            busy: false,
            interrupted: false,
            stream_msg: None,
            stream_attempt_checkpoint: None,
            continuation_msg: None,
            pending_tools: Vec::new(),
            thinking_buf: String::new(),
            thinking_seg_open: false,
            output_tokens: 0,
            output_round_tokens: 0,
            token_rate: crate::token_rate::TokenRateSampler::default(),
            context_usage,
            turn_start_tick: 0,
            turn_started: None,
            settle_at: None,
            turn_verb: super::chat::THINKING_WORDS[0],
            projected: 0,
            intake_seen: false,
        }
    }

    pub(crate) fn pending_tools_clear(&mut self) {
        self.pending_tools.clear();
    }

    pub(crate) fn pending_tools_push(&mut self, idx: usize) {
        self.pending_tools.push(idx);
    }

    pub(crate) fn pending_tools_pop(&mut self) -> Option<usize> {
        let first = self.pending_tools.first().copied();
        if first.is_some() {
            self.pending_tools.remove(0);
        }
        first
    }

    /// A continuation message the turn never filled (the answer was the last thing that
    /// happened): an empty assistant block renders as a stray gap. Only ever drops the
    /// message [`crate::tui::chat::Chat::open_continuation_message`] opened. Call before
    /// clearing `stream_msg`.
    pub(crate) fn drop_empty_stream_message(&mut self) {
        let Some(i) = self.continuation_msg.take() else {
            return;
        };
        if self.stream_msg == Some(i)
            && i + 1 == self.messages.len()
            && self.messages[i].text.is_empty()
            && self.messages[i].activities.is_empty()
        {
            self.messages.pop();
            self.stream_msg = None;
            self.stream_attempt_checkpoint = None;
        }
    }

    /// A tool call, message text, or a mid-turn answer all end the current reasoning segment.
    pub(crate) fn close_running_thinking(&mut self, i: usize, tick: u64) {
        for hint in &mut self.messages[i].activities {
            if let ActivityKind::Thinking(t) = &mut hint.kind
                && t.state == ThinkingState::Running
            {
                t.state = ThinkingState::Done;
                t.duration_ms = tick
                    .saturating_sub(t.start_tick)
                    .saturating_mul(crate::tui::motion::TICK_MS);
            }
        }
    }
}
