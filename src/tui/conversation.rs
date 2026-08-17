//! One conversation's transcript and the turn writing to it.

use super::chat::{QueuedInput, UiMessage};

/// One conversation: the transcript, and the turn producing it.
///
/// The line is drawn at *what there is one of*. The console has one terminal,
/// one composer, one input history, one theme, one tick — those stay on
/// [`Chat`](super::chat::Chat). A conversation has one transcript and at most
/// one turn writing into it, and every field here is either that transcript or
/// something the turn is doing to it: where the stream is writing, what it has
/// thought, what it has spent, when it started, what is queued behind it.
///
/// `Chat` holds exactly one — the conversation the screen is on. That is the
/// whole point of the split: main is a privileged set of fields on the console
/// today and every other conversation reaches the screen as a projection
/// rebuilt per frame, which is why the two keep disagreeing (D130 lost the tool
/// results, D132 drew the running turn twice, and the composer's `/`, `!` and
/// `@name` are still dead on an agent page). D134 makes this N, keyed by
/// conversation and fed by the same event channel; an agent's page is then
/// main's page pointed at a different one of these.
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
}
