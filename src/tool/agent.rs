use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;

use crate::agents::{
    AgentDef, AgentKind, AgentRegistry, FollowUp, InboxItem, MAX_FOLLOW_UPS, MsgId,
};
use crate::api::contract::SystemBlock;
use crate::api::types::Message;
use crate::channels::ChannelRegistry;
use crate::query::{Session, UiHooks};
use crate::tool::{Tool, ToolContext, ToolError, ToolResult, parse_input};
use crate::ui::UiEvent;
use crate::watch::{NotifyCondition, WatchId, WatchKind, WatchRegistry, WatchState};

use crate::tool::address::{self, Address};
use crate::tool::agent_notes::{CHANNEL_NOTE, SUBAGENT_NOTE};

const MAX_AGENT_DEPTH: usize = 3;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct AgentInput {
    #[schemars(description = "Independent task instructions for the subagent")]
    prompt: String,
    /// Background mode: returns async_launched immediately and notifies the main agent when done.
    #[serde(default)]
    #[schemars(
        description = "Async execution (default true): returns the instance name immediately without waiting; set false to wait synchronously for the result"
    )]
    background: Option<bool>,
    /// Notification condition: notify the main agent when the sub-agent output contains any of these strings.
    #[serde(default)]
    #[schemars(
        description = "Notify condition: notify when the subagent's output contains any of these strings"
    )]
    notify_on: Option<Vec<String>>,
    /// Short task description (optional), shown in the header.
    #[serde(default)]
    #[schemars(description = "Short task description (optional)")]
    description: Option<String>,
    /// Sub-agent model (optional): defaults to the named definition or parent session model.
    #[serde(default)]
    #[schemars(
        description = "Model for the subagent (optional; inherits the named definition / parent session by default); required when crossing providers — the parent model is not inherited"
    )]
    model: Option<String>,
    /// Sub-agent provider (optional, from the `providers` section of settings.json): when set, the sub-agent
    /// uses that provider's endpoint and key (independent of the parent session's current provider).
    #[serde(default)]
    #[schemars(
        description = "Provider for the subagent (optional; the providers section of settings; \"default\" or omitted = shared parent endpoint; specify model when crossing providers). Also the way to get an image looked at when this endpoint cannot receive one: fork onto an image-capable provider and repeat the `#[image N]` marker in the prompt — the attachment table is shared, so the subagent receives the real image."
    )]
    provider: Option<String>,
    /// Sub-agent thinking level (optional): off | low | medium | high | xhigh | max.
    #[serde(default)]
    #[schemars(
        description = "Thinking level for the subagent (optional): off/low/medium/high/xhigh/max; invalid values are rejected; defaults to off when crossing providers, otherwise inherits the named definition / parent session's current level"
    )]
    thinking: Option<String>,
    /// Instance name (optional): address used by SendMessage/AgentControl.
    #[serde(default)]
    #[schemars(
        description = "Instance name (optional): used to address it later via SendMessage/AgentControl; defaults to the named definition name or agent, with -2/-3 suffixes on name collisions"
    )]
    name: Option<String>,
    /// Named definition (optional): `.bingo/agents/<name>.md` or `~/.config/bingo/agents/<name>.md`.
    #[serde(default)]
    #[schemars(
        description = "Named agent definition (optional): uses that definition's system prompt and default model/provider"
    )]
    agent: Option<String>,
}

/// Sub-agent tool (D14/D29): recursive query loop with its own message history; result text is fed back
/// to the parent model. Each spawn is registered as a registry instance (addressable by name); history
/// is kept after completion and the main agent resumes the conversation via SendMessage (hub-and-spoke).
pub struct AgentTool {
    session: Arc<Session>,
    defs: Vec<AgentDef>,
}

impl AgentTool {
    pub fn new(session: Arc<Session>, defs: Vec<AgentDef>) -> Self {
        Self { session, defs }
    }
}

/// Serializes permission prompts forwarded by subagents: several background instances can
/// reach the single user at once, and both prompt surfaces (TUI modal, headless stdin) answer
/// one question at a time. Queue them instead of interleaving.
fn ask_gate() -> &'static tokio::sync::Mutex<()> {
    static GATE: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    GATE.get_or_init(|| tokio::sync::Mutex::new(()))
}

struct SubagentOutput {
    text: Arc<Mutex<String>>,
    progress: Arc<Mutex<crate::agents::AgentProgress>>,
}

/// `TurnStart` on the way in, `TurnEnd` on the way out — including the way out
/// an aborted task takes, which runs no code of its own.
struct TurnBrackets(crate::ui::EventSink);

impl TurnBrackets {
    fn open(events: crate::ui::EventSink) -> Self {
        events.send(UiEvent::TurnStart);
        Self(events)
    }
}

impl Drop for TurnBrackets {
    fn drop(&mut self) {
        self.0.send(UiEvent::TurnEnd);
    }
}

/// Sub-agent UI: captures text, streams onto the console's event channel, and
/// forwards permission prompts to the session that owns the UI. The cell tracks
/// the number of characters produced (for interval progress checks of background
/// agents).
/// Snapshot of everything a subagent accumulated up to the last committed round;
/// a stream retry rolls the failed attempt back to this point.
///
/// The rendered half of that rollback left with D134: `UiEvent::StreamRetry` is
/// what the console has always used to unwind main's failed attempt, and an
/// instance's turn is now on the same channel. What stays here is the flat reply
/// — the spawn's return value, which no console sees.
#[derive(Clone, Default)]
struct AttemptCheckpoint {
    text_len: usize,
    produced_chars: usize,
    output_tokens: u64,
    tool_uses: usize,
    recent_activity: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
fn subagent_hooks(
    output: SubagentOutput,
    events: Option<crate::ui::EventSink>,
    cell: Arc<AgentCell>,
    watch: Arc<WatchRegistry>,
    id: WatchId,
    instance: String,
    ask: Option<Arc<crate::query::AskFn>>,
) -> UiHooks {
    // `output.text` stays the flat reply — what the spawn returns and what
    // `spoke` is judged on. Everything the *screen* needs travels as events, so
    // an instance's page is built by the code that builds main's rather than by
    // a second store polled per frame (D134).
    let events = events.unwrap_or_else(crate::ui::EventSink::detached);
    let tool_events = events.clone();
    let done_events = events.clone();
    let retry_events = events.clone();
    let round_events = events.clone();
    let warn_events = events.clone();
    let inbound_events = events.clone();
    let usage_events = events.clone();
    let tool_progress = output.progress.clone();
    let retry_text = output.text.clone();
    let retry_progress = output.progress.clone();
    let round_text = output.text.clone();
    let round_progress = output.progress.clone();
    let text_output = output.text;
    let progress_output = output.progress;
    let registry = cell.registry.clone();
    let event_registry = registry.clone();
    let event_instance = instance.clone();
    let tool_registry = registry.clone();
    let tool_instance = instance.clone();
    let done_registry = registry.clone();
    let done_instance = instance.clone();
    let round_units = Arc::new(Mutex::new(0u64));
    let retry_units = round_units.clone();
    let event_round_units = round_units.clone();
    let attempt_checkpoint = Arc::new(Mutex::new(AttemptCheckpoint::default()));
    let retry_checkpoint = attempt_checkpoint.clone();
    let round_checkpoint = attempt_checkpoint.clone();
    let event_cell = cell.clone();
    let retry_cell = cell.clone();
    let round_cell = cell.clone();
    UiHooks {
        on_event: Box::new(move |event| {
            let tokens = match event {
                crate::api::contract::StreamEvent::TextDelta { text, .. } => {
                    event_registry.touch(&event_instance);
                    let tokens = {
                        let mut units = event_round_units.lock().unwrap_or_else(|e| e.into_inner());
                        *units = units.saturating_add(crate::compact::text_units(text));
                        units.div_ceil(4)
                    };
                    if let Ok(mut output) = text_output.lock() {
                        output.push_str(text);
                        events.send(UiEvent::TextDelta(text.clone()));
                        event_cell.record_chars(text.chars().count());
                        // Feed produced text into the condition engine (notify_on hit → signal notification).
                        watch.feed_content(id, text);
                    }
                    tokens
                }
                crate::api::contract::StreamEvent::ThinkingDelta { thinking, .. } => {
                    event_registry.touch(&event_instance);
                    events.send(UiEvent::ThinkingDelta(thinking.clone()));
                    let mut units = event_round_units.lock().unwrap_or_else(|e| e.into_inner());
                    *units = units.saturating_add(crate::compact::text_units(thinking));
                    units.div_ceil(4)
                }
                crate::api::contract::StreamEvent::InputJsonDelta { partial_json, .. } => {
                    let mut units = event_round_units.lock().unwrap_or_else(|e| e.into_inner());
                    *units = units.saturating_add(crate::compact::text_units(partial_json));
                    units.div_ceil(4)
                }
                crate::api::contract::StreamEvent::ToolUseStart { name, .. } => {
                    events.send(UiEvent::ToolStart { name: name.clone() });
                    return;
                }
                crate::api::contract::StreamEvent::StopReason {
                    output_tokens: Some(tokens),
                    ..
                } => {
                    *event_round_units.lock().unwrap_or_else(|e| e.into_inner()) =
                        tokens.saturating_mul(4);
                    if let Ok(mut progress) = progress_output.lock() {
                        progress.add_output_tokens(*tokens);
                    }
                    events.send(UiEvent::OutputTokens {
                        tokens: *tokens,
                        authoritative: true,
                    });
                    return;
                }
                _ => return,
            };
            events.send(UiEvent::OutputTokens {
                tokens,
                authoritative: false,
            });
        }),
        on_stream_retry: Box::new(move || {
            *retry_units.lock().unwrap_or_else(|e| e.into_inner()) = 0;
            retry_events.send(UiEvent::StreamRetry);
            let checkpoint = retry_checkpoint
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Ok(mut text) = retry_text.lock() {
                text.truncate(checkpoint.text_len);
            }
            retry_cell.set_chars(checkpoint.produced_chars);
            if let Ok(mut progress) = retry_progress.lock() {
                progress.restore_attempt(
                    checkpoint.output_tokens,
                    checkpoint.tool_uses,
                    checkpoint.recent_activity,
                );
            }
        }),
        // The surface D89 left this hook waiting for: an instance's page has a
        // footer of its own now, and it reports the instance's window rather
        // than borrowing main's.
        on_context_usage: Arc::new(move |usage| {
            usage_events.send(UiEvent::ContextUsage(usage));
        }),
        on_tool_ready: Box::new(move |tool_call_id, name, input, standalone| {
            tool_registry.touch(&tool_instance);
            // The progress line is a label and stays one; the call itself goes
            // on the channel, because the page builds its rows from the call.
            let glyph = crate::tui::activities::tool_glyph(&name);
            let shown = crate::tui::activities::display_tool_name(&name);
            let summary = crate::query::summarize_input(&name, &input);
            let activity = if summary.is_empty() {
                format!("{glyph}{shown}")
            } else {
                format!("{glyph}{shown}({summary})")
            };
            tool_events.send(UiEvent::ToolReady {
                tool_call_id,
                name,
                input,
                standalone,
            });
            if let Ok(mut progress) = tool_progress.lock() {
                progress.record_tool(activity);
            }
        }),
        on_tool_done: Box::new(move |done| {
            done_registry.touch(&done_instance);
            done_events.send(UiEvent::ToolDone(done.clone()));
        }),
        on_round_end: Box::new(move || {
            *round_units.lock().unwrap_or_else(|e| e.into_inner()) = 0;
            round_events.send(UiEvent::RoundEnd);
            let (output_tokens, tool_uses, recent_activity) = round_progress
                .lock()
                .map(|progress| {
                    (
                        progress.output_tokens,
                        progress.tool_uses,
                        progress.recent_activity.clone(),
                    )
                })
                .unwrap_or_default();
            *round_checkpoint.lock().unwrap_or_else(|e| e.into_inner()) = AttemptCheckpoint {
                text_len: round_text.lock().map_or(0, |text| text.len()),
                produced_chars: round_cell.chars(),
                output_tokens,
                tool_uses,
                recent_activity,
            };
        }),
        // A reconnect notice used to be spliced into the instance's own prose,
        // where it read as something the agent had said. It is a warning about
        // the stream, so it takes the tier every other warning takes.
        on_warning: Box::new(move |message| {
            warn_events.send(UiEvent::Warning(message));
        }),
        on_inbound: Box::new(move |text| {
            inbound_events.send(UiEvent::Inbound(text.to_string()));
        }),
        // A subagent has no prompt surface of its own, so its Ask decisions are forwarded to
        // the session that owns the UI, stamped with the instance name. Auto-denying here
        // would fail the tool call as "user denied" without the user ever being asked — and
        // auto-allowing under bypassPermissions would silently clear the safety-check gate
        // that is supposed to survive bypass.
        // A subagent is steered through its own inbox (`absorb_inbox`), drained at the
        // top of every round; the composer's channel belongs to the foreground turn.
        steer: crate::query::no_steer(),
        // A subagent's shell output belongs to its own transcript, not to the main
        // view's tail: its commands are not the foreground turn's commands, and its
        // rows are not the rows ctrl+b talks about (D84).
        live: crate::live::LiveBash::detached(),
        ask: std::sync::Arc::new(move |request| {
            // No prompt surface attached: both real entry points (TUI, headless) attach one at
            // startup, so this is the embedded/test path — fall back to denying.
            let Some(ask) = ask.clone() else {
                return Box::pin(async { crate::query::AskOutcome::Deny { feedback: None } });
            };
            // The forwarded request is rebuilt from owned copies: the borrowed
            // one cannot cross into the future that waits on the gate lock.
            let tool = request.tool.to_string();
            let reason = format!("{instance} · {}", request.reason);
            let input = request.input.clone();
            let cwd = request.cwd.to_path_buf();
            let scope = request.scope.map(str::to_string);
            let diff = request.diff.map(str::to_string);
            Box::pin(async move {
                let _serialized = ask_gate().lock().await;
                ask(&crate::query::AskContext {
                    tool: &tool,
                    reason: &reason,
                    input: &input,
                    cwd: &cwd,
                    scope: scope.as_deref(),
                    diff: diff.as_deref(),
                })
                .await
            })
        }),
        // AskUserQuestion is not assembled for subagents (see `assemble_tools`); if one ever
        // reaches here, treat it as unanswered rather than blocking on a modal.
        ask_question: std::sync::Arc::new(|_title, _question, _options| Box::pin(async { None })),
    }
}

/// Start every idle recipient that has mail waiting. `AgentRegistry::flush_pending` claims the
/// full inbox atomically, so concurrent dispatchers cannot double-start an instance and every
/// item present at the receiver's claim point becomes one prompt.
pub(crate) fn flush_agent_inbox(session: &Arc<Session>, watch: &Arc<WatchRegistry>) {
    for wake in session.agents.flush_pending() {
        if !session.agents.accepts_run(&wake.name, wake.run) {
            session.agents.restore_inbox(&wake.name, wake.items);
            continue;
        }
        let (prompt, images) = absorb_inbox(&session.channels, &wake.name, &wake.items);
        let items = wake.items;
        let label = format!("{} #{} · {}", wake.name, wake.run, excerpt(&prompt));
        spawn_agent_loop(
            session.agents.clone(),
            watch.clone(),
            wake.name,
            wake.session,
            wake.history,
            prompt,
            images,
            items,
            label,
            wake.run,
            Vec::new(),
            session.instance.clone(),
            false,
        );
    }
}

/// Acknowledgement watchdog for one message: the sender named a wait, so when that wait elapses
/// this re-reads the very record `AgentControl(action=messages)` reports and, while the message
/// still has not entered the receiver's context, nudges the receiver and retries the boundary
/// flush — the automatic form of the poll main would otherwise have to run by hand.
///
/// What it waits for is an *answer*, not a delivery. Reading a message into a prompt proves
/// nothing about the receiver: an instance can take the message, run a turn and end it without a
/// word, which from the sender's side is indistinguishable from a hang. So `Delivered` is chased
/// exactly like `Queued`, and only `Answered` stops the clock.
///
/// Two bounds keep it a mechanism rather than a loop: at most `MAX_FOLLOW_UPS` rounds, and every
/// outcome except an answer inside the wait is reported back to the sender as a watch line, whose
/// terminal state reaches main's next turn. A chase that never gives up and never speaks would
/// be worse than no chase at all.
pub(crate) fn spawn_ack_watchdog(
    session: Arc<Session>,
    watch: Arc<WatchRegistry>,
    agent: String,
    id: MsgId,
    timeout: std::time::Duration,
) {
    let owner = session.instance.clone();
    let label = format!("{agent} #{id} receipt");
    tokio::spawn(async move {
        // Registered on the first missed deadline, not up front: a message that lands on time
        // leaves no trace, so the line itself means "this one needed chasing".
        let mut line: Option<WatchId> = None;
        let mut sent = 0u8;
        let report = |state: WatchState, detail: String, line: &mut Option<WatchId>| {
            let id = *line.get_or_insert_with(|| {
                watch.register_with_conditions(
                    Box::new(AckWatch {
                        label: label.clone(),
                    }),
                    Vec::new(),
                    owner.clone(),
                )
            });
            watch.set_state(id, state, Some(detail), None);
        };
        loop {
            tokio::time::sleep(timeout).await;
            match session.agents.follow_up(&agent, id) {
                FollowUp::Settled(crate::agents::AckState::Answered { run }) => {
                    if sent > 0 {
                        report(
                            WatchState::Done,
                            format!("{sent} follow-ups before the reply (round {run})"),
                            &mut line,
                        );
                    }
                    return;
                }
                FollowUp::Settled(crate::agents::AckState::Dropped { reason }) => {
                    report(
                        WatchState::Failed,
                        format!("not delivered: {reason}"),
                        &mut line,
                    );
                    return;
                }
                // The two waiting states are exactly what follow_up chases, never settles on.
                FollowUp::Settled(
                    crate::agents::AckState::Queued | crate::agents::AckState::Delivered { .. },
                ) => return,
                FollowUp::Gone => {
                    report(
                        WatchState::Failed,
                        "instance removed; the message never got an answer".to_string(),
                        &mut line,
                    );
                    return;
                }
                FollowUp::Sent { round } => {
                    sent = round;
                    report(
                        WatchState::Running,
                        format!("waiting for a reply, chased {round}/{MAX_FOLLOW_UPS}"),
                        &mut line,
                    );
                    flush_agent_inbox(&session, &watch);
                }
                FollowUp::Exhausted => {
                    report(
                        WatchState::Failed,
                        format!(
                            "{MAX_FOLLOW_UPS} follow-ups and {agent} still has not replied: use AgentControl(action=messages) to see whether it is stuck queued or read it without answering"
                        ),
                        &mut line,
                    );
                    return;
                }
            }
        }
    });
}

/// Watch line for a chased acknowledgement: driven entirely by `spawn_ack_watchdog`, so it
/// declares no polling interval of its own.
struct AckWatch {
    label: String,
}

impl crate::watch::Watchable for AckWatch {
    fn label(&self) -> String {
        self.label.clone()
    }
    fn poll(&self) -> crate::watch::WatchPoll {
        crate::watch::WatchPoll {
            state: WatchState::Running,
            detail: None,
            payload: None,
            signal: None,
        }
    }
    fn check_interval(&self) -> Option<std::time::Duration> {
        None
    }
    fn kind(&self) -> WatchKind {
        WatchKind::Agent
    }
}

/// Single-line excerpt (for labels): cut at newline / 40 characters.
pub(crate) fn excerpt(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    let cut: String = line.chars().take(40).collect();
    if cut.chars().count() < text.chars().count() {
        format!("{cut}…")
    } else {
        cut
    }
}

/// Line the user's direct messages arrive under, on its own line above the text (D64). The
/// main stays untagged — it is the default voice of direct instructions — so the marker is
/// the one observable difference between "your manager" and "the human", and the DM view
/// drops the line rather than rendering scaffolding as prose.
pub(crate) const DM_FROM_USER_MARKER: &str = "[DM from user]";

/// The user's messages carry the marker in every shape of batch; main's only gain their
/// `[follow-up instruction]` label when a batch makes the boundaries ambiguous.
fn direct_text(from: &str, text: &str, batched: bool) -> String {
    if from == crate::channels::USER_NAME {
        format!("{DM_FROM_USER_MARKER}\n{text}")
    } else if batched {
        format!("[follow-up instruction] {text}")
    } else {
        text.to_string()
    }
}

/// Inbox → turn prompt plus the images those instructions carried: a single main instruction is
/// kept verbatim; user messages arrive under [`DM_FROM_USER_MARKER`]; mixed or multiple entries
/// are annotated with their sources in order. Channel entries also advance the member's read
/// cursor (messages enter its context with this turn).
pub(crate) fn absorb_inbox(
    channels: &Arc<ChannelRegistry>,
    name: &str,
    items: &[InboxItem],
) -> (String, Vec<crate::api::types::ImageAttachment>) {
    let mut latest: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
    for item in items {
        if let InboxItem::Channel { channel, seq, .. } = item {
            let cursor = latest.entry(channel.as_str()).or_insert(0);
            if *cursor < *seq {
                *cursor = *seq;
            }
        }
    }
    for (channel, seq) in latest {
        channels.mark_seen(name, channel, seq);
    }
    let images: Vec<crate::api::types::ImageAttachment> = items
        .iter()
        .filter_map(|item| match item {
            InboxItem::Direct { images, .. } => Some(images.clone()),
            InboxItem::Channel { .. }
            | InboxItem::FollowUp { .. }
            | InboxItem::Unanswered { .. } => None,
        })
        .flatten()
        .collect();
    let prompt = match items {
        [InboxItem::Direct { from, text, .. }] => direct_text(from, text, false),
        _ => items
            .iter()
            .map(|item| match item {
                InboxItem::Direct { from, text, .. } => direct_text(from, text, true),
                InboxItem::Channel {
                    channel,
                    from,
                    text,
                    seq,
                    ..
                } => format!("[#{channel} msg #{seq}] {from}: {text}"),
                InboxItem::FollowUp {
                    original,
                    round,
                    excerpt,
                    waited,
                    delivered,
                } => {
                    let silence = if *delivered {
                        "you read it in an earlier turn and ended that turn without saying anything"
                    } else {
                        "it sat in your inbox without being picked up"
                    };
                    format!(
                        "[follow-up {round}/{MAX_FOLLOW_UPS}] Main sent you message \
                         #{original} (\"{excerpt}\") {}s ago and has had no reply: {silence}. \
                         Answer it now — if you are still working, say what you are doing and \
                         what you have so far; if you have nothing to add, say that. Ending a \
                         turn in silence reads as a hang from the outside.",
                        waited.as_secs()
                    )
                }
                InboxItem::Unanswered {
                    channel,
                    seq,
                    from,
                    excerpt,
                    round,
                    waited,
                } => format!(
                    "[follow-up {round}/{MAX_FOLLOW_UPS}] {from} named you in \
                     [#{channel} msg #{seq}] (\"{excerpt}\") {}s ago and you have not \
                     posted to #{channel} since. An `@` on your name is the one thing \
                     that owes an answer — answer it in the room now: if you are still \
                     working, say what you are doing and what you have so far; if you \
                     have nothing to add, say that. Silence in a room is free only when \
                     nobody named you.",
                    waited.as_secs()
                ),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };
    (prompt, images)
}

/// Placeholder for empty output.
fn non_empty(text: String) -> String {
    if text.trim().is_empty() {
        "[subagent returned no text]".to_string()
    } else {
        text
    }
}

/// Register a watch line for a run (◉ `{label}` · produced N chars).
///
/// `dispatch` marks the one run an `Agent` call itself asked for (D114): the
/// flow staples rows and prints `●` for dispatches only, so a delivery or a
/// continuation must not claim the bit.
fn register_run_watch(
    watch: &Arc<WatchRegistry>,
    label: String,
    cell: Arc<AgentCell>,
    conditions: Vec<NotifyCondition>,
    owner: Option<String>,
    notify_owner: bool,
    dispatch: bool,
) -> WatchId {
    watch.register_addressed(
        Box::new(AgentWatch {
            cell,
            label,
            interval: Some(std::time::Duration::from_secs(5)),
        }),
        conditions,
        owner,
        notify_owner,
        dispatch,
    )
}

/// Whether the end of a run driven by these items is the owner's business (D98).
///
/// The trigger decides, not the run: a batch the *user* typed into this
/// instance's DM is a conversation between the two of them, and its terminal
/// state owes the main agent nothing — no task notification, no woken turn (the
/// D63 privacy line, finally drawn on the wake path too). A dispatch (no items
/// at all — the `Agent` call itself is the trigger), a `SendMessage`
/// continuation, a room relay, or any batch that *mixes* one of those in, is
/// main's business as it always was: one main-origin item in the batch is
/// enough, because the reply that comes back answers it.
pub(crate) fn wakes_owner(items: &[InboxItem]) -> bool {
    items.is_empty()
        || !items
            .iter()
            .all(|item| matches!(item, InboxItem::Direct { from, .. } if from == crate::channels::USER_NAME))
}

/// Drive an instance's run chain in the background: run_query → history saved to the registry → if
/// the inbox is non-empty, continue with the next run of the same task (new watch line); once drained,
/// transition to Idle. The abort handle is attached to the registry (stop/delete can abort).
/// Returns the watch id of the first run.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_agent_loop(
    registry: Arc<AgentRegistry>,
    watch: Arc<WatchRegistry>,
    name: String,
    session: Arc<Session>,
    history: Vec<Message>,
    prompt: String,
    images: Vec<crate::api::types::ImageAttachment>,
    initial_items: Vec<InboxItem>,
    first_label: String,
    first_run: u64,
    conditions: Vec<NotifyCondition>,
    owner: Option<String>,
    dispatch: bool,
) -> WatchId {
    let cell = Arc::new(AgentCell::new(registry.clone()));
    let wakes_owner_first = wakes_owner(&initial_items);
    let first_id = register_run_watch(
        &watch,
        first_label,
        cell.clone(),
        conditions,
        owner.clone(),
        wakes_owner_first,
        dispatch,
    );
    registry.set_run_watch(&name, first_id);
    let loop_registry = registry.clone();
    let loop_name = name.clone();
    let retry_items = initial_items;
    let current_items_for_install = retry_items.clone();
    let handle = tokio::spawn(async move {
        let name = loop_name;
        if !loop_registry.accepts_run(&name, first_run) {
            loop_registry.restore_inbox(&name, retry_items);
            return;
        }
        let mut history = history;
        let mut prompt = prompt;
        let mut images = images;
        let mut current_items = retry_items;
        let mut run = (first_id, cell);
        loop {
            let output = Arc::new(Mutex::new(String::new()));
            let progress = Arc::new(Mutex::new(crate::agents::AgentProgress::default()));
            if let Ok(mut progress) = progress.lock() {
                progress.start_run();
            }
            loop_registry.set_prompt(&name, prompt.clone());
            loop_registry.set_progress(&name, Some(progress.clone()));
            let sink = loop_registry.sink_for(&name);
            // The turn's brackets, and the reason they are a guard: an instance
            // is stopped by aborting its task, which unwinds this future without
            // running another line. A `TurnEnd` sent on the way out is the only
            // one an abort cannot swallow — and a conversation left `busy`
            // forever is a spinner that never stops.
            let turn = sink.clone().map(TurnBrackets::open);
            let mut ui = subagent_hooks(
                SubagentOutput {
                    text: output.clone(),
                    progress,
                },
                sink,
                run.1.clone(),
                watch.clone(),
                run.0,
                name.clone(),
                loop_registry.ask_fn(),
            );
            let outcome =
                crate::query::run_query(&session, history, &prompt, &images, &mut ui, None).await;
            drop(turn);
            match outcome {
                Ok(outcome) => {
                    let text = output.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let output_chars = text.chars().count();
                    loop_registry.set_progress(&name, None);
                    watch.set_state(
                        run.0,
                        WatchState::Done,
                        Some("done".to_string()),
                        Some(serde_json::json!(non_empty(text))),
                    );
                    match loop_registry.finish(&name, outcome.messages, output_chars) {
                        Some(next) => {
                            history = next.history;
                            current_items = next.items;
                            (prompt, images) =
                                absorb_inbox(&session.channels, &name, &current_items);
                            let cell = Arc::new(AgentCell::new(loop_registry.clone()));
                            let label = format!("{name} #{} · {}", next.run, excerpt(&prompt));
                            let wakes = wakes_owner(&current_items);
                            // A continuation is a delivery draining, never the
                            // dispatch itself — even when the first run was one.
                            let id = register_run_watch(
                                &watch,
                                label,
                                cell.clone(),
                                Vec::new(),
                                owner.clone(),
                                wakes,
                                false,
                            );
                            loop_registry.set_run_watch(&name, id);
                            run = (id, cell);
                        }
                        None => break,
                    }
                }
                Err(e) => {
                    loop_registry.set_progress(&name, None);
                    loop_registry.restore_inbox(&name, current_items);
                    watch.set_state(
                        run.0,
                        WatchState::Failed,
                        Some(format!("subagent failed: {e}")),
                        None,
                    );
                    loop_registry.mark_idle(&name);
                    break;
                }
            }
        }
    });
    let _ = registry.set_abort_if_running(
        &name,
        first_run,
        handle.abort_handle(),
        current_items_for_install,
    );
    first_id
}

impl AgentTool {
    /// Resolve the named definition (agent parameter).
    fn resolve_def(&self, params: &AgentInput) -> Result<Option<&AgentDef>, ToolError> {
        let Some(want) = &params.agent else {
            return Ok(None);
        };
        self.defs
            .iter()
            .find(|d| &d.name == want)
            .map(Some)
            .ok_or_else(|| {
                let known: Vec<&str> = self.defs.iter().map(|d| d.name.as_str()).collect();
                ToolError::failed(if known.is_empty() {
                    format!("unknown agent definition: {want} (no named definitions)")
                } else {
                    format!(
                        "unknown agent definition: {want}; available: {}",
                        known.join(", ")
                    )
                })
            })
    }

    /// Spawn an instance: claim a name → build a sub-session (carrying the instance name for Post
    /// stamps) → register in the registry. Returns (instance name, description, sub-session).
    fn spawn_instance(
        &self,
        params: &AgentInput,
        def: Option<&AgentDef>,
        cwd: &std::path::Path,
    ) -> Result<(String, String, Arc<Session>), ToolError> {
        let base = params
            .name
            .clone()
            .or_else(|| def.map(|d| d.name.clone()))
            .unwrap_or_else(|| "agent".to_string());
        let name = self.session.agents.claim_name(&base);
        let sub_session = self.build_sub_session(params, def, &name, cwd)?;
        let description = params
            .description
            .clone()
            .unwrap_or_else(|| excerpt(&params.prompt));
        // Every spawn from this tool is a hire, never a member: the blueprint is the only
        // thing that makes a crew, and it is written by the user's confirmation alone (D53).
        self.session.agents.insert(
            &name,
            AgentKind::Hire,
            def.map(|d| d.name.clone()),
            description.clone(),
            sub_session.clone(),
        );
        record_hire(&self.session, cwd, &name, &description);
        Ok((name, description, sub_session))
    }

    fn launch_background(
        &self,
        params: &AgentInput,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let def = self.resolve_def(params)?;
        let (name, description, sub_session) = self.spawn_instance(params, def, &ctx.cwd)?;
        let run = self.session.agents.next_run(&name);
        let conditions = params
            .notify_on
            .clone()
            .map(|p| vec![NotifyCondition::Contains(p)])
            .unwrap_or_default();
        let id = spawn_agent_loop(
            self.session.agents.clone(),
            ctx.watch.clone(),
            name.clone(),
            sub_session,
            Vec::new(),
            params.prompt.clone(),
            self.session.attachments.resolve(&params.prompt),
            Vec::new(),
            format!("{name} · {description}"),
            run,
            conditions,
            ctx.instance.clone(),
            true,
        );
        Ok(ToolResult {
            content: serde_json::Value::String(serde_json::json!({
                "status": "async_launched",
                "name": name,
                "task_id": id.0,
                "note": "subagent is running in the background; a completion notification will be injected into the next turn's context; SendMessage sends follow-up instructions, AgentControl can list/stop/delete",
            })
            .to_string()),
            is_error: false,
            diff: None,
        })
    }

    /// Build a sub-agent session: the named definition provides the system prompt and default
    /// model/provider; explicit parameters take precedence over the definition, which takes
    /// precedence over inheritance (when a provider is set, fork an independent-endpoint client
    /// so the parent session's current provider is unaffected).
    fn build_sub_session(
        &self,
        params: &AgentInput,
        def: Option<&AgentDef>,
        instance: &str,
        cwd: &std::path::Path,
    ) -> Result<Arc<Session>, ToolError> {
        build_sub_session(
            &self.session,
            params.model.clone(),
            params.provider.clone(),
            params.thinking.clone(),
            def,
            instance,
            hire_context(cwd),
        )
    }
}

/// What a fresh ad-hoc spawn carries about the project's crew: the agreement it works to,
/// and the fact that this spawn is a hire rather than a member.
///
/// Empty when no crew is pinned here — an ordinary subagent in a project with no crew is
/// not temporary relative to anything, and telling it otherwise would be a lie about a
/// team that does not exist. Memory stays empty either way: a past on disk belongs to a
/// crew member, which is the thing a blueprint keeps across sessions (D51).
/// Record a hire in the crew's decision log — the same append-only file `/team assign`
/// writes to, so "who was brought in from outside, and for what" is reviewable after the
/// fact rather than being a thing that only ever happened in a context window. No-op in a
/// project with no crew: there is nothing for the hire to be outside of.
fn record_hire(session: &Arc<Session>, cwd: &std::path::Path, name: &str, description: &str) {
    let Some(team) = crate::team::load_team_file(cwd).ok().flatten() else {
        return;
    };
    crate::team::append_decision(
        &session.home,
        cwd,
        &crate::team::current_branch(cwd),
        &team.name,
        "hire",
        description,
        &[name],
    );
}

fn hire_context(cwd: &std::path::Path) -> MemberContext {
    let Some(team) = crate::team::load_team_file(cwd).ok().flatten() else {
        return MemberContext::default();
    };
    MemberContext {
        memory: None,
        norms: crate::team::load_norms(cwd).map(|n| crate::team::norms_block(&team.name, &n)),
        standing: Some(crate::team::hire_note(&team.name)),
        cwd: None,
    }
}

/// The system blocks an instance carries beyond its persona: where its own past on disk is
/// (D51), the agreement the project's crew works to, and — for a hire — the fact that it is
/// not on that crew (D53). All three are system blocks rather than messages, because nobody
/// said them, and because compaction rewrites messages and leaves `Session::system` alone.
#[derive(Debug, Default, Clone)]
pub(crate) struct MemberContext {
    /// Pointer to this instance's own transcript, for the instances that have one.
    pub memory: Option<String>,
    /// The crew's working agreement (`.bingo/team-norms.md`), already wrapped with its
    /// precedence rule by [`crate::team::norms_block`].
    pub norms: Option<String>,
    /// Where this instance stands relative to the crew — set only for a temporary hire.
    pub standing: Option<String>,
    /// Override the parent session's working directory for a team rooted elsewhere.
    pub cwd: Option<std::path::PathBuf>,
}

impl MemberContext {
    fn blocks(self) -> impl Iterator<Item = String> {
        [self.norms, self.standing, self.memory]
            .into_iter()
            .flatten()
    }
}

/// Normalize a thinking level (explicit parameter / named definition entry): `off` → `None`
/// (no thinking parameter); valid levels pass through; anything else is an error — silently
/// degrading an invalid value to off would let the user believe thinking is on when it isn't,
/// so sub-agent spawn must surface it immediately. Inherited values skip this check
/// (consistent with the main session after `/think`, see [`build_sub_session`]).
pub(crate) fn normalize_thinking(level: &str) -> Result<Option<String>, String> {
    if level == "off" {
        return Ok(None);
    }
    if crate::api::contract::THINKING_LEVELS.contains(&level) {
        return Ok(Some(level.to_string()));
    }
    Err(format!(
        "invalid thinking level \"{level}\" (use: off/low/medium/high/xhigh/max)"
    ))
}

/// Build a sub-agent session (shared by AgentTool and team spawn, D31):
/// the named definition provides the system prompt and default model/provider; explicit parameters
/// take precedence over the definition, which takes precedence over inheritance. A named provider
/// forks an independent-endpoint client so the parent session is unaffected; "default" or no
/// provider shares the parent endpoint and follows the parent session's switches.
///
/// `context` carries what this instance knows beyond its persona — its own past on disk,
/// the crew's agreement, its standing on that crew (see [`MemberContext`]).
pub(crate) fn build_sub_session(
    parent: &Arc<Session>,
    model: Option<String>,
    provider: Option<String>,
    thinking: Option<String>,
    def: Option<&AgentDef>,
    instance: &str,
    context: MemberContext,
) -> Result<Arc<Session>, ToolError> {
    let model = model.or_else(|| def.and_then(|d| d.model.clone()));
    // provider: "default" and unset are equivalent (shared parent endpoint, follows the parent's switches);
    // only a named provider forks an independent endpoint. Unknown names error here (immediate feedback).
    let named_provider = provider
        .or_else(|| def.and_then(|d| d.provider.clone()))
        .filter(|p| p != "default");
    let client = match &named_provider {
        Some(name) => parent
            .client
            .with_provider(name)
            .map_err(ToolError::failed)?,
        None => parent.client.clone(),
    };
    let provider_name = named_provider
        .clone()
        .unwrap_or_else(|| parent.runtime.provider.borrow().clone());
    // Cross-provider rule: crossing means forking to an endpoint different from the parent's current provider (unset
    // provider = shared parent endpoint, always the same provider). When crossing, the parent session's model and
    // thinking level are unusable — the model name would go to the wrong endpoint (e.g. claude-sonnet-5 sent to
    // DeepSeek would 404 with "model not found"), and the thinking parameter may be rejected by the endpoint.
    let cross_provider = match &named_provider {
        Some(name) => name != parent.runtime.provider.borrow().as_str(),
        None => false,
    };
    let model = match model {
        Some(m) => m,
        None if cross_provider => {
            let parent_provider = parent.runtime.provider.borrow().clone();
            return Err(ToolError::failed(format!(
                "provider \"{}\" requires a model: crossing providers does not inherit the parent session's model \
                 (current parent provider = \"{parent_provider}\"); specify an explicit model or drop the provider",
                named_provider.as_deref().unwrap_or("")
            )));
        }
        None => parent.runtime.model.borrow().clone(),
    };
    // Thinking level: explicit parameter/definition is validated (off→no parameter, invalid values error rather than silently degrading);
    // when neither is set: crossing providers defaults to off (no thinking parameter, compatible with ds/ollama endpoints),
    // same-provider inherits a snapshot of the parent session's current level (the same lenient semantics as the main session).
    let thinking = match thinking.or_else(|| def.and_then(|d| d.thinking.clone())) {
        Some(level) => normalize_thinking(&level).map_err(ToolError::failed)?,
        None if cross_provider => None,
        None => parent.runtime.thinking.borrow().clone(),
    };
    let cache = parent.settings.cache_control.unwrap_or(false);
    let persona = |text: &str| SystemBlock {
        text: text.to_string(),
        cache,
    };
    let mut system = match def {
        Some(d) if d.system.trim().is_empty() => parent.system.clone(),
        // Replacing wholesale also drops the environment info, CLAUDE.md/AGENTS.md and project
        // memory — rarely what a persona wants, so appending is the default.
        Some(d) if d.inherit_system => {
            let mut blocks = parent.system.clone();
            blocks.push(persona(&d.system));
            blocks
        }
        Some(d) => vec![persona(&d.system)],
        None => parent.system.clone(),
    };
    // Uncached on purpose: a short tail block is not worth another cache breakpoint.
    system.push(SystemBlock {
        text: SUBAGENT_NOTE.to_string(),
        cache: false,
    });
    // The parent's capability block names the parent's model; refresh it for
    // this instance's own (a cross-provider subagent has a different model,
    // and the vision/thinking facts must describe the endpoint actually
    // speaking). The per-request refresh in query_turn keeps it honest after
    // any mid-session switch.
    system =
        crate::system::with_model_capabilities(&system, &model, &provider_name, &client.models());
    // Only when the feature is on: channel etiquette is noise for a solo subagent that will
    // never see a room.
    if parent.settings.experimental.agent_channels {
        system.push(SystemBlock {
            text: CHANNEL_NOTE.to_string(),
            cache: false,
        });
    }
    let cwd = context
        .cwd
        .clone()
        .map(|cwd| Arc::new(std::sync::Mutex::new(cwd)))
        .unwrap_or_else(|| parent.cwd.clone());
    // The agreement, the standing, the past — whichever of them this instance has.
    for text in context.blocks() {
        system.push(SystemBlock { text, cache: false });
    }
    let mut runtime = crate::query::Runtime::new(model, None, Default::default());
    // Share the parent's permission table and MCP connections rather than snapshotting them:
    // `/permissions` edits reach instances that are already running, and a subagent reuses the
    // parent's MCP handshake instead of starting from an empty manager (i.e. no MCP tools).
    runtime.permissions = parent.runtime.permissions.clone();
    runtime.mcp = parent.runtime.mcp.clone();
    let _ = runtime.provider_tx.send(provider_name);
    let _ = runtime.thinking_tx.send(thinking);
    Ok(Arc::new(Session {
        client,
        runtime,
        permission_mode: parent.permission_mode,
        settings: parent.settings.clone(),
        system,
        depth: parent.depth + 1,
        cwd,
        home: parent.home.clone(),
        user_config_dir: parent.user_config_dir.clone(),
        quiet: parent.quiet,
        compact_failures: parent.compact_failures.clone(),
        watch: parent.watch.clone(),
        tasks: parent.tasks.clone(),
        expand_tasks: parent.expand_tasks.clone(),
        agents: parent.agents.clone(),
        channels: parent.channels.clone(),
        instance: Some(instance.to_string()),
        attachments: parent.attachments.clone(),
    }))
}

/// Background agent progress: characters produced (for interval polling).
struct AgentCell {
    chars: std::sync::atomic::AtomicUsize,
    registry: Arc<AgentRegistry>,
}

impl AgentCell {
    fn new(registry: Arc<AgentRegistry>) -> Self {
        Self {
            chars: std::sync::atomic::AtomicUsize::new(0),
            registry,
        }
    }
    fn record_chars(&self, n: usize) {
        self.chars.fetch_add(n, std::sync::atomic::Ordering::SeqCst);
    }
    fn chars(&self) -> usize {
        self.chars.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn set_chars(&self, chars: usize) {
        self.chars.store(chars, std::sync::atomic::Ordering::SeqCst);
    }
    fn poll(&self) -> crate::watch::WatchPoll {
        crate::watch::WatchPoll {
            state: WatchState::Running,
            detail: Some(format!(
                "produced {} chars",
                self.chars.load(std::sync::atomic::Ordering::SeqCst)
            )),
            payload: None,
            signal: None,
        }
    }
}

struct AgentWatch {
    cell: Arc<AgentCell>,
    label: String,
    interval: Option<std::time::Duration>,
}

impl crate::watch::Watchable for AgentWatch {
    fn label(&self) -> String {
        self.label.clone()
    }
    fn poll(&self) -> crate::watch::WatchPoll {
        self.cell.poll()
    }
    fn check_interval(&self) -> Option<std::time::Duration> {
        self.interval
    }
    fn kind(&self) -> WatchKind {
        WatchKind::Agent
    }
}

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> String {
        "Agent".to_string()
    }

    fn description(&self) -> String {
        let mut desc = "Spawn a subagent for an independent task (depth-limited). Async by default: returns the instance name and task id immediately without waiting; a completion notification is injected when the subagent finishes; background:false waits synchronously for the result; notify_on also notifies when the subagent's output matches. The instance name is addressable: SendMessage sends follow-up instructions (context preserved), AgentControl manages (list/stop/delete). The `agent` argument uses a named definition (preset system prompt and model); model/provider/thinking can be set per instance (defaulting to the named definition or parent session)."
            .to_string();
        // The crew is the reason not to reach for this tool, so it is named here and not
        // only in the system prompt: this description is where the list of definitions
        // tempts a second `dev` into existence beside the `dev` already standing by.
        if let Some(crew) = crate::team::load_team_file(&self.session.cwd())
            .ok()
            .flatten()
            .map(|team| team.name)
        {
            desc.push_str(&format!(
                "\n\nThis project has a standing crew ({crew}, see the system prompt's roster): \
                 give the work to a member with SendMessage first, and spawn here only for what \
                 no member covers. A spawn is a temporary hire — it never enters .bingo/team.json \
                 and is released once its task is done."
            ));
        }
        if !self.defs.is_empty() {
            desc.push_str("\n\nAvailable named definitions:");
            for def in &self.defs {
                desc.push_str(&format!("\n- {}: {}", def.name, def.description));
            }
        }
        desc
    }

    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<AgentInput>()
    }

    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }

    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        false
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: AgentInput = parse_input(&input)?;
        if self.session.depth >= MAX_AGENT_DEPTH {
            return Err(ToolError::failed(format!(
                "max agent depth ({MAX_AGENT_DEPTH}) exceeded"
            )));
        }
        // Async by default: the main agent does not wait for the sub-agent; the completion
        // notification is injected into the next turn.
        if params.background.unwrap_or(true) {
            return self.launch_background(&params, ctx);
        }

        let def = self.resolve_def(&params)?;
        let (name, description, sub_session) = self.spawn_instance(&params, def, &ctx.cwd)?;
        let _ = self.session.agents.next_run(&name);

        // Foreground sub-agents can also be watched: Running (characters produced) → Done/Failed.
        let cell = Arc::new(AgentCell::new(self.session.agents.clone()));
        let conditions = params
            .notify_on
            .clone()
            .map(|p| vec![NotifyCondition::Contains(p)])
            .unwrap_or_default();
        // A dispatch is always the caller's business: they asked for it.
        let id = register_run_watch(
            &ctx.watch,
            format!("{name} · {description}"),
            cell.clone(),
            conditions,
            ctx.instance.clone(),
            true,
            true,
        );
        self.session.agents.set_run_watch(&name, id);
        let output = Arc::new(Mutex::new(String::new()));
        let progress = Arc::new(Mutex::new(crate::agents::AgentProgress::default()));
        if let Ok(mut progress) = progress.lock() {
            progress.start_run();
        }
        self.session.agents.set_prompt(&name, params.prompt.clone());
        self.session
            .agents
            .set_progress(&name, Some(progress.clone()));
        let sink = self.session.agents.sink_for(&name);
        let turn = sink.clone().map(TurnBrackets::open);
        let mut ui = subagent_hooks(
            SubagentOutput {
                text: output.clone(),
                progress,
            },
            sink,
            cell.clone(),
            ctx.watch.clone(),
            id,
            name.clone(),
            self.session.agents.ask_fn(),
        );
        let images = self.session.attachments.resolve(&params.prompt);
        let sync_run = crate::query::run_query(
            &sub_session,
            Vec::new(),
            &params.prompt,
            &images,
            &mut ui,
            None,
        )
        .await;
        drop(turn);
        self.session.agents.set_progress(&name, None);
        match sync_run {
            Ok(outcome) => {
                let text = output.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let output_chars = text.chars().count();
                let content = non_empty(text);
                ctx.watch.set_state(
                    id,
                    WatchState::Done,
                    Some("done".to_string()),
                    Some(serde_json::json!(content.clone())),
                );
                // On the synchronous path tools run serially, so queued messages never reach here;
                // if one somehow does, hand it to the background loop (same continuation mechanism).
                if let Some(next) =
                    self.session
                        .agents
                        .finish(&name, outcome.messages, output_chars)
                {
                    let (prompt, images) = absorb_inbox(&sub_session.channels, &name, &next.items);
                    let items = next.items;
                    spawn_agent_loop(
                        self.session.agents.clone(),
                        ctx.watch.clone(),
                        name.clone(),
                        sub_session,
                        next.history,
                        prompt.clone(),
                        images,
                        items,
                        format!("{name} #{} · {}", next.run, excerpt(&prompt)),
                        next.run,
                        Vec::new(),
                        ctx.instance.clone(),
                        false,
                    );
                }
                Ok(ToolResult {
                    content: serde_json::Value::String(content),
                    is_error: false,
                    diff: None,
                })
            }
            Err(e) => {
                ctx.watch.set_state(
                    id,
                    WatchState::Failed,
                    Some(format!("subagent failed: {e}")),
                    None,
                );
                self.session.agents.mark_idle(&name);
                Err(ToolError::failed(format!("subagent failed: {e}")))
            }
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SendMessageInput {
    #[schemars(
        description = "Who to speak to, in the conversation namespace: an instance name (or @name) for an agent, #name for a room. Which of those you may address depends on who you are — see the tool description."
    )]
    to: String,
    #[schemars(description = "What to say")]
    message: String,
    /// One-line preview for the surface that draws the message (D108). CC's
    /// own field (`SendMessageTool.ts:76-81`), whose readers prefer it over
    /// truncating the body (`:765`). Optional here where CC's team path makes
    /// it mandatory, because bingo keeps CC's *fallback* too: a message without
    /// one still renders, as the first fifty columns of what it says.
    #[serde(default)]
    #[schemars(
        description = "A 5-10 word summary shown as a preview in the UI. Optional: without one the preview is the message's own first line, cut to fit."
    )]
    summary: Option<String>,
    /// Reply wait: arms the follow-up watchdog (see `spawn_ack_watchdog`).
    #[serde(default)]
    #[schemars(
        description = "Reply wait in seconds for a message to a subagent, defaulting to 300 when omitted — the check is on by default, since a message nobody ever answers is the failure you would otherwise find out about last. Once the wait elapses the harness re-checks the same record AgentControl(action=messages) reports, and while you are still owed an answer — the message is queued, or it was read into a turn that ended saying nothing — it sends the receiver a follow-up asking it to reply, at most 3 rounds; anything other than an answer inside the wait comes back to you as a task notification. Shorten it when you are actively waiting on this instance, lengthen it for a long task that will be quiet for a while (clamped to 5-3600), or pass 0 to switch the check off for a message you need no answer to. Ignored for a room and for a message to main: neither answers on a schedule."
    )]
    ack_timeout: Option<u64>,
    /// Attention request, subagent→main only (see [`SendMessageTool::call`]).
    #[serde(default)]
    #[schemars(
        description = "Ring the terminal's attention channel when this message lands (only when you are a subagent writing to main). Reserve it for something blocking that cannot wait for the user to look — it interrupts them wherever they are. Default false."
    )]
    urgent: bool,
}

/// Bounds on the reply wait: below the floor the watchdog would fire before the receiver could
/// plausibly finish a turn; the ceiling keeps a stray task from outliving the day.
const ACK_TIMEOUT_RANGE: std::ops::RangeInclusive<u64> = 5..=3600;

/// Reply wait when the sender names none. The check is on by default because leaving it opt-in
/// would put the correctness of the whole thing back where it does not belong — on the model
/// remembering to ask for it, which is the exact failure this mechanism exists to remove.
/// Five minutes is long enough that an instance genuinely working does not get chased for being
/// quiet, and short enough that a hang is not discovered by the user an hour later.
const DEFAULT_ACK_TIMEOUT_SECS: u64 = 300;

/// The one way any participant speaks to any conversation (D98).
///
/// Two semantics, one verb: deliver and wake. Who may be addressed narrows by
/// caller — main reaches any instance and any room it is in, a subagent reaches
/// `main` and the rooms it is a member of — so hub-and-spoke is preserved by
/// *addressing* rather than by a second tool. `Post` and `notify_user` retired
/// into this one.
pub struct SendMessageTool {
    session: Arc<Session>,
}

impl SendMessageTool {
    pub fn new(session: Arc<Session>) -> Self {
        Self { session }
    }

    /// This session's name in a conversation: a subagent's instance name, or
    /// `main`. Stamped by the runtime; the model cannot state it for itself.
    fn sender(&self) -> String {
        address::sender_of(&self.session)
    }

    /// Speak in a room: the retired `Post`'s path, unchanged.
    fn post(&self, ctx: &ToolContext, room: &str, message: &str) -> Result<ToolResult, ToolError> {
        let from = self.sender();
        match crate::tool::channel::deliver_post(&self.session, &ctx.watch, &from, room, message)
            .map_err(ToolError::failed)?
        {
            crate::tool::channel::PostDelivery::Sent {
                seq,
                unknown_mentions,
                undelivered_mentions,
            } => {
                let mut note = format!("sent (#{room} msg #{seq})");
                for name in &unknown_mentions {
                    note.push_str(&format!(
                        "\n@{name} is not in #{room}; that mention reached nobody"
                    ));
                }
                for name in &undelivered_mentions {
                    note.push_str(&format!(
                        "\n@{name} is stopped; the line is in the room log but was not delivered"
                    ));
                }
                Ok(ToolResult {
                    content: serde_json::Value::String(note),
                    is_error: false,
                    diff: None,
                })
            }
            crate::tool::channel::PostDelivery::Stale { missed } => {
                let lines: Vec<String> = missed
                    .iter()
                    .map(|m| format!("[#{room} msg #{}] {}: {}", m.seq, m.from, m.text))
                    .collect();
                Ok(ToolResult {
                    content: serde_json::Value::String(format!(
                        "not sent — the room got new messages while you were drafting:\n{}\n\
Decide again from the latest content, and the default is to drop: if what landed already covers \
your message — a colleague answered the same broadcast, reported the same result, said the same \
hello — the room does not need it twice, and dropping it IS the answer. Resend (call again \
unchanged) or edit and resend only when yours still adds something the room has not heard.",
                        lines.join("\n")
                    )),
                    is_error: false,
                    diff: None,
                })
            }
        }
    }

    /// Speak to main: into its inbox, drained at its next turn boundary.
    ///
    /// There is no delivery record and no chase, because main is not an instance
    /// in the registry — it is the host turn loop. What answers a message here is
    /// main saying something, which the user reads in the conversation they are
    /// already in.
    fn to_main(&self, message: &str, summary: Option<&str>, urgent: bool) -> ToolResult {
        let from = self.sender();
        self.session
            .channels
            .deliver_to_main(&from, message, summary, urgent);
        ToolResult {
            content: serde_json::json!({
                "status": "queued",
                "to": crate::channels::MAIN_NAME,
                "from": from,
                "urgent": urgent,
                "note": "in main's inbox, read at its next turn boundary; it starts one now if it is idle. \
Main answers by speaking to the user, not by replying to you — there is no receipt to wait for.",
            })
            .to_string()
            .into(),
            is_error: false,
            diff: None,
        }
    }
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> String {
        "SendMessage".to_string()
    }
    fn description(&self) -> String {
        let me = self.sender();
        let rooms = if address::rooms_allowed(&self.session) {
            "; `#room` for a room you are a member of (every member's inbox gets it, in one order; \
`@name` inside the text asks that member for an answer and wakes them now, `@all` asks the room \
and wakes everyone, and a line naming nobody owes nothing and is read in batches — so spend the \
`@` on what you need answered, not on what you want read; in a serial room a stale send bounces \
back with what you missed attached)"
        } else {
            ""
        };
        let reach = if self.session.depth == 0 {
            "You may write to any subagent instance (the name the Agent tool returned, or AgentControl list)".to_string()
        } else {
            "You may write to `main` and to nothing else in the agent namespace: work that concerns another agent goes through main, or into a room you are both in".to_string()
        };
        let lane = if self.session.depth == 0 {
            "This is the private lane: right for what concerns the receiver alone — an assignment, a follow-up, a correction. Something every member of a room should act on belongs in one room message, not in per-member private copies that drift apart. \
An idle receiver starts immediately; a running one drains everything waiting at its next tool round, batched into one prompt. Neither queued nor delivered is an acknowledgement — a receiver can read a message and end its turn saying nothing — so the harness re-checks five minutes after sending (tune with ack_timeout) and follows up, up to 3 rounds, reporting back if no answer ever comes; AgentControl(action=messages) shows the same record on demand."
        } else {
            "Writing to main is deliberate, not routine: your ordinary work is already visible to whoever is watching your conversation, and your final text is returned to whoever started you. Send when the overall task is finished, when you are blocked and need a decision, or when you found something that changes what is being coordinated — not for progress, acknowledgements, or anything already in your reply. \
Write a summary: it is the single line the user's transcript shows for your message, and without one they read the first fifty columns of the message itself. \
Set urgent only for something blocking that cannot wait for the user to look: it rings the terminal's attention channel, which interrupts them wherever they are."
        };
        format!(
            "Speak to one conversation. Your name in it is {me} (stamped by the runtime; it cannot be forged). \
`to` is the conversation namespace: an instance name or `@name` for an agent{rooms}. \
{reach}. \
{lane}"
        )
    }
    /// The schema, minus what this caller's message would not be previewed by.
    ///
    /// `summary` is drawn only for a message *from* an agent — the `@name❯`
    /// line (D106) and the tree preview (D104) — so main's own sends have no
    /// surface for it and it is left off the schema main assembles rather than
    /// advertised and ignored. [`SendMessageInput`] still accepts it at every
    /// depth: `deny_unknown_fields` would turn a harmless word into an error.
    fn input_schema(&self) -> serde_json::Value {
        let mut schema = super::schema_for::<SendMessageInput>();
        if self.session.depth == 0
            && let Some(props) = schema
                .get_mut("properties")
                .and_then(serde_json::Value::as_object_mut)
        {
            props.remove("summary");
        }
        schema
    }
    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }
    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        false
    }
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: SendMessageInput = parse_input(&input)?;
        let address = address::parse_address(&params.to)?;
        address::check_target(&self.session, &address)?;
        // The bell is the harness's and it has exactly one meaning: an agent
        // needs the user. Main speaking to a subagent, or anyone speaking to a
        // room, has no user on the other end to interrupt — refused rather than
        // ignored, so a model that reaches for it learns the shape of the tool.
        let sub_to_main =
            self.session.depth > 0 && address == Address::Agent(crate::channels::MAIN_NAME.into());
        if params.urgent && !sub_to_main {
            return Err(ToolError::failed(
                "urgent only applies when a subagent writes to main — it rings the user's attention channel, and nobody else is on the other end of this message",
            ));
        }
        let room = match address {
            Address::Room(room) => room,
            Address::Agent(_) if sub_to_main => {
                return Ok(self.to_main(&params.message, params.summary.as_deref(), params.urgent));
            }
            Address::Agent(agent) => return self.to_agent(ctx, &agent, &params).await,
        };
        self.post(ctx, &room, &params.message)
    }
}

impl SendMessageTool {
    /// Main→instance: the continuation channel, with the acknowledgement
    /// watchdog it has carried since D44.
    async fn to_agent(
        &self,
        ctx: &ToolContext,
        agent: &str,
        params: &SendMessageInput,
    ) -> Result<ToolResult, ToolError> {
        let images = self.session.attachments.resolve(&params.message);
        let timeout = match params.ack_timeout {
            // The one way to opt out: an explicit "I am not waiting for an answer to this".
            Some(0) => None,
            Some(secs) => Some(std::time::Duration::from_secs(
                secs.clamp(*ACK_TIMEOUT_RANGE.start(), *ACK_TIMEOUT_RANGE.end()),
            )),
            None => Some(std::time::Duration::from_secs(DEFAULT_ACK_TIMEOUT_SECS)),
        };
        let id = self
            .session
            .agents
            .deliver(agent, &self.sender(), &params.message, images, timeout)
            .map_err(ToolError::failed)?;
        flush_agent_inbox(&self.session, &ctx.watch);
        let note = match timeout {
            Some(t) => format!(
                "enqueued for immediate processing; the receiver batches everything waiting when it next drains its inbox; if no reply arrives within {}s (including read-but-silent rounds), it is automatically re-checked and chased (up to {MAX_FOLLOW_UPS} rounds); the outcome is reported as a task notification",
                t.as_secs()
            ),
            None => "enqueued for immediate processing; the receiver batches everything waiting when it next drains its inbox; follow-up chasing is off (ack_timeout=0); check yourself with AgentControl(action=messages, agent=…) when needed"
                .to_string(),
        };
        if let Some(timeout) = timeout {
            spawn_ack_watchdog(
                self.session.clone(),
                ctx.watch.clone(),
                agent.to_string(),
                id,
                timeout,
            );
        }
        Ok(ToolResult {
            content: serde_json::json!({
                "status": "queued",
                "message_id": id.0,
                "to": agent,
                "ack_timeout_secs": timeout.map(|t| t.as_secs()),
                "note": note,
            })
            .to_string()
            .into(),
            is_error: false,
            diff: None,
        })
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AgentAction {
    /// List all instances (name/definition/status/pending message count).
    List,
    /// Delivery records for one instance: which messages were delivered, are still queued, or
    /// were dropped.
    Messages,
    /// Stop: abort the current run and stop accepting messages; history is kept and can be listed.
    Stop,
    /// Delete: stop and remove the instance (name released).
    Delete,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct AgentControlInput {
    #[schemars(
        description = "Action: list all instances / check message delivery / stop one / delete one"
    )]
    action: AgentAction,
    #[serde(default)]
    #[schemars(description = "Target instance name (required for messages/stop/delete)")]
    agent: Option<String>,
}

/// Sub-agent lifecycle management (hub-and-spoke, main session only).
pub struct AgentControlTool {
    session: Arc<Session>,
}

impl AgentControlTool {
    pub fn new(session: Arc<Session>) -> Self {
        Self { session }
    }

    /// Watchdog state of a message still owed an answer: empty when the sender armed none, so a
    /// fire-and-forget listing reads exactly as it did before.
    fn chase_note(ack: &crate::agents::Ack) -> String {
        let Some(timeout) = ack.timeout else {
            return String::new();
        };
        let sent = match ack.follow_ups {
            0 => String::new(),
            n => format!(", chased {n}/{MAX_FOLLOW_UPS}"),
        };
        format!(
            ", auto re-check after {}s without a reply{sent}",
            timeout.as_secs()
        )
    }

    fn require_agent(input: &AgentControlInput) -> Result<&str, ToolError> {
        input
            .agent
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ToolError::failed(
                    "messages/stop/delete require the agent parameter (instance name)",
                )
            })
    }
}

pub(crate) fn format_last_active(age: std::time::Duration) -> String {
    let seconds = age.as_secs();
    if seconds == 0 {
        "active now".to_string()
    } else if seconds < 60 {
        format!("active {seconds}s ago")
    } else if seconds < 3_600 {
        format!("active {}min ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("active {}h ago", seconds / 3_600)
    } else {
        format!("active {}d ago", seconds / 86_400)
    }
}

#[async_trait]
impl Tool for AgentControlTool {
    fn name(&self) -> String {
        "AgentControl".to_string()
    }
    fn description(&self) -> String {
        "Manage subagent instances: list all (name/definition/status/last activity/queued-instruction count), check messages sent to one (per-message queued/delivered-but-unanswered/answered/dropped, how long it has been waiting, and whether SendMessage's ack_timeout is already chasing it — use this when an instance has gone quiet on you), stop one (aborts the current run, stops accepting instructions; history kept), delete one (stops and removes it; the name is released).".to_string()
    }
    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<AgentControlInput>()
    }
    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }
    fn is_read_only(&self, input: &serde_json::Value) -> bool {
        matches!(
            input.get("action").and_then(|a| a.as_str()),
            Some("list") | Some("messages")
        )
    }
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: AgentControlInput = parse_input(&input)?;
        let registry = &self.session.agents;
        let text = match params.action {
            AgentAction::List => {
                let statuses = registry.list();
                if statuses.is_empty() {
                    "no subagent instances right now".to_string()
                } else {
                    statuses
                        .iter()
                        .map(|s| {
                            let def = s
                                .def
                                .as_deref()
                                .map(|d| format!(", definition {d}"))
                                .unwrap_or_default();
                            let pending = if s.pending > 0 {
                                format!(", {} instructions queued", s.pending)
                            } else {
                                String::new()
                            };
                            let unacked = if s.unacked > 0 {
                                format!(", {} unacknowledged", s.unacked)
                            } else {
                                String::new()
                            };
                            let active = format_last_active(s.last_active.elapsed());
                            format!(
                                "- {} ({} {}, {active}{def}{pending}{unacked}, {} @ {}): {}",
                                s.name,
                                s.kind.label(),
                                s.state.label(),
                                s.model,
                                s.provider,
                                s.description
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            AgentAction::Messages => {
                let name = Self::require_agent(&params)?;
                let acks = registry
                    .acks_of(name)
                    .ok_or_else(|| ToolError::failed(format!("no subagent named {name}")))?;
                if acks.is_empty() {
                    format!("no messages sent to {name} yet")
                } else {
                    acks.iter()
                        .map(|a| {
                            let detail = match &a.state {
                                crate::agents::AckState::Queued => format!(
                                    "queued (waiting {}s{}, the receiver will claim it immediately when idle or at its next tool round)",
                                    a.queued_at.elapsed().as_secs(),
                                    Self::chase_note(a)
                                ),
                                crate::agents::AckState::Delivered { run } => format!(
                                    "delivered (read into the context in round {run}, but that round did not answer{})",
                                    Self::chase_note(a)
                                ),
                                crate::agents::AckState::Answered { run } => {
                                    format!("answered (replied in round {run})")
                                }
                                crate::agents::AckState::Dropped { reason } => {
                                    format!("dropped ({reason}, never delivered)")
                                }
                            };
                            format!("- #{} {detail}: {}", a.id, a.excerpt)
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            AgentAction::Stop => {
                let name = Self::require_agent(&params)?;
                let (watch_id, dropped) = registry.stop(name).map_err(ToolError::failed)?;
                let lost = if dropped > 0 {
                    format!(", {dropped} undelivered instructions discarded")
                } else {
                    String::new()
                };
                match watch_id {
                    Some(id) => {
                        ctx.watch.set_state(
                            id,
                            WatchState::Cancelled,
                            Some("stopped".to_string()),
                            None,
                        );
                        format!("stopped {name} (current run aborted, history kept{lost})")
                    }
                    None => format!("{name} stopped (no run in progress{lost})"),
                }
            }
            AgentAction::Delete => {
                let name = Self::require_agent(&params)?;
                self.session.channels.remove_member_everywhere(name);
                let (watch_id, dropped) = registry.remove(name).map_err(ToolError::failed)?;
                // The ack trail is removed with the instance, so this count is the sender's
                // last chance to learn that queued instructions never landed.
                let lost = if dropped > 0 {
                    format!(", {dropped} undelivered instructions discarded")
                } else {
                    String::new()
                };
                match watch_id {
                    Some(id) => {
                        ctx.watch.set_state(
                            id,
                            WatchState::Cancelled,
                            Some("deleted".to_string()),
                            None,
                        );
                        format!("deleted {name} (run aborted, name released{lost})")
                    }
                    None => format!("deleted {name} (name released{lost})"),
                }
            }
        };
        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: false,
            diff: None,
        })
    }
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
