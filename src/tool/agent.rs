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
use crate::watch::{NotifyCondition, WatchId, WatchKind, WatchRegistry, WatchState};

const MAX_AGENT_DEPTH: usize = 3;

/// Appended to every sub-agent's system prompt. The base prompt is written for the session that
/// owns the terminal, and two of its promises do not hold here: rendering images for the user,
/// and being woken by background-task notifications. Say so rather than letting the model plan
/// against a surface it does not have.
///
/// The DM bullet exists because the user has a real private line to every instance (D57's
/// workspace and the main-chat selector), and its messages arrive indistinguishable from the
/// hub's. A note that claims the user never sees the turn text leaves exactly one imaginable
/// way to reach them — a channel Post — which is how a private question ends up answered in
/// front of the whole room (D63).
const SUBAGENT_NOTE: &str = "\
# You are a subagent

- The main agent (the hub) spawned you for one task. Your final text is returned to the hub
  as its tool result; it does not appear in the user's main transcript, and markdown image
  blocks are not rendered for anyone. Put conclusions in the text itself.
- The user has a private direct-message window with you. A message they send there arrives
  under a `[DM from user]` line; a direct instruction without that line is from the hub.
  Either way the prose of your turns is exactly what the sender reads back — a direct
  message is answered where it arrived, in your turn text.
- You cannot question the user: AskUserQuestion is not available here. Permission prompts do
  reach the user, but anything else you need must be reported back to the hub.
- `SendMessage(to: \"main\")` is your one deliberate way to reach the hub *between* turns —
  for the overall task being finished, for being blocked on a decision, for a finding that
  changes what is being coordinated. It is not for progress, acknowledgements, or anything
  already in your reply: your work is visible in your DM and your final text is returned to
  whoever started you. `urgent: true` interrupts the user wherever they are; reserve it.
- Your turn ends when you stop calling tools, and background tasks you started will NOT wake
  you afterwards. Finish what needs finishing within this turn, or state what is still
  pending — the hub can resume you with a follow-up message.";

/// Appended when agent channels are on. Three failure modes pull against each other and the
/// note has to hold all of them: a room of polite agents acknowledging each other's
/// acknowledgements (D45), a room so afraid of chatter that nobody answers the human at all
/// (D48), and a member answering a private DM with a channel Post because `user` only ever
/// appeared in this note as a room speaker (D63).
///
/// The rule that separates the first two is *who spoke*, not how the message reads — a person
/// answers their manager and ignores their colleagues' hellos — plus the mechanical fact the
/// model cannot infer: a turn woken by a channel message reports back to the hub, so a reply
/// written as turn text never reaches the room. Without that sentence the model believes it has
/// already answered and stays silent on purpose. The third failure mode needs the opposite
/// mechanical fact: *where* a message arrived decides where the answer goes, and the only
/// observable difference is the `[#channel msg #N]` tag on channel traffic.
///
/// The first three rules all govern replies, which left initiated messages lawless (D67): a
/// member that *discovered* something team-wide had no rule sending it to the room — it went to
/// the hub as turn text and the team worked on stale ground — while the symmetric mistake,
/// narrating personal progress into the room, is D45's flood through a new door. The venue rule
/// closes both at once, so its two halves must stay together.
///
/// It lives in the system prompt rather than in the wake-up payload deliberately: compaction
/// rewrites the message history but never touches `Session::system`, so the rule is still there
/// on turn fifty, when a long-running member has forgotten everything else about the room.
const CHANNEL_NOTE: &str = "\
# Speaking in a channel

**Only `SendMessage(to: \"#channel\")` puts words in the room.** The text you write in a turn woken
by a channel message goes back to the hub as your result — nobody in the channel sees it. Writing
\"standing by, no channel reply needed\" as your turn text is not an answer to the room; it is a
private note to your manager, and from the room it is indistinguishable from ignoring the message.
If you decide to answer, send it to the room.

**Who spoke decides whether you owe a reply** — not how the message is worded.

- **`user` or `main` addressed the room**: answer once, briefly, to the room. When the person
  running the room greets the team, asks who is around, or puts a question to everyone, a human
  answers — silence reads as absence, not as discipline. One short line, in your own voice, then
  stop.
- **Another member spoke**: you owe them nothing. Send only if they named you, you can unblock
  them, you disagree, or you are holding the result they are waiting on.
- **Never answer an answer.** A room does not flood because members reply to the human; it floods
  because they reply to each other's replies. Your line is the end of that thread — do not
  acknowledge, thank, agree with, or restate what a colleague just said.

Beyond that first line, send to the room only what changes what someone else will do: a decision
someone is blocked on, a disagreement, a result, a question you cannot continue without. Name the
person you mean. When you have nothing to add, stop calling tools — silence costs nothing and wakes
nobody.

**The audience decides the lane — for what you initiate, not only for replies.** When your work
surfaces something that changes what other members will do — a contract or interface change, a
shared blocker, a hazard someone is about to walk into — take it to the room
without waiting to be asked: reporting it only to the hub in your turn text leaves the team
working on stale ground. What
concerns nobody but you and the hub — your progress, partial results, questions only the hub can
answer — stays in your turn text: the room's attention is the scarcest thing in it.

**A direct message is a different lane, and a private one.** Channel traffic arrives tagged
`[#channel msg #N]`; text without that tag was sent to you alone — under a `[DM from user]`
line when the user wrote it in your direct-message window, unmarked when it is the hub. Your
turn text is exactly what the sender reads. Answer a direct message in your turn text —
never in a room: the answer belongs to the person who asked, not to the room. What reaches
you privately stays private — do not repeat or summarize it into a channel unless the
message itself tells you to take it there. When something private has to reach the hub between
turns rather than at the end of one, that is `SendMessage(to: \"main\")`, never a room.";

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
    live: Arc<Mutex<Vec<crate::agents::LiveBlock>>>,
    progress: Arc<Mutex<crate::agents::AgentProgress>>,
}

/// Sub-agent UI: captures text, renders nothing, and forwards permission prompts to the
/// session that owns the UI. The cell tracks the number of characters produced (for interval
/// progress checks of background agents).
/// Snapshot of everything a subagent's live view accumulated up to the last committed round;
/// a stream retry rolls the failed attempt back to this point.
#[derive(Clone, Default)]
struct AttemptCheckpoint {
    text_len: usize,
    live: Vec<crate::agents::LiveBlock>,
    produced_chars: usize,
    output_tokens: u64,
    tool_uses: usize,
    recent_activity: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
fn subagent_hooks(
    output: SubagentOutput,
    token_rate: Arc<Mutex<crate::token_rate::TokenRateSampler>>,
    cell: Arc<AgentCell>,
    watch: Arc<WatchRegistry>,
    id: WatchId,
    instance: String,
    ask: Option<Arc<crate::query::AskFn>>,
) -> UiHooks {
    // `output` stays the flat reply (what the spawn returns and what `spoke` is
    // judged on); `live` is the same turn as the instance view needs to show it,
    // with the tool calls and round boundaries the flat string cannot carry.
    let tool_live = output.live.clone();
    let tool_progress = output.progress.clone();
    let retry_live = output.live.clone();
    let retry_text = output.text.clone();
    let retry_progress = output.progress.clone();
    let warning_live = output.live.clone();
    let round_live = output.live.clone();
    let round_text = output.text.clone();
    let round_live_checkpoint = output.live.clone();
    let round_progress = output.progress.clone();
    let text_output = output.text;
    let live_output = output.live;
    let progress_output = output.progress;
    let registry = cell.registry.clone();
    let event_registry = registry.clone();
    let event_instance = instance.clone();
    let tool_registry = registry.clone();
    let tool_instance = instance.clone();
    let done_registry = registry.clone();
    let done_instance = instance.clone();
    let round_rate = token_rate.clone();
    let retry_rate = token_rate.clone();
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
                        if let Ok(mut live) = live_output.lock() {
                            crate::agents::LiveBlock::push_text(&mut live, text);
                        }
                        event_cell.record_chars(text.chars().count());
                        // Feed produced text into the condition engine (notify_on hit → signal notification).
                        watch.feed_content(id, text);
                    }
                    tokens
                }
                crate::api::contract::StreamEvent::ThinkingDelta { thinking, .. } => {
                    event_registry.touch(&event_instance);
                    // The DM view shows the phase, not the stream: the block marks
                    // "reasoning happened here" the way the transcript's collapsed
                    // `✻ Thinking` row does.
                    if let Ok(mut live) = live_output.lock() {
                        crate::agents::LiveBlock::push_thinking(&mut live, thinking);
                    }
                    let mut units = event_round_units.lock().unwrap_or_else(|e| e.into_inner());
                    *units = units.saturating_add(crate::compact::text_units(thinking));
                    units.div_ceil(4)
                }
                crate::api::contract::StreamEvent::InputJsonDelta { partial_json, .. } => {
                    let mut units = event_round_units.lock().unwrap_or_else(|e| e.into_inner());
                    *units = units.saturating_add(crate::compact::text_units(partial_json));
                    units.div_ceil(4)
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
                    // Accounting correction, not freshly streamed output: fed as a
                    // sample it rendered as a one-frame rate spike (see UiEvent::OutputTokens).
                    if let Ok(mut sampler) = token_rate.lock() {
                        sampler.correct_round(*tokens, std::time::Instant::now());
                    }
                    return;
                }
                _ => return,
            };
            if let Ok(mut sampler) = token_rate.lock() {
                sampler.observe_round(tokens, std::time::Instant::now());
            }
        }),
        on_stream_retry: Box::new(move || {
            *retry_units.lock().unwrap_or_else(|e| e.into_inner()) = 0;
            if let Ok(mut sampler) = retry_rate.lock() {
                sampler.retry_round();
            }
            let checkpoint = retry_checkpoint
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Ok(mut text) = retry_text.lock() {
                text.truncate(checkpoint.text_len);
            }
            if let Ok(mut live) = retry_live.lock() {
                *live = checkpoint.live;
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
        // An instance's context usage had exactly one display, the workspace
        // DM composer's footer, and it retired with the workspace (D89). The
        // hook stays wired so the contract is unchanged and a later surface can
        // read it again without re-plumbing the callback.
        on_context_usage: Arc::new(move |_usage| {}),
        on_tool_ready: Box::new(move |_tool_call_id, name, input, _standalone| {
            tool_registry.touch(&tool_instance);
            let glyph = crate::tui::activities::tool_glyph(&name);
            let shown = crate::tui::activities::display_tool_name(&name);
            let summary = crate::query::summarize_input(&name, &input);
            let activity = if summary.is_empty() {
                format!("{glyph}{shown}")
            } else {
                format!("{glyph}{shown}({summary})")
            };
            if let Ok(mut live) = tool_live.lock() {
                live.push(crate::agents::LiveBlock::Tool(activity.clone()));
            }
            if let Ok(mut progress) = tool_progress.lock() {
                progress.record_tool(activity);
            }
        }),
        on_tool_done: Box::new(move |_| done_registry.touch(&done_instance)),
        // A round boundary closes the open prose block, so the next round's first
        // sentence does not run into the previous round's last one.
        on_round_end: Box::new(move || {
            *round_units.lock().unwrap_or_else(|e| e.into_inner()) = 0;
            if let Ok(mut sampler) = round_rate.lock() {
                sampler.finish_round();
            }
            if let Ok(mut live) = round_live.lock()
                && matches!(live.last(), Some(crate::agents::LiveBlock::Text(_)))
            {
                live.push(crate::agents::LiveBlock::Text(String::new()));
            }
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
                live: round_live_checkpoint
                    .lock()
                    .map(|live| live.clone())
                    .unwrap_or_default(),
                produced_chars: round_cell.chars(),
                output_tokens,
                tool_uses,
                recent_activity,
            };
        }),
        on_warning: Box::new(move |message| {
            if message.starts_with(crate::query::RECONNECT_WARNING_PREFIX)
                && let Ok(mut live) = warning_live.lock()
            {
                live.retain(|block| {
                    !matches!(block, crate::agents::LiveBlock::Text(text) if text.starts_with(crate::query::RECONNECT_WARNING_PREFIX))
                });
                live.push(crate::agents::LiveBlock::Text(message));
                live.push(crate::agents::LiveBlock::Text(String::new()));
            }
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
        );
    }
}

/// Acknowledgement watchdog for one message: the sender named a wait, so when that wait elapses
/// this re-reads the very record `AgentControl(action=messages)` reports and, while the message
/// still has not entered the receiver's context, nudges the receiver and retries the boundary
/// flush — the automatic form of the poll the hub would otherwise have to run by hand.
///
/// What it waits for is an *answer*, not a delivery. Reading a message into a prompt proves
/// nothing about the receiver: an instance can take the message, run a turn and end it without a
/// word, which from the sender's side is indistinguishable from a hang. So `Delivered` is chased
/// exactly like `Queued`, and only `Answered` stops the clock.
///
/// Two bounds keep it a mechanism rather than a loop: at most `MAX_FOLLOW_UPS` rounds, and every
/// outcome except an answer inside the wait is reported back to the sender as a watch line, whose
/// terminal state reaches the hub's next turn. A chase that never gives up and never speaks would
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
/// hub stays untagged — it is the default voice of direct instructions — so the marker is
/// the one observable difference between "your manager" and "the human", and the DM view
/// drops the line rather than rendering scaffolding as prose.
pub(crate) const DM_FROM_USER_MARKER: &str = "[DM from user]";

/// The user's messages carry the marker in every shape of batch; the hub's only gain their
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

/// Inbox → turn prompt plus the images those instructions carried: a single hub instruction is
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
            InboxItem::Channel { .. } | InboxItem::FollowUp { .. } => None,
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
                        "[follow-up {round}/{MAX_FOLLOW_UPS}] The hub sent you message \
                         #{original} (\"{excerpt}\") {}s ago and has had no reply: {silence}. \
                         Answer it now — if you are still working, say what you are doing and \
                         what you have so far; if you have nothing to add, say that. Ending a \
                         turn in silence reads as a hang from the outside.",
                        waited.as_secs()
                    )
                }
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
fn register_run_watch(
    watch: &Arc<WatchRegistry>,
    label: String,
    cell: Arc<AgentCell>,
    conditions: Vec<NotifyCondition>,
    owner: Option<String>,
    notify_owner: bool,
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
    );
    registry.set_run_watch(&name, first_id);
    registry.set_run_trigger(&name, wakes_owner_first);
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
            let live = Arc::new(Mutex::new(Vec::new()));
            let progress = Arc::new(Mutex::new(crate::agents::AgentProgress::default()));
            if let Ok(mut progress) = progress.lock() {
                progress.start_run();
            }
            let token_rate = Arc::new(Mutex::new(crate::token_rate::TokenRateSampler::default()));
            if let Ok(mut sampler) = token_rate.lock() {
                sampler.start(std::time::Instant::now());
            }
            loop_registry.set_prompt(&name, prompt.clone());
            loop_registry.set_live(&name, Some(live.clone()), Some(token_rate.clone()));
            loop_registry.set_progress(&name, Some(progress.clone()));
            let mut ui = subagent_hooks(
                SubagentOutput {
                    text: output.clone(),
                    live: live.clone(),
                    progress,
                },
                token_rate,
                run.1.clone(),
                watch.clone(),
                run.0,
                name.clone(),
                loop_registry.ask_fn(),
            );
            match crate::query::run_query(&session, history, &prompt, &images, &mut ui, None).await
            {
                Ok(outcome) => {
                    let text = output.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let output_chars = text.chars().count();
                    loop_registry.set_live(&name, None, None);
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
                            let id = register_run_watch(
                                &watch,
                                label,
                                cell.clone(),
                                Vec::new(),
                                owner.clone(),
                                wakes,
                            );
                            loop_registry.set_run_watch(&name, id);
                            loop_registry.set_run_trigger(&name, wakes);
                            run = (id, cell);
                        }
                        None => break,
                    }
                }
                Err(e) => {
                    loop_registry.set_live(&name, None, None);
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
        );
        self.session.agents.set_run_watch(&name, id);
        let output = Arc::new(Mutex::new(String::new()));
        let live = Arc::new(Mutex::new(Vec::new()));
        let progress = Arc::new(Mutex::new(crate::agents::AgentProgress::default()));
        if let Ok(mut progress) = progress.lock() {
            progress.start_run();
        }
        let token_rate = Arc::new(Mutex::new(crate::token_rate::TokenRateSampler::default()));
        if let Ok(mut sampler) = token_rate.lock() {
            sampler.start(std::time::Instant::now());
        }
        self.session.agents.set_prompt(&name, params.prompt.clone());
        self.session
            .agents
            .set_live(&name, Some(live.clone()), Some(token_rate.clone()));
        self.session
            .agents
            .set_progress(&name, Some(progress.clone()));
        let mut ui = subagent_hooks(
            SubagentOutput {
                text: output.clone(),
                live: live.clone(),
                progress,
            },
            token_rate,
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
        self.session.agents.set_live(&name, None, None);
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

/// A resolved `to`: one of the two kinds of conversation there are.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Address {
    /// An agent instance, `main` included.
    Agent(String),
    /// A room, named without its `#`.
    Room(String),
}

/// Read the `to` field as the conversation namespace the interface already uses:
/// `#name` is a room, anything else is an agent and may wear a leading `@`.
///
/// One address language for the tool layer and the display layer, so an agent
/// naming a target says what the user's own composer says (D90's `@name ` /
/// `#name ` routing) and what the bar shows.
pub(crate) fn parse_address(to: &str) -> Result<Address, ToolError> {
    let to = to.trim();
    if let Some(room) = to.strip_prefix('#') {
        let room = room.trim();
        if room.is_empty() {
            return Err(ToolError::failed("`to` is `#` with no room name after it"));
        }
        return Ok(Address::Room(room.to_string()));
    }
    let agent = to.strip_prefix('@').unwrap_or(to).trim();
    if agent.is_empty() {
        return Err(ToolError::failed(
            "`to` is empty; name an agent instance (or @name), or a room as #name",
        ));
    }
    Ok(Address::Agent(agent.to_string()))
}

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
        self.session
            .instance
            .clone()
            .unwrap_or_else(|| crate::channels::HUB_NAME.to_string())
    }

    /// Whether this caller may address rooms at all. Rooms are still behind the
    /// `experimental.agentChannels` gate, and the cohort that could hold a room
    /// membership is the same one the retired `Post` was assembled for: the main
    /// session, and named direct subagents.
    fn rooms_allowed(&self) -> bool {
        self.session.settings.experimental.agent_channels
            && (self.session.depth == 0
                || (self.session.depth == 1 && self.session.instance.is_some()))
    }

    /// Refuse a target this caller has no business addressing, in words that say
    /// what it may address instead.
    fn check_target(&self, address: &Address) -> Result<(), ToolError> {
        let me = self.sender();
        match address {
            Address::Agent(name) if *name == me => Err(ToolError::failed(format!(
                "{name} is you — a message to yourself is a note, not a message"
            ))),
            Address::Agent(name) => {
                if self.session.depth == 0 || name == crate::channels::HUB_NAME {
                    Ok(())
                } else {
                    Err(ToolError::failed(format!(
                        "you may not message {name}: as a subagent you can write to main, and to rooms you are a member of. \
Work that concerns another agent goes through main, or into a room you are both in."
                    )))
                }
            }
            Address::Room(room) => {
                if !self.rooms_allowed() {
                    return Err(ToolError::failed(format!(
                        "rooms are not available to you; #{room} cannot be addressed from here"
                    )));
                }
                if !self.session.channels.is_member(room, &me) {
                    return Err(ToolError::failed(format!(
                        "you are not a member of #{room} — join the room before speaking in it"
                    )));
                }
                Ok(())
            }
        }
    }

    /// Speak in a room: the retired `Post`'s path, unchanged.
    fn post(&self, ctx: &ToolContext, room: &str, message: &str) -> Result<ToolResult, ToolError> {
        let from = self.sender();
        match crate::tool::channel::deliver_post(&self.session, &ctx.watch, &from, room, message)
            .map_err(ToolError::failed)?
        {
            crate::tool::channel::PostDelivery::Sent { seq } => Ok(ToolResult {
                content: serde_json::Value::String(format!("sent (#{room} msg #{seq})")),
                is_error: false,
                diff: None,
            }),
            crate::tool::channel::PostDelivery::Stale { missed } => {
                let lines: Vec<String> = missed
                    .iter()
                    .map(|m| format!("[#{room} msg #{}] {}: {}", m.seq, m.from, m.text))
                    .collect();
                Ok(ToolResult {
                    content: serde_json::Value::String(format!(
                        "not sent — the room got new messages while you were drafting:\n{}\n\
Decide again from the latest content: resend as-is (call again unchanged), edit and resend, or drop the message.",
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
    fn to_main(&self, message: &str, urgent: bool) -> ToolResult {
        let from = self.sender();
        self.session
            .channels
            .deliver_to_main(&from, message, urgent);
        ToolResult {
            content: serde_json::json!({
                "status": "queued",
                "to": crate::channels::HUB_NAME,
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
        let rooms = if self.rooms_allowed() {
            "; `#room` for a room you are a member of (every member's context gets it, in one order; in a serial room a stale send bounces back with what you missed attached)"
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
            "Writing to main is deliberate, not routine: your ordinary work is already visible in your DM with the user, and your final text is returned to whoever started you. Send when the overall task is finished, when you are blocked and need a decision, or when you found something that changes what is being coordinated — not for progress, acknowledgements, or anything already in your reply. \
Set urgent only for something blocking that cannot wait for the user to look: it rings the terminal's attention channel, which interrupts them wherever they are."
        };
        format!(
            "Speak to one conversation. Your name in it is {me} (stamped by the runtime; it cannot be forged). \
`to` is the conversation namespace: an instance name or `@name` for an agent{rooms}. \
{reach}. \
{lane}"
        )
    }
    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<SendMessageInput>()
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
        let address = parse_address(&params.to)?;
        self.check_target(&address)?;
        // The bell is the harness's and it has exactly one meaning: an agent
        // needs the user. Main speaking to a subagent, or anyone speaking to a
        // room, has no user on the other end to interrupt — refused rather than
        // ignored, so a model that reaches for it learns the shape of the tool.
        let sub_to_main =
            self.session.depth > 0 && address == Address::Agent(crate::channels::HUB_NAME.into());
        if params.urgent && !sub_to_main {
            return Err(ToolError::failed(
                "urgent only applies when a subagent writes to main — it rings the user's attention channel, and nobody else is on the other end of this message",
            ));
        }
        let room = match address {
            Address::Room(room) => room,
            Address::Agent(_) if sub_to_main => {
                return Ok(self.to_main(&params.message, params.urgent));
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
mod tests {
    use super::*;
    use crate::query::{Runtime, Session};

    /// The exact capability block a subagent with the given (unknown-to-the-
    /// table → conservative defaults) model carries. Unknown models keep the
    /// default: vision yes, thinking yes.
    fn capability_block(model: &str, provider: &str) -> String {
        format!(
            "{}\nActive model: {model} (provider: {provider})\n- Vision: yes — accepts image input; \
             you can act on screenshots and rendered output\n- Thinking: yes — bingo may send \
             thinking parameters for this model",
            crate::system::MODEL_CAPABILITIES_HEADING
        )
    }

    fn parent_session() -> (Arc<Session>, Arc<crate::api::client::Client>) {
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-parent".into()),
            api_base_url: Some("https://parent.example".into()),
            // Explicitly opted out: models a compat proxy that speaks the protocol but rejects
            // image blocks. Image support is otherwise the default.
            send_images: Some(false),
            ..Default::default()
        };
        settings.providers.insert(
            "ds".to_string(),
            crate::settings::ProviderConfig {
                env_key: None,
                models: None,
                api_key: Some("sk-ds".into()),
                api_base_url: "https://api.deepseek.com".into(),
                supports_images: None,
                protocol: None,
                oauth: None,
            },
        );
        // An image-capable endpoint next to a text-only default: the shape that lets a text-only
        // session delegate an attachment to a subagent.
        settings.providers.insert(
            "vision".to_string(),
            crate::settings::ProviderConfig {
                env_key: None,
                models: None,
                api_key: Some("sk-v".into()),
                api_base_url: "https://vision.example".into(),
                supports_images: Some(true),
                protocol: None,
                oauth: None,
            },
        );
        let client = Arc::new(crate::api::client::Client::from_settings(&settings).unwrap());
        let mut runtime = Runtime::new("parent-model".into(), None, Default::default());
        runtime.mcp = Arc::new(tokio::sync::Mutex::new(crate::mcp::McpManager::new(
            Default::default(),
            Default::default(),
        )));
        let session = Arc::new(Session {
            client: (*client).clone(),
            runtime,
            permission_mode: crate::permission::PermissionMode::Default,
            settings,
            system: vec![SystemBlock {
                text: "parent system".into(),
                cache: false,
            }],
            depth: 0,
            cwd: Arc::new(std::sync::Mutex::new(std::env::temp_dir())),
            home: std::env::temp_dir(),
            user_config_dir: std::env::temp_dir().join(".config"),
            quiet: true,
            compact_failures: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            expand_tasks: tokio::sync::watch::channel(false).0,
            agents: AgentRegistry::new(),
            channels: crate::channels::ChannelRegistry::new(Default::default()),
            instance: None,
            attachments: crate::api::image::Attachments::new(),
        });
        (session, client)
    }

    /// A project directory with no crew pinned. Never the ambient cwd: these tests assert
    /// the exact system blocks a sub-session gets, and running them inside a repo that has
    /// its own `.bingo/team.json` would add the hire's blocks and fail them for a reason
    /// that has nothing to do with what they check.
    fn crewless() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("bingo-crewless-{}", std::process::id()))
    }

    fn params(prompt: &str) -> AgentInput {
        AgentInput {
            prompt: prompt.into(),
            background: None,
            notify_on: None,
            description: None,
            model: None,
            provider: None,
            thinking: None,
            name: None,
            agent: None,
        }
    }

    fn def(name: &str) -> AgentDef {
        AgentDef {
            name: name.into(),
            description: format!("{name} description"),
            model: Some("def-model".into()),
            provider: Some("ds".into()),
            thinking: Some("high".into()),
            system: "You are the reviewer.".into(),
            inherit_system: true,
            source: crate::agents::AgentDefSource::Unknown,
        }
    }

    /// Extract build_sub_session's error text (Arc<Session> has no Debug, so unwrap_err is unavailable).
    fn sub_err(r: Result<Arc<Session>, ToolError>) -> String {
        match r {
            Err(e) => e.to_string(),
            Ok(_) => panic!("expected build_sub_session error"),
        }
    }

    /// A project directory with a pinned crew and a written agreement.
    fn crewed_project(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bingo-hire-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".bingo")).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(
            dir.join(crate::team::TEAM_FILE),
            r#"{"name":"dev-room","members":[{"name":"Mira","agent":"qa"}]}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(
            dir.join(crate::team::NORMS_FILE),
            "# Team norms\n\n- Report outcomes as they are.\n",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        dir
    }

    /// A spawn in a crewed project is a hire and is told so (D53): it carries the crew's
    /// agreement, and it knows it is not on the crew. Without the second block "temporary"
    /// would be bookkeeping the instance itself never learns, and it would plan as if there
    /// were a next session in which it is asked again.
    #[test]
    fn a_spawn_beside_a_crew_is_a_hire_and_knows_it() {
        let (session, _client) = parent_session();
        let project = crewed_project("standing");
        let tool = AgentTool::new(session.clone(), Vec::new());
        let sub = tool
            .build_sub_session(&params("one job"), None, "temp", &project)
            .unwrap_or_else(|e| panic!("{e}"));
        let has = |head: &str| sub.system.iter().any(|b| b.text.starts_with(head));
        assert!(has("# Team norms (dev-room)"), "{:?}", sub.system);
        assert!(has("# You are a temporary hire"), "{:?}", sub.system);
        let standing = sub
            .system
            .iter()
            .find(|b| b.text.starts_with("# You are a temporary hire"))
            .unwrap_or_else(|| panic!("expected the standing block"));
        assert!(
            standing.text.contains(crate::team::TEAM_FILE)
                && standing.text.contains("not written into"),
            "it is told it never joins the blueprint: {}",
            standing.text
        );
        std::fs::remove_dir_all(&project).unwrap_or_else(|e| panic!("{e}"));
    }

    /// With no crew pinned, an ad-hoc subagent is the ordinary way to work: telling it that
    /// it is temporary relative to a team that does not exist would be a lie, and it is the
    /// same session it has always been.
    #[test]
    fn a_spawn_with_no_crew_is_told_nothing_about_one() {
        let (session, _client) = parent_session();
        let empty = std::env::temp_dir().join(format!("bingo-nocrew-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&empty);
        std::fs::create_dir_all(&empty).unwrap_or_else(|e| panic!("{e}"));
        let tool = AgentTool::new(session.clone(), Vec::new());
        let sub = tool
            .build_sub_session(&params("do it"), None, "solo", &empty)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            !sub.system
                .iter()
                .any(|b| b.text.contains("temporary hire") || b.text.starts_with("# Team norms")),
            "{:?}",
            sub.system
        );
        std::fs::remove_dir_all(&empty).unwrap_or_else(|e| panic!("{e}"));
    }

    /// The acceptance criterion in one assertion: hiring leaves the blueprint byte-identical.
    /// A hire that could edit `.bingo/team.json` would make the crew something the model
    /// grows on its own, which is exactly the decision the user keeps.
    #[tokio::test]
    async fn hiring_never_touches_the_blueprint() {
        let (session, _client) = parent_session();
        let project = crewed_project("blueprint");
        let path = project.join(crate::team::TEAM_FILE);
        let before = std::fs::read(&path).unwrap_or_else(|e| panic!("{e}"));
        let tool = AgentTool::new(session.clone(), Vec::new());
        let ctx = ToolContext {
            cwd: project.clone(),
            ..hub_ctx(&session)
        };
        let out = tool
            .call(
                serde_json::json!({"prompt": "look at one thing", "description": "one job"}),
                &ctx,
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            out.content.as_str().unwrap_or_default().contains("name"),
            "the spawn returns an addressable instance"
        );
        assert_eq!(
            std::fs::read(&path).unwrap_or_else(|e| panic!("{e}")),
            before,
            "the blueprint is byte-identical before and after a hire"
        );
        let listed = session.agents.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].kind,
            AgentKind::Hire,
            "an Agent-tool spawn is never a crew member"
        );
        std::fs::remove_dir_all(&project).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn sub_session_inherits_model_and_shared_endpoint() {
        let (session, client) = parent_session();
        let _ = session.runtime.thinking_tx.send(Some("medium".into()));
        let tool = AgentTool::new(session.clone(), Vec::new());
        let sub = tool
            .build_sub_session(&params("do it"), None, "sub", &crewless())
            .unwrap();
        assert_eq!(*sub.runtime.model.borrow(), "parent-model");
        assert_eq!(
            sub.client.current_endpoint(),
            (
                Some("sk-parent".to_string()),
                "https://parent.example".to_string()
            )
        );
        assert_eq!(
            sub.system[0].text, "parent system",
            "inherits the parent system when no definition is given"
        );
        assert_eq!(
            sub.runtime.thinking.borrow().as_deref(),
            Some("medium"),
            "inherits the parent session's current thinking level when neither explicit nor defined"
        );
        // No provider specified: shares the parent endpoint (follows the parent's provider switch).
        client.set_provider("ds").unwrap();
        assert_eq!(
            sub.client.current_endpoint().0.as_deref(),
            Some("sk-ds"),
            "the shared endpoint follows the parent session's switches"
        );
    }

    #[test]
    fn sub_session_overrides_model_and_provider() {
        let (session, _client) = parent_session();
        let tool = AgentTool::new(session.clone(), Vec::new());
        let mut p = params("do it");
        p.model = Some("sub-model".into());
        p.provider = Some("ds".into());
        p.thinking = Some("xhigh".into());
        let sub = tool
            .build_sub_session(&p, None, "sub", &crewless())
            .unwrap();
        assert_eq!(*sub.runtime.model.borrow(), "sub-model");
        assert_eq!(sub.runtime.provider.borrow().as_str(), "ds");
        assert_eq!(
            sub.client.current_endpoint(),
            (
                Some("sk-ds".to_string()),
                "https://api.deepseek.com".to_string()
            )
        );
        assert_eq!(
            sub.runtime.thinking.borrow().as_deref(),
            Some("xhigh"),
            "an explicit thinking level takes effect"
        );
        // Forked independent endpoint: the parent session is unaffected.
        assert_eq!(
            session.client.current_endpoint().0.as_deref(),
            Some("sk-parent")
        );
    }

    #[test]
    fn named_def_supplies_system_and_defaults() {
        let (session, _client) = parent_session();
        let d = def("reviewer");
        let tool = AgentTool::new(session.clone(), vec![d.clone()]);
        // The definition supplies system/model/provider/thinking defaults.
        let sub = tool
            .build_sub_session(&params("review"), Some(&d), "sub", &crewless())
            .unwrap();
        // Default is append: parent system + persona + the subagent note block
        // + the instance's own capability block.
        let texts: Vec<&str> = sub.system.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "parent system",
                "You are the reviewer.",
                SUBAGENT_NOTE,
                &capability_block("def-model", "ds")
            ],
            "a named definition appends by default rather than replacing"
        );
        assert_eq!(*sub.runtime.model.borrow(), "def-model");
        assert_eq!(sub.runtime.provider.borrow().as_str(), "ds");
        assert_eq!(
            sub.runtime.thinking.borrow().as_deref(),
            Some("high"),
            "the definition provides the thinking-level default"
        );
        // Explicit parameters take precedence over the definition.
        let mut p = params("review");
        p.model = Some("explicit".into());
        p.thinking = Some("off".into());
        let sub = tool
            .build_sub_session(&p, Some(&d), "sub", &crewless())
            .unwrap();
        assert_eq!(*sub.runtime.model.borrow(), "explicit");
        assert_eq!(
            sub.runtime.thinking.borrow().as_deref(),
            None,
            "explicit off normalizes to no parameter"
        );
        // resolve_def: an unknown definition errors out and lists the available ones.
        let mut p = params("x");
        p.agent = Some("nope".into());
        let err = tool.resolve_def(&p).unwrap_err().to_string();
        assert!(err.contains("nope") && err.contains("reviewer"), "{err}");
    }

    #[test]
    fn sub_session_unknown_provider_errors() {
        let (session, _client) = parent_session();
        let tool = AgentTool::new(session, Vec::new());
        let mut p = params("do it");
        p.provider = Some("nope".into());
        assert!(
            tool.build_sub_session(&p, None, "sub", &crewless())
                .is_err(),
            "unknown provider errors"
        );
    }

    #[test]
    fn sub_session_cross_provider_requires_model() {
        // Parent provider = "default" (the parent_session default).
        let (session, _client) = parent_session();
        // Only a provider given, no model → fail early: the parent model is not inherited (so claude-sonnet-5 never
        // lands on a DeepSeek endpoint as "model not found").
        let tool = AgentTool::new(session.clone(), Vec::new());
        let mut p = params("do it");
        p.provider = Some("ds".into());
        let err = sub_err(tool.build_sub_session(&p, None, "sub", &crewless()));
        assert!(
            err.contains("requires a model") && err.contains("ds"),
            "crossing providers requires an explicit model: {err}"
        );
        // The definition provides a provider but no model → errors the same way.
        let mut d = def("reviewer");
        d.model = None;
        let tool = AgentTool::new(session.clone(), vec![d.clone()]);
        let err = sub_err(tool.build_sub_session(&params("review"), Some(&d), "sub", &crewless()));
        assert!(
            err.contains("requires a model"),
            "the definition-side cross-provider case errors the same way: {err}"
        );
        // Same provider (the parent's current is ds) → inherits the model, no error.
        let _ = session.runtime.provider_tx.send("ds".into());
        let tool = AgentTool::new(session.clone(), Vec::new());
        let mut p = params("do it");
        p.provider = Some("ds".into());
        let sub = tool
            .build_sub_session(&p, None, "sub", &crewless())
            .unwrap();
        assert_eq!(
            *sub.runtime.model.borrow(),
            "parent-model",
            "same provider inherits the parent model"
        );
    }

    #[test]
    fn sub_session_cross_provider_defaults_thinking_off() {
        let (session, _client) = parent_session();
        let _ = session.runtime.thinking_tx.send(Some("xhigh".into()));
        let tool = AgentTool::new(session.clone(), Vec::new());
        // Crossing providers with no explicit/defined thinking → defaults to off (no thinking parameter,
        // compatible with DeepSeek/Ollama endpoints).
        let mut p = params("do it");
        p.provider = Some("ds".into());
        p.model = Some("ds-model".into());
        let sub = tool
            .build_sub_session(&p, None, "sub", &crewless())
            .unwrap();
        assert_eq!(
            sub.runtime.thinking.borrow().as_deref(),
            None,
            "crossing providers defaults to off"
        );
        // An explicit thinking level still applies when crossing providers.
        let mut p = params("do it");
        p.provider = Some("ds".into());
        p.model = Some("ds-model".into());
        p.thinking = Some("high".into());
        let sub = tool
            .build_sub_session(&p, None, "sub", &crewless())
            .unwrap();
        assert_eq!(sub.runtime.thinking.borrow().as_deref(), Some("high"));
    }

    #[test]
    fn sub_session_same_provider_inherits_thinking() {
        let (session, _client) = parent_session();
        let _ = session.runtime.thinking_tx.send(Some("xhigh".into()));
        let _ = session.runtime.provider_tx.send("ds".into());
        let tool = AgentTool::new(session.clone(), Vec::new());
        let mut p = params("do it");
        p.provider = Some("ds".into());
        let sub = tool
            .build_sub_session(&p, None, "sub", &crewless())
            .unwrap();
        assert_eq!(
            sub.runtime.thinking.borrow().as_deref(),
            Some("xhigh"),
            "same provider keeps the inherited snapshot"
        );
    }

    #[test]
    fn sub_session_default_provider_aliases_parent_endpoint() {
        let (session, client) = parent_session();
        let tool = AgentTool::new(session.clone(), Vec::new());
        // Explicit "default": shares the parent endpoint, no fork, no error.
        let mut p = params("do it");
        p.provider = Some("default".into());
        let sub = tool
            .build_sub_session(&p, None, "sub", &crewless())
            .unwrap();
        assert_eq!(sub.runtime.provider.borrow().as_str(), "default");
        assert_eq!(
            sub.client.current_endpoint(),
            (
                Some("sk-parent".to_string()),
                "https://parent.example".to_string()
            )
        );
        // The shared endpoint follows the parent's switches ("default" and unset are equivalent).
        client.set_provider("ds").unwrap();
        let _ = session.runtime.provider_tx.send("ds".into());
        assert_eq!(sub.client.current_endpoint().0.as_deref(), Some("sk-ds"));
        // AgentDef frontmatter provider: default takes the same path (follows the parent's current provider name).
        let mut d = def("reviewer");
        d.provider = Some("default".into());
        let tool = AgentTool::new(session.clone(), vec![d.clone()]);
        let sub = tool
            .build_sub_session(&params("review"), Some(&d), "sub", &crewless())
            .unwrap();
        assert_eq!(sub.runtime.provider.borrow().as_str(), "ds");
    }

    #[test]
    fn sub_session_rejects_invalid_thinking() {
        let (session, _client) = parent_session();
        let tool = AgentTool::new(session.clone(), Vec::new());
        for bad in ["auto", "super", "HIGH"] {
            let mut p = params("do it");
            p.thinking = Some(bad.into());
            let err = sub_err(tool.build_sub_session(&p, None, "sub", &crewless()));
            assert!(
                err.contains("invalid thinking level"),
                "invalid level {bad:?} should error: {err}"
            );
        }
        // An invalid definition-side value errors the same way.
        let mut d = def("reviewer");
        d.thinking = Some("bogus".into());
        let tool = AgentTool::new(session.clone(), vec![d.clone()]);
        let err = sub_err(tool.build_sub_session(&params("review"), Some(&d), "sub", &crewless()));
        assert!(
            err.contains("invalid thinking level"),
            "definition-side invalid value should error: {err}"
        );
    }

    #[test]
    fn schema_exposes_name_and_agent() {
        let (session, _client) = parent_session();
        let tool = AgentTool::new(session, vec![def("reviewer")]);
        let schema = tool.input_schema();
        let props = schema["properties"].as_object().unwrap();
        for key in ["model", "provider", "thinking", "name", "agent"] {
            assert!(props.contains_key(key), "schema contains {key}");
        }
        assert!(
            tool.description()
                .contains("- reviewer: reviewer description"),
            "the description lists the named definitions"
        );
    }

    #[test]
    fn excerpt_is_single_line_and_bounded() {
        assert_eq!(excerpt("short task"), "short task");
        assert_eq!(excerpt("first line\nsecond line"), "first line…");
        let long = "x".repeat(50);
        let cut = excerpt(&long);
        assert!(cut.chars().count() <= 41, "{cut}");
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn agent_control_list_reports_relative_last_activity() {
        assert_eq!(format_last_active(std::time::Duration::ZERO), "active now");
        assert_eq!(
            format_last_active(std::time::Duration::from_secs(3)),
            "active 3s ago"
        );
        assert_eq!(
            format_last_active(std::time::Duration::from_secs(125)),
            "active 2min ago"
        );
        assert_eq!(
            format_last_active(std::time::Duration::from_secs(7_200)),
            "active 2h ago"
        );
    }

    #[tokio::test]
    async fn agent_control_list_stop_delete() {
        let (session, _client) = parent_session();
        session.agents.insert(
            "scout",
            AgentKind::Hire,
            None,
            "research".into(),
            session.clone(),
        );
        let ctl = AgentControlTool::new(session.clone());
        let ctx = crate::tool::ToolContext {
            home: std::env::temp_dir(),
            cwd: std::path::PathBuf::from("/tmp"),
            watch: session.watch.clone(),
            live: Default::default(),
            http: reqwest::Client::new(),
            tasks: session.tasks.clone(),
            hooks: crate::settings::HooksConfig::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
            instance: None,
            rewind: Default::default(),
        };
        assert!(ctl.is_read_only(&serde_json::json!({"action": "list"})));
        assert!(!ctl.is_read_only(&serde_json::json!({"action": "stop", "agent": "scout"})));
        let out = ctl
            .call(serde_json::json!({"action": "list"}), &ctx)
            .await
            .unwrap();
        let text = out.content.as_str().unwrap();
        assert!(
            text.contains("scout") && text.contains("running") && text.contains("active now"),
            "{text}"
        );
        let out = ctl
            .call(
                serde_json::json!({"action": "stop", "agent": "scout"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.content.as_str().unwrap().contains("stopped"), "stop");
        // After stopping, SendMessage rejects delivery.
        let send = SendMessageTool::new(session.clone());
        let err = send
            .call(serde_json::json!({"to": "scout", "message": "hi"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("stopped"), "{err}");
        let out = ctl
            .call(
                serde_json::json!({"action": "delete", "agent": "scout"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.content.as_str().unwrap().contains("deleted"));
        assert!(session.agents.list().is_empty());
        // Unknown instance: stop errors out.
        let err = ctl
            .call(
                serde_json::json!({"action": "stop", "agent": "ghost"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("ghost"), "{err}");
    }

    #[tokio::test]
    async fn send_message_starts_an_idle_instance_before_returning() {
        let (session, _client) = parent_session();
        session.agents.insert(
            "worker",
            AgentKind::Hire,
            None,
            "do work".into(),
            session.clone(),
        );
        session.agents.mark_idle("worker");
        let out = SendMessageTool::new(session.clone())
            .call(
                serde_json::json!({"to": "worker", "message": "start now", "ack_timeout": 0}),
                &hub_ctx(&session),
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let receipt: serde_json::Value =
            serde_json::from_str(out.content.as_str().unwrap_or_default())
                .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(receipt["status"], "queued");
        let status = &session.agents.list()[0];
        assert_eq!(status.state, crate::agents::AgentState::Running);
        assert_eq!(status.pending, 0, "the idle inbox was claimed immediately");
        let acks = session
            .agents
            .acks_of("worker")
            .unwrap_or_else(|| unreachable!());
        assert!(matches!(
            acks[0].state,
            crate::agents::AckState::Delivered { run: 1 }
        ));
        let _ = session.agents.stop("worker");
    }

    #[tokio::test]
    async fn send_message_keeps_running_instance_queued_for_its_next_tool_round() {
        let (session, _client) = parent_session();
        session.agents.insert(
            "worker",
            AgentKind::Hire,
            None,
            "do work".into(),
            session.clone(),
        );
        let send = SendMessageTool::new(session.clone());
        let ctx = hub_ctx(&session);
        // The acknowledgement wait is opt-in: omitting it keeps the plain fire-and-forget path.
        let schema = send.input_schema();
        assert!(schema["properties"]["ack_timeout"].is_object());
        assert_eq!(schema["required"], serde_json::json!(["message", "to"]));
        let out = send
            .call(
                serde_json::json!({"to": "worker", "message": "add more"}),
                &ctx,
            )
            .await
            .unwrap();
        // A running receiver keeps it queued until its query loop reaches the next tool round.
        let receipt: serde_json::Value =
            serde_json::from_str(out.content.as_str().unwrap_or_default())
                .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(receipt["status"], "queued");
        assert_eq!(receipt["message_id"], 1);
        let status = &session.agents.list()[0];
        assert_eq!(status.pending, 1);
        assert_eq!(status.unacked, 1, "queued is not yet a receipt");
        // Unknown instance: the error lists the existing instance names.
        let err = send
            .call(serde_json::json!({"to": "nobody", "message": "x"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("worker"), "{err}");
    }

    /// The chase protects a sender who never thought to ask for it — that is the whole point of a
    /// default. Opting out has to be said out loud.
    #[tokio::test]
    async fn the_reply_check_is_on_by_default_and_zero_turns_it_off() {
        let (session, _client) = parent_session();
        session.agents.insert(
            "worker",
            AgentKind::Hire,
            None,
            "do work".into(),
            session.clone(),
        );
        let send = SendMessageTool::new(session.clone());
        let ctx = hub_ctx(&session);
        let receipt = |out: ToolResult| -> serde_json::Value {
            serde_json::from_str(out.content.as_str().unwrap_or_default())
                .unwrap_or_else(|e| panic!("{e}"))
        };

        let out = send
            .call(
                serde_json::json!({"to": "worker", "message": "default"}),
                &ctx,
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            receipt(out)["ack_timeout_secs"],
            DEFAULT_ACK_TIMEOUT_SECS,
            "it is watched even without a request"
        );

        let out = send
            .call(
                serde_json::json!({"to": "worker", "message": "no wait for a reply", "ack_timeout": 0}),
                &ctx,
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            receipt(out)["ack_timeout_secs"].is_null(),
            "0 = explicitly off"
        );

        let acks = session
            .agents
            .acks_of("worker")
            .unwrap_or_else(|| unreachable!());
        assert_eq!(
            acks[0].timeout,
            Some(std::time::Duration::from_secs(DEFAULT_ACK_TIMEOUT_SECS))
        );
        assert_eq!(acks[1].timeout, None);
    }

    fn hub_ctx(session: &Arc<Session>) -> crate::tool::ToolContext {
        crate::tool::ToolContext {
            home: std::env::temp_dir(),
            cwd: std::path::PathBuf::from("/tmp"),
            watch: session.watch.clone(),
            live: Default::default(),
            http: reqwest::Client::new(),
            tasks: session.tasks.clone(),
            hooks: crate::settings::HooksConfig::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
            instance: None,
            rewind: Default::default(),
        }
    }

    /// A depth-1 sub-session under the same registries, instance name stamped —
    /// the shape `build_sub_session` produces, minus the model plumbing these
    /// tests do not exercise.
    fn sub_of(parent: &Arc<Session>, instance: &str, rooms: bool) -> Arc<Session> {
        let mut settings = parent.settings.clone();
        settings.experimental.agent_channels = rooms;
        Arc::new(Session {
            depth: 1,
            instance: Some(instance.to_string()),
            settings,
            ..(**parent).clone()
        })
    }

    /// The address grammar is the conversation namespace: `#name` is a room,
    /// anything else is an agent and may wear a leading `@`.
    #[test]
    fn to_is_read_as_the_conversation_namespace() {
        assert_eq!(
            parse_address("scout").unwrap_or_else(|e| panic!("{e}")),
            Address::Agent("scout".into())
        );
        assert_eq!(
            parse_address(" @scout ").unwrap_or_else(|e| panic!("{e}")),
            Address::Agent("scout".into()),
            "the sigil the bar shows is accepted, not required"
        );
        assert_eq!(
            parse_address("#build").unwrap_or_else(|e| panic!("{e}")),
            Address::Room("build".into())
        );
        for bad in ["", "   ", "@", "#"] {
            assert!(parse_address(bad).is_err(), "{bad:?} is not an address");
        }
    }

    /// Hub-and-spoke survives the merge of the two speech tools, but as an
    /// addressing rule rather than as a withheld tool: a subagent holds
    /// `SendMessage` and still cannot reach a sibling with it.
    #[tokio::test]
    async fn a_subagent_may_address_main_and_nothing_else_in_the_agent_namespace() {
        let (session, _client) = parent_session();
        session.agents.insert(
            "sibling",
            AgentKind::Hire,
            None,
            "work".into(),
            session.clone(),
        );
        let ctx = hub_ctx(&session);
        let send = SendMessageTool::new(sub_of(&session, "scout", false));

        let err = send
            .call(
                serde_json::json!({"to": "sibling", "message": "take this"}),
                &ctx,
            )
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("sibling"), "{text}");
        assert!(
            text.contains("main") && text.contains("rooms you are a member of"),
            "the refusal has to say what it may address instead: {text}"
        );

        let err = send
            .call(serde_json::json!({"to": "scout", "message": "note"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("is you"), "{err}");

        // And main is reachable.
        let out = send
            .call(
                serde_json::json!({"to": "main", "message": "the migration is done"}),
                &ctx,
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(!out.is_error);
    }

    /// The message lands in the store the query layer drains into main's next
    /// turn, under the calling instance's real name — not the hub's, which is
    /// what the old sender field hardcoded.
    #[tokio::test]
    async fn a_message_to_main_lands_in_the_inbox_under_the_sender_s_own_name() {
        let (session, _client) = parent_session();
        let ctx = hub_ctx(&session);
        SendMessageTool::new(sub_of(&session, "scout", false))
            .call(
                serde_json::json!({"to": "@main", "message": "the migration is done"}),
                &ctx,
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert!(session.channels.has_hub_mail());
        assert!(
            !session.channels.take_hub_mail_urgent(),
            "an ordinary message does not ring"
        );
        let mail = session.channels.drain_hub_mail();
        assert_eq!(
            mail,
            vec!["[message from @scout]\nthe migration is done".to_string()],
            "the marker names who, and the text follows it"
        );
    }

    /// `urgent` is the harness's bell and it has exactly one meaning: an agent
    /// needs the user. Anywhere else there is nobody on the other end, so it is
    /// refused rather than quietly ignored.
    #[tokio::test]
    async fn urgent_is_a_subagent_to_main_flag_and_refused_elsewhere() {
        let (session, _client) = parent_session();
        session.agents.insert(
            "worker",
            AgentKind::Hire,
            None,
            "work".into(),
            session.clone(),
        );
        let ctx = hub_ctx(&session);

        SendMessageTool::new(sub_of(&session, "scout", false))
            .call(
                serde_json::json!({"to": "main", "message": "I need the deploy key", "urgent": true}),
                &ctx,
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            session.channels.take_hub_mail_urgent(),
            "the bell is owed on arrival"
        );

        let err = SendMessageTool::new(session.clone())
            .call(
                serde_json::json!({"to": "worker", "message": "look now", "urgent": true}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("urgent only applies"), "{err}");
    }

    /// Room addressing is still behind the experimental gate, and a member has
    /// to be a member. Both refusals name the room.
    #[tokio::test]
    async fn room_addressing_is_gated_and_checked() {
        let (session, _client) = parent_session();
        let ctx = hub_ctx(&session);

        let err = SendMessageTool::new(sub_of(&session, "scout", false))
            .call(serde_json::json!({"to": "#build", "message": "hi"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("#build"), "{err}");

        let with_rooms = sub_of(&session, "scout", true);
        let err = SendMessageTool::new(with_rooms.clone())
            .call(serde_json::json!({"to": "#ghost", "message": "hi"}), &ctx)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("not a member of #ghost"),
            "an unknown room and a room you are not in are the same refusal: {err}"
        );

        session
            .channels
            .create(
                "build",
                vec!["scout".into()],
                crate::channels::ChannelMode::Free,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        let out = SendMessageTool::new(with_rooms)
            .call(serde_json::json!({"to": "#build", "message": "hi"}), &ctx)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            out.content
                .as_str()
                .unwrap_or_default()
                .contains("#build msg #1"),
            "{out:?}"
        );
    }

    /// Contract 3's discriminator, in isolation: what the run was woken *by*
    /// decides whether its end is main's business.
    #[test]
    fn only_an_all_user_batch_keeps_its_end_to_itself() {
        let user_item = |text: &str| InboxItem::Direct {
            id: MsgId(1),
            from: crate::channels::USER_NAME.to_string(),
            text: text.to_string(),
            images: Vec::new(),
        };
        let hub_item = InboxItem::Direct {
            id: MsgId(2),
            from: crate::channels::HUB_NAME.to_string(),
            text: "carry on".to_string(),
            images: Vec::new(),
        };
        assert!(
            wakes_owner(&[]),
            "a dispatch has no items: the Agent call itself is the trigger"
        );
        assert!(!wakes_owner(&[user_item("are you there?")]));
        assert!(!wakes_owner(&[user_item("one"), user_item("two")]));
        assert!(
            wakes_owner(&[user_item("one"), hub_item]),
            "one main-origin item in the batch and the reply answers it"
        );
        assert!(wakes_owner(&[InboxItem::Channel {
            channel: "build".into(),
            from: "zoe".into(),
            text: "the tests pass".into(),
            seq: 3,
        }]));
    }

    /// A message that is never picked up is chased on the sender's own clock and then reported:
    /// three follow-ups ride along with it, and the give-up lands in the hub's notification queue
    /// rather than staying an unanswered "queued" nobody looks at again.
    #[tokio::test(start_paused = true)]
    async fn unacknowledged_message_is_chased_three_times_then_reported() {
        let (session, _client) = parent_session();
        // Running without a query loop: the dispatcher cannot claim it, so the message stays queued.
        session.agents.insert(
            "worker",
            AgentKind::Hire,
            None,
            "do work".into(),
            session.clone(),
        );
        let ctx = hub_ctx(&session);
        let out = SendMessageTool::new(session.clone())
            .call(
                serde_json::json!({"to": "worker", "message": "check the logs", "ack_timeout": 1}),
                &ctx,
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let receipt: serde_json::Value =
            serde_json::from_str(out.content.as_str().unwrap_or_default())
                .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            receipt["ack_timeout_secs"], 5,
            "waits below the lower bound are clamped"
        );

        // Four deadlines: three follow-ups, then the give-up.
        tokio::time::sleep(std::time::Duration::from_secs(5 * 5)).await;

        let acks = session
            .agents
            .acks_of("worker")
            .unwrap_or_else(|| unreachable!());
        assert_eq!(
            acks[0].follow_ups, MAX_FOLLOW_UPS,
            "chased until the budget runs out"
        );
        assert_eq!(
            session.agents.list()[0].pending,
            1 + MAX_FOLLOW_UPS as usize,
            "one follow-up per round, in the inbox with the original"
        );
        let notes = session.watch.consume_notifications(None);
        assert!(
            notes
                .iter()
                .any(|n| n.contains("follow-ups") && n.contains("worker")),
            "the hub is told after giving up: {notes:?}"
        );
    }

    /// Being read is not being answered: an instance that takes the message and stays quiet is
    /// chased exactly like one that never picked it up, and the sender hears about it.
    #[tokio::test(start_paused = true)]
    async fn a_receiver_that_reads_and_says_nothing_is_still_chased() {
        let (session, _client) = parent_session();
        session.agents.insert(
            "mute",
            AgentKind::Hire,
            None,
            "silent".into(),
            session.clone(),
        );
        let ctx = hub_ctx(&session);
        SendMessageTool::new(session.clone())
            .call(
                serde_json::json!({"to": "mute", "message": "report progress", "ack_timeout": 5}),
                &ctx,
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        // A turn ends without a word and takes the queued message into the next one: delivered,
        // unanswered, and still Running — so the flush the watchdog retries stays a no-op here.
        assert!(session.agents.finish("mute", Vec::new(), 0).is_some());
        assert!(matches!(
            session
                .agents
                .acks_of("mute")
                .unwrap_or_else(|| unreachable!())[0]
                .state,
            crate::agents::AckState::Delivered { .. }
        ));

        tokio::time::sleep(std::time::Duration::from_secs(5 * 5)).await;

        let acks = session
            .agents
            .acks_of("mute")
            .unwrap_or_else(|| unreachable!());
        assert_eq!(
            acks[0].follow_ups, MAX_FOLLOW_UPS,
            "read-but-silent is still chased to the end"
        );
        assert_eq!(session.agents.list()[0].pending, MAX_FOLLOW_UPS as usize);
        let notes = session.watch.consume_notifications(None);
        assert!(
            notes.iter().any(|n| n.contains("still has not replied")),
            "silence is eventually reported to the hub: {notes:?}"
        );
    }

    /// The silent half of the same mechanism: a message answered inside its wait leaves no watch
    /// line and no notification — the chase only speaks when something went wrong.
    #[tokio::test(start_paused = true)]
    async fn an_acknowledged_message_reports_nothing() {
        let (session, _client) = parent_session();
        session.agents.insert(
            "worker",
            AgentKind::Hire,
            None,
            "do work".into(),
            session.clone(),
        );
        let ctx = hub_ctx(&session);
        SendMessageTool::new(session.clone())
            .call(
                serde_json::json!({"to": "worker", "message": "check the logs", "ack_timeout": 60}),
                &ctx,
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        // The receiver picks it up at the boundary, then that run ends with something to say.
        assert!(session.agents.finish("worker", Vec::new(), 1).is_some());
        assert!(session.agents.finish("worker", Vec::new(), 2).is_none());
        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
        let acks = session
            .agents
            .acks_of("worker")
            .unwrap_or_else(|| unreachable!());
        assert!(matches!(
            acks[0].state,
            crate::agents::AckState::Answered { .. }
        ));
        assert_eq!(
            acks[0].follow_ups, 0,
            "an on-time reply does not trigger chasing"
        );
        assert!(
            session.watch.consume_notifications(None).is_empty(),
            "no news, no nagging the hub"
        );
        assert!(
            session.watch.snapshot().is_empty(),
            "and leaves no board line"
        );
    }

    /// The hub forwards an image to a subagent by repeating its `#[image N]` marker: the
    /// attachment table is shared with the sub-session, and the resolved images ride along with
    /// the queued instruction so a busy instance still receives them.
    #[test]
    fn image_markers_resolve_for_spawn_and_follow_up() {
        let (session, _client) = parent_session();
        let png = {
            let img = image::RgbaImage::from_pixel(4, 2, image::Rgba([255u8, 0, 0, 255]));
            let mut out = Vec::new();
            image::DynamicImage::ImageRgba8(img)
                .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                .unwrap_or_else(|_| unreachable!());
            out
        };
        assert_eq!(session.attachments.register(&png), Some(1));

        // Spawn: markers in the prompt resolve against the session table.
        let images = session
            .attachments
            .resolve("look at this #[image 1] and decide");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].media_type, "image/png");
        // Sub-sessions share the table, so a nested spawn can resolve the same marker.
        let sub = build_sub_session(
            &session,
            None,
            None,
            None,
            None,
            "worker",
            MemberContext::default(),
        )
        .unwrap();
        assert_eq!(sub.attachments.resolve("#[image 1]").len(), 1);

        // Follow-up: a queued instruction keeps its images until it is delivered.
        session
            .agents
            .insert("worker", AgentKind::Hire, None, "d".into(), sub.clone());
        let id = session
            .agents
            .deliver(
                "worker",
                crate::channels::HUB_NAME,
                "compare #[image 1]",
                images.clone(),
                None,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        let (prompt, carried) = match session.agents.finish("worker", Vec::new(), 1) {
            Some(next) => absorb_inbox(&sub.channels, "worker", &next.items),
            None => unreachable!("queued messages should be claimed by the receiver"),
        };
        let acks = session
            .agents
            .acks_of("worker")
            .unwrap_or_else(|| unreachable!());
        assert_eq!(acks[0].id, id);
        assert_eq!(prompt, "compare #[image 1]");
        assert_eq!(
            carried.len(),
            1,
            "images arrive with the queued instruction"
        );
        assert_eq!(carried[0].data, images[0].data);
    }

    /// D64: who wrote a direct message is part of the message. The user's DMs arrive under
    /// the `[DM from user]` line — alone or batched with hub traffic — while a single hub
    /// instruction stays byte-identical, so the common SendMessage path is unchanged.
    #[test]
    fn absorb_inbox_names_the_user_and_keeps_the_hub_verbatim() {
        let (session, _client) = parent_session();
        let sub = build_sub_session(
            &session,
            None,
            None,
            None,
            None,
            "worker",
            MemberContext::default(),
        )
        .unwrap_or_else(|e| panic!("spawn: {e}"));
        session
            .agents
            .insert("worker", AgentKind::Hire, None, "d".into(), sub.clone());

        let deliver = |from: &str, text: &str| {
            session
                .agents
                .deliver("worker", from, text, Vec::new(), None)
                .unwrap_or_else(|e| panic!("{e}"));
        };
        let absorb = || match session.agents.finish("worker", Vec::new(), 0) {
            Some(next) => absorb_inbox(&sub.channels, "worker", &next.items).0,
            None => unreachable!("queued messages should be claimed by the receiver"),
        };

        deliver(crate::channels::USER_NAME, "are you there?");
        assert_eq!(absorb(), format!("{DM_FROM_USER_MARKER}\nare you there?"));

        deliver(crate::channels::HUB_NAME, "map the module");
        assert_eq!(absorb(), "map the module", "hub singles stay verbatim");

        deliver(crate::channels::HUB_NAME, "first");
        deliver(crate::channels::USER_NAME, "second");
        assert_eq!(
            absorb(),
            format!("[follow-up instruction] first\n{DM_FROM_USER_MARKER}\nsecond"),
            "a batch labels the hub's line and marks the user's"
        );
    }

    /// A text-only main session can still get an image looked at: the attachment table is
    /// session-scoped and independent of endpoint capability, so a subagent forked onto an
    /// image-capable provider resolves the same `#[image N]` marker and actually receives it.
    #[test]
    fn text_only_parent_can_hand_an_image_to_a_vision_subagent() {
        let (parent, _client) = parent_session();
        let png = {
            let img = image::RgbaImage::from_pixel(4, 2, image::Rgba([9u8, 9, 9, 255]));
            let mut out = Vec::new();
            image::DynamicImage::ImageRgba8(img)
                .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                .unwrap_or_else(|_| unreachable!());
            out
        };
        assert_eq!(parent.attachments.register(&png), Some(1));
        assert!(
            !parent.client.supports_images(),
            "the parent endpoint does not accept images (a precondition of this test)"
        );

        // Markers resolve regardless of what the parent endpoint can carry.
        let images = parent.attachments.resolve("describe #[image 1]");
        assert_eq!(
            images.len(),
            1,
            "resolution is unaffected by the endpoint's capabilities"
        );

        // Forked onto the vision provider, the sub-session is the one whose capability decides.
        let sub = build_sub_session(
            &parent,
            Some("vision-model".into()),
            Some("vision".into()),
            None,
            None,
            "looker",
            MemberContext::default(),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            sub.client.supports_images(),
            "the sub-session endpoint accepts images"
        );
        assert!(
            Arc::ptr_eq(&sub.attachments, &parent.attachments),
            "the attachment table is shared; restating the placeholder hits it"
        );
        assert!(
            parent
                .client
                .image_capable_providers()
                .contains(&"vision".to_string()),
            "the path pointed to in the prompt is discoverable: {:?}",
            parent.client.image_capable_providers()
        );
    }

    /// `inherit_system: false` opts back into wholesale replacement; the subagent note is still
    /// appended, because it describes the runtime rather than the persona.
    #[test]
    fn inherit_system_false_replaces_parent_blocks() {
        let (session, _client) = parent_session();
        let mut d = def("reviewer");
        d.inherit_system = false;
        let tool = AgentTool::new(session, vec![d.clone()]);
        let sub = tool
            .build_sub_session(&params("review"), Some(&d), "sub", &crewless())
            .unwrap();
        let texts: Vec<&str> = sub.system.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "You are the reviewer.",
                SUBAGENT_NOTE,
                &capability_block("def-model", "ds")
            ]
        );
    }

    /// Channel etiquette rides in the system prompt, and only when channels are on.
    ///
    /// The placement is the point: it outlives compaction. That is not asserted here because it
    /// cannot fail — `compact::maybe_compact` takes `&Session`, so the borrow checker forbids it
    /// from touching `Session::system` at all; it splices `messages` and builds its summary
    /// request with `system: Vec::new()`. A test that re-stated that would prove nothing.
    #[test]
    fn channel_note_is_gated_by_the_flag() {
        let (off, _c1) = parent_session();
        assert!(!off.settings.experimental.agent_channels, "off by default");
        let sub = build_sub_session(
            &off,
            None,
            None,
            None,
            None,
            "solo",
            MemberContext::default(),
        )
        .unwrap_or_else(|e| panic!("spawn: {e}"));
        assert!(
            !sub.system.iter().any(|b| b.text == CHANNEL_NOTE),
            "channel etiquette must not be injected when channels are off"
        );

        let (mut on, _c2) = parent_session();
        let session = Arc::get_mut(&mut on).unwrap_or_else(|| panic!("exclusive"));
        session.settings.experimental.agent_channels = true;
        let sub = build_sub_session(
            &on,
            None,
            None,
            None,
            None,
            "member",
            MemberContext::default(),
        )
        .unwrap_or_else(|e| panic!("spawn: {e}"));
        assert!(sub.system.iter().any(|b| b.text == CHANNEL_NOTE));
        // Both failure modes have to survive edits to this text: the storm it was written
        // for, and the over-correction where nobody answers the human at all.
        assert!(
            CHANNEL_NOTE.contains("Never answer an answer"),
            "must name the reply-to-replies storm specifically, not just say \"keep it brief\""
        );
        assert!(
            CHANNEL_NOTE.contains("puts words in the room"),
            "must state that the turn body never reaches the channel — otherwise members think they already answered"
        );
        assert!(
            CHANNEL_NOTE.contains("`user` or `main` addressed the room"),
            "must spell out \"answer when a human speaks\", otherwise the silence rule overshoots"
        );
        assert!(
            CHANNEL_NOTE.contains("never in a room"),
            "must state that a DM is answered in turn text — otherwise a member takes a private question to the room"
        );
        assert!(
            CHANNEL_NOTE.contains("stays private"),
            "must forbid relaying DM content into a channel, not just answering there"
        );
        assert!(
            CHANNEL_NOTE.contains(DM_FROM_USER_MARKER),
            "the medium rule needs the observable tag, not just the concept"
        );
        assert!(
            CHANNEL_NOTE.contains("without waiting to be asked"),
            "must impose the proactive duty to speak in the room — otherwise a team-wide finding \
             reaches only the hub as turn text and the room works on stale ground (D67)"
        );
        assert!(
            CHANNEL_NOTE.contains("stays in your turn text"),
            "must keep member status out of the room — without this second half the venue rule \
             reopens the reply storm through a new door (D67)"
        );
    }

    /// The user reads a member's turn text in the DM window (D57), so the subagent note may
    /// not claim the user never sees it. That claim is what made a DM'd member believe the
    /// only way to reach the human was a room message (D63).
    #[test]
    fn subagent_note_knows_the_dm_window_exists() {
        assert!(
            SUBAGENT_NOTE.contains("direct-message window"),
            "must name the private surface the user reaches an instance through"
        );
        assert!(
            !SUBAGENT_NOTE.contains("not displayed to the user"),
            "the old claim was false once the DM window existed, and it routed private answers into channels"
        );
        assert!(
            SUBAGENT_NOTE.contains(DM_FROM_USER_MARKER),
            "must teach the tag that identifies the human's messages (D64)"
        );
    }

    /// A crew member's memory arrives as a system block, not as history and not as
    /// a message: nobody said it, and the whole point of D51 is that the past stays
    /// on disk until the member decides to fetch it. An ad-hoc subagent has no past
    /// and is told nothing.
    #[test]
    fn memory_note_rides_the_system_prompt_when_there_is_one() {
        let (parent, _c) = parent_session();
        let note = "your past is at /tmp/qa.md".to_string();
        let sub = build_sub_session(
            &parent,
            None,
            None,
            None,
            None,
            "qa",
            MemberContext {
                memory: Some(note.clone()),
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("spawn: {e}"));
        assert!(
            sub.system.iter().any(|b| b.text == note),
            "the pointer is in the system prompt"
        );
        assert!(
            sub.system.iter().all(|b| !b.cache),
            "a per-member tail block must not open another cache breakpoint"
        );

        let solo = build_sub_session(
            &parent,
            None,
            None,
            None,
            None,
            "solo",
            MemberContext::default(),
        )
        .unwrap_or_else(|e| panic!("spawn: {e}"));
        assert!(
            !solo
                .system
                .iter()
                .any(|b| b.text.contains("your past is at")),
            "an ad-hoc subagent is told nothing about a past it does not have"
        );
    }

    /// No named definition: the parent's system carries over, plus the note.
    #[test]
    fn plain_subagent_inherits_parent_system_plus_note() {
        let (session, _client) = parent_session();
        let sub = build_sub_session(
            &session,
            None,
            None,
            None,
            None,
            "worker",
            MemberContext::default(),
        )
        .unwrap();
        let texts: Vec<&str> = sub.system.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "parent system",
                SUBAGENT_NOTE,
                &capability_block("parent-model", "default")
            ]
        );
        let moved = std::env::temp_dir().join("bingo-subagent-shared-cwd");
        session.set_cwd(moved.clone());
        assert_eq!(
            sub.cwd(),
            moved,
            "ad-hoc subagents follow the parent session's cwd"
        );
        assert!(
            !sub.system.last().map(|b| b.cache).unwrap_or(true),
            "the note block does not occupy a cache breakpoint"
        );
    }

    /// MCP connections and the permission table are shared handles, not snapshots: a subagent
    /// sees the parent's MCP tools, and `/permissions` edits reach instances already running.
    #[test]
    fn sub_session_shares_parent_mcp_and_permissions() {
        let (parent, _) = parent_session();
        let sub = build_sub_session(
            &parent,
            None,
            None,
            None,
            None,
            "worker",
            MemberContext::default(),
        )
        .unwrap();
        assert!(
            Arc::ptr_eq(&sub.runtime.mcp, &parent.runtime.mcp),
            "the MCP manager should be shared, otherwise subagents get no MCP tools"
        );
        assert!(
            Arc::ptr_eq(&sub.runtime.permissions, &parent.runtime.permissions),
            "the permission tables should be shared, otherwise /permissions changes after spawn never reach subagents"
        );
    }

    /// Thinking deltas reach the live tail as their own blocks — one per
    /// phase, closed by whatever interrupts it — so the DM can show the
    /// reasoning happening, while the flat reply output stays prose-only.
    #[tokio::test]
    async fn thinking_deltas_open_one_live_block_per_phase() {
        let output = Arc::new(Mutex::new(String::new()));
        let live = Arc::new(Mutex::new(Vec::new()));
        let progress = Arc::new(Mutex::new(crate::agents::AgentProgress::default()));
        let watch = crate::watch::WatchRegistry::new();
        let registry = AgentRegistry::new();
        let cell = Arc::new(AgentCell::new(registry.clone()));
        let id = register_run_watch(&watch, "think".into(), cell.clone(), Vec::new(), None, true);
        let mut ui = subagent_hooks(
            SubagentOutput {
                text: output.clone(),
                live: live.clone(),
                progress,
            },
            Arc::new(Mutex::new(crate::token_rate::TokenRateSampler::default())),
            cell,
            watch,
            id,
            "worker".into(),
            None,
        );
        (ui.on_event)(&crate::api::contract::StreamEvent::ThinkingDelta {
            index: 0,
            thinking: "first ".into(),
        });
        (ui.on_event)(&crate::api::contract::StreamEvent::ThinkingDelta {
            index: 0,
            thinking: "phase".into(),
        });
        (ui.on_tool_ready)(
            "test-tool".into(),
            "Read".into(),
            serde_json::json!({"file_path": "a"}),
            false,
        );
        (ui.on_event)(&crate::api::contract::StreamEvent::ThinkingDelta {
            index: 0,
            thinking: "second phase".into(),
        });
        (ui.on_event)(&crate::api::contract::StreamEvent::TextDelta {
            index: 0,
            text: "the answer".into(),
        });

        let live = live.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(live.len(), 4, "{live:?}");
        assert!(
            matches!(&live[0], crate::agents::LiveBlock::Thinking(t) if t == "first phase"),
            "consecutive deltas fold into one phase: {live:?}"
        );
        assert!(matches!(&live[1], crate::agents::LiveBlock::Tool(_)));
        assert!(
            matches!(&live[2], crate::agents::LiveBlock::Thinking(t) if t == "second phase"),
            "a tool call closes the phase: {live:?}"
        );
        assert!(matches!(&live[3], crate::agents::LiveBlock::Text(t) if t == "the answer"));
        assert_eq!(
            &*output.lock().unwrap_or_else(|e| e.into_inner()),
            "the answer",
            "reasoning never leaks into the flat reply"
        );
    }

    #[tokio::test]
    async fn subagent_retry_restores_the_current_attempt_checkpoint() {
        let output = Arc::new(Mutex::new(String::new()));
        let live = Arc::new(Mutex::new(Vec::new()));
        let progress = Arc::new(Mutex::new(crate::agents::AgentProgress::default()));
        let watch = crate::watch::WatchRegistry::new();
        let registry = AgentRegistry::new();
        let cell = Arc::new(AgentCell::new(registry.clone()));
        let id = register_run_watch(&watch, "retry".into(), cell.clone(), Vec::new(), None, true);
        let mut ui = subagent_hooks(
            SubagentOutput {
                text: output.clone(),
                live: live.clone(),
                progress: progress.clone(),
            },
            Arc::new(Mutex::new(crate::token_rate::TokenRateSampler::default())),
            cell.clone(),
            watch,
            id,
            "worker".into(),
            None,
        );
        (ui.on_event)(&crate::api::contract::StreamEvent::TextDelta {
            index: 0,
            text: "committed".into(),
        });
        (ui.on_tool_ready)(
            "test-tool".into(),
            "Read".into(),
            serde_json::json!({"file_path":"a"}),
            false,
        );
        (ui.on_round_end)();
        (ui.on_event)(&crate::api::contract::StreamEvent::TextDelta {
            index: 0,
            text: "partial".into(),
        });
        (ui.on_tool_ready)(
            "test-tool".into(),
            "Bash".into(),
            serde_json::json!({"command":"bad"}),
            false,
        );
        (ui.on_stream_retry)();
        (ui.on_warning)("Reconnecting... 2/10".into());
        (ui.on_event)(&crate::api::contract::StreamEvent::TextDelta {
            index: 0,
            text: "answer".into(),
        });

        assert_eq!(
            &*output.lock().unwrap_or_else(|e| e.into_inner()),
            "committedanswer"
        );
        let live = live.lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            matches!(live.first(), Some(crate::agents::LiveBlock::Text(text)) if text == "committed")
        );
        assert!(matches!(
            live.get(live.len().saturating_sub(2)),
            Some(crate::agents::LiveBlock::Text(text)) if text == "Reconnecting... 2/10"
        ));
        assert!(matches!(
            live.last(),
            Some(crate::agents::LiveBlock::Text(text)) if text == "answer"
        ));
        let progress = progress.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(progress.tool_uses, 1);
        assert_eq!(cell.chars(), "committedanswer".chars().count());
    }

    #[tokio::test]
    async fn subagent_progress_accumulates_tokens_tools_and_recent_activity() {
        let output = Arc::new(Mutex::new(String::new()));
        let live = Arc::new(Mutex::new(Vec::new()));
        let progress = Arc::new(Mutex::new(crate::agents::AgentProgress::default()));
        progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .start_run();
        let watch = crate::watch::WatchRegistry::new();
        let registry = AgentRegistry::new();
        let id = register_run_watch(
            &watch,
            "progress".into(),
            Arc::new(AgentCell::new(registry.clone())),
            Vec::new(),
            None,
            true,
        );
        let mut ui = subagent_hooks(
            SubagentOutput {
                text: output,
                live,
                progress: progress.clone(),
            },
            Arc::new(Mutex::new(crate::token_rate::TokenRateSampler::default())),
            Arc::new(AgentCell::new(registry.clone())),
            watch,
            id,
            "worker".into(),
            None,
        );
        (ui.on_event)(&crate::api::contract::StreamEvent::StopReason {
            stop_reason: Some("tool_use".into()),
            output_tokens: Some(12),
        });
        (ui.on_tool_ready)(
            "test-tool".into(),
            "Read".into(),
            serde_json::json!({"file_path":"src/main.rs"}),
            false,
        );
        (ui.on_event)(&crate::api::contract::StreamEvent::StopReason {
            stop_reason: Some("end_turn".into()),
            output_tokens: Some(7),
        });
        (ui.on_tool_ready)(
            "test-tool".into(),
            "Bash".into(),
            serde_json::json!({"command":"cargo check"}),
            false,
        );
        let progress = progress.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(progress.output_tokens, 19);
        assert_eq!(progress.tool_uses, 2);
        assert_eq!(progress.recent_activity.len(), 2);
        assert!(progress.recent_activity[0].contains("Read"));
        assert!(progress.recent_activity[1].contains("Bash"));
    }

    /// A subagent's Ask decision is forwarded to the attached prompt surface, stamped with the
    /// instance name — never silently auto-denied (or auto-allowed under bypass).
    #[tokio::test]
    async fn subagent_hooks_touch_activity_on_stream_and_tool_signals() {
        let session = parent_session().0;
        session.agents.insert(
            "worker",
            AgentKind::Hire,
            None,
            "work".into(),
            session.clone(),
        );
        let watch = crate::watch::WatchRegistry::new();
        let registry = session.agents.clone();
        let id = register_run_watch(
            &watch,
            "l".into(),
            Arc::new(AgentCell::new(registry.clone())),
            Vec::new(),
            None,
            true,
        );
        let mut ui = subagent_hooks(
            SubagentOutput {
                text: Arc::new(Mutex::new(String::new())),
                live: Arc::new(Mutex::new(Vec::new())),
                progress: Arc::new(Mutex::new(crate::agents::AgentProgress::default())),
            },
            Arc::new(Mutex::new(crate::token_rate::TokenRateSampler::default())),
            Arc::new(AgentCell::new(registry.clone())),
            watch,
            id,
            "worker".into(),
            None,
        );
        let inserted = session.agents.list()[0].last_active;
        std::thread::sleep(std::time::Duration::from_millis(2));
        (ui.on_event)(&crate::api::contract::StreamEvent::TextDelta {
            index: 0,
            text: "hi".into(),
        });
        let streamed = session.agents.list()[0].last_active;
        assert!(streamed > inserted);

        std::thread::sleep(std::time::Duration::from_millis(2));
        (ui.on_tool_ready)(
            "test-tool".into(),
            "Read".into(),
            serde_json::json!({"file_path": "a"}),
            false,
        );
        let ready = session.agents.list()[0].last_active;
        assert!(ready > streamed);

        std::thread::sleep(std::time::Duration::from_millis(2));
        (ui.on_tool_done)(&crate::query::ToolCallDone {
            tool_call_id: "test-tool".into(),
            name: "Read".into(),
            summary: String::new(),
            output: String::new(),
            status: crate::query::ToolCallStatus::Done,
            diff: None,
            duration_ms: 1,
        });
        assert!(session.agents.list()[0].last_active > ready);
    }

    #[tokio::test]
    async fn subagent_ask_forwards_to_attached_prompt() {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorder = seen.clone();
        let ask: Arc<crate::query::AskFn> = Arc::new(move |request| {
            recorder
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(format!(
                    "{}|{}|{}",
                    request.tool,
                    request.reason,
                    request.scope.unwrap_or("-")
                ));
            Box::pin(async { crate::query::AskOutcome::Allow })
        });
        let watch = crate::watch::WatchRegistry::new();
        let registry = AgentRegistry::new();
        let id = register_run_watch(
            &watch,
            "l".into(),
            Arc::new(AgentCell::new(registry.clone())),
            Vec::new(),
            None,
            true,
        );
        let ui = subagent_hooks(
            SubagentOutput {
                text: Arc::new(Mutex::new(String::new())),
                live: Arc::new(Mutex::new(Vec::new())),
                progress: Arc::new(Mutex::new(crate::agents::AgentProgress::default())),
            },
            Arc::new(Mutex::new(crate::token_rate::TokenRateSampler::default())),
            Arc::new(AgentCell::new(registry.clone())),
            watch.clone(),
            id,
            "worker".into(),
            Some(ask),
        );
        let input = serde_json::json!({ "file_path": "/tmp/x.txt" });
        let request = crate::query::AskContext {
            tool: "Write",
            reason: "Write needs permission",
            input: &input,
            cwd: &std::env::temp_dir(),
            scope: Some("Write(/tmp/)"),
            diff: None,
        };
        assert!((ui.ask)(&request).await.allowed());
        assert_eq!(
            seen.lock().unwrap_or_else(|e| e.into_inner()).as_slice(),
            ["Write|worker · Write needs permission|Write(/tmp/)"],
            "the instance stamps the reason; the scope travels untouched"
        );

        // Nothing attached (embedded/test path): deny rather than block on a modal nobody shows.
        let ui = subagent_hooks(
            SubagentOutput {
                text: Arc::new(Mutex::new(String::new())),
                live: Arc::new(Mutex::new(Vec::new())),
                progress: Arc::new(Mutex::new(crate::agents::AgentProgress::default())),
            },
            Arc::new(Mutex::new(crate::token_rate::TokenRateSampler::default())),
            Arc::new(AgentCell::new(registry.clone())),
            watch,
            id,
            "worker".into(),
            None,
        );
        assert_eq!(
            (ui.ask)(&request).await,
            crate::query::AskOutcome::Deny { feedback: None }
        );
    }
}
