use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;

use crate::agents::{AgentDef, AgentRegistry, FollowUp, InboxItem, MAX_FOLLOW_UPS, MsgId};
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
const SUBAGENT_NOTE: &str = "\
# You are a subagent

- The main agent (the hub) spawned you for one task. Your final text is returned to the hub
  as its tool result; it is not displayed to the user, and markdown image blocks are not
  rendered for anyone. Put conclusions in the text itself.
- You cannot question the user: AskUserQuestion is not available here. Permission prompts do
  reach the user, but anything else you need must be reported back to the hub.
- Your turn ends when you stop calling tools, and background tasks you started will NOT wake
  you afterwards. Finish what needs finishing within this turn, or state what is still
  pending — the hub can resume you with a follow-up message.";

/// Appended when agent channels are on. Two failure modes pull in opposite directions and the
/// note has to hold both: a room of polite agents acknowledging each other's acknowledgements
/// (D45), and a room so afraid of chatter that nobody answers the human at all (D48).
///
/// The rule that separates them is *who spoke*, not how the message reads — a person answers
/// their manager and ignores their colleagues' hellos — plus the mechanical fact the model
/// cannot infer: a turn woken by a channel message reports back to the hub, so a reply written
/// as turn text never reaches the room. Without that sentence the model believes it has already
/// answered and stays silent on purpose.
///
/// It lives in the system prompt rather than in the wake-up payload deliberately: compaction
/// rewrites the message history but never touches `Session::system`, so the rule is still there
/// on turn fifty, when a long-running member has forgotten everything else about the room.
const CHANNEL_NOTE: &str = "\
# Speaking in a channel

**Only `Post` puts words in the room.** The text you write in a turn woken by a channel message
goes back to the hub as your result — nobody in the channel sees it. Writing \"standing by, no
channel reply needed\" as your turn text is not an answer to the room; it is a private note to
your manager, and from the room it is indistinguishable from ignoring the message. If you decide
to answer, call Post.

**Who spoke decides whether you owe a reply** — not how the message is worded.

- **`user` or `main` addressed the room**: answer once, briefly, with Post. When the person
  running the room greets the team, asks who is around, or puts a question to everyone, a human
  answers — silence reads as absence, not as discipline. One short line, in your own voice, then
  stop.
- **Another member spoke**: you owe them nothing. Post only if they named you, you can unblock
  them, you disagree, or you are holding the result they are waiting on.
- **Never answer an answer.** A room does not flood because members reply to the human; it floods
  because they reply to each other's replies. Your line is the end of that thread — do not
  acknowledge, thank, agree with, or restate what a colleague just said.

Beyond that first line, post only what changes what someone else will do: a decision someone is
blocked on, a disagreement, a result, a question you cannot continue without. Name the person you
mean. When you have nothing to add, stop calling tools — silence costs nothing and wakes nobody.

A direct instruction sent to you alone is a different thing: it is not channel traffic, and your
turn text is exactly where the hub is listening for the answer.";

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

/// Sub-agent UI: captures text, renders nothing, and forwards permission prompts to the
/// session that owns the UI. The cell tracks the number of characters produced (for interval
/// progress checks of background agents).
fn subagent_hooks(
    output: Arc<Mutex<String>>,
    live: Arc<Mutex<Vec<crate::agents::LiveBlock>>>,
    cell: Arc<AgentCell>,
    watch: Arc<WatchRegistry>,
    id: WatchId,
    instance: String,
    ask: Option<Arc<crate::query::AskFn>>,
) -> UiHooks {
    // `output` stays the flat reply (what the spawn returns and what `spoke` is
    // judged on); `live` is the same turn as the instance view needs to show it,
    // with the tool calls and round boundaries the flat string cannot carry.
    let tool_live = live.clone();
    let round_live = live.clone();
    UiHooks {
        on_event: Box::new(move |event| {
            if let crate::api::contract::StreamEvent::TextDelta { text, .. } = event
                && let Ok(mut output) = output.lock()
            {
                output.push_str(text);
                if let Ok(mut live) = live.lock() {
                    crate::agents::LiveBlock::push_text(&mut live, text);
                }
                cell.record_chars(text.chars().count());
                // Feed produced text into the condition engine (notify_on hit → signal notification).
                watch.feed_content(id, text);
            }
        }),
        on_tool_ready: Box::new(move |_tool_call_id, name, input, _standalone| {
            let Ok(mut live) = tool_live.lock() else {
                return;
            };
            let glyph = crate::tui::activities::tool_glyph(&name);
            let shown = crate::tui::activities::display_tool_name(&name);
            let summary = crate::query::summarize_input(&name, &input);
            live.push(crate::agents::LiveBlock::Tool(if summary.is_empty() {
                format!("{glyph}{shown}")
            } else {
                format!("{glyph}{shown}({summary})")
            }));
        }),
        on_tool_done: Box::new(|_| {}),
        // A round boundary closes the open prose block, so the next round's first
        // sentence does not run into the previous round's last one.
        on_round_end: Box::new(move || {
            if let Ok(mut live) = round_live.lock()
                && matches!(live.last(), Some(crate::agents::LiveBlock::Text(_)))
            {
                live.push(crate::agents::LiveBlock::Text(String::new()));
            }
        }),
        on_warning: Box::new(|_| {}),
        // A subagent has no prompt surface of its own, so its Ask decisions are forwarded to
        // the session that owns the UI, stamped with the instance name. Auto-denying here
        // would fail the tool call as "user denied" without the user ever being asked — and
        // auto-allowing under bypassPermissions would silently clear the safety-check gate
        // that is supposed to survive bypass.
        ask: std::sync::Arc::new(move |tool_name, reason| {
            // No prompt surface attached: both real entry points (TUI, headless) attach one at
            // startup, so this is the embedded/test path — fall back to denying.
            let Some(ask) = ask.clone() else {
                return Box::pin(async { false });
            };
            let request = format!("{instance} · {reason}");
            let tool_name = tool_name.to_string();
            Box::pin(async move {
                let _serialized = ask_gate().lock().await;
                ask(&tool_name, &request).await
            })
        }),
        // AskUserQuestion is not assembled for subagents (see `assemble_tools`); if one ever
        // reaches here, treat it as unanswered rather than blocking on a modal.
        ask_question: std::sync::Arc::new(|_title, _question, _options| Box::pin(async { None })),
    }
}

/// Turn-boundary delivery: start a run for every instance that has messages waiting.
///
/// This is the only place a queued message becomes a running turn. Holding delivery until the
/// boundary is what makes a batch a batch — messages sent during one turn arrive together
/// instead of one per turn — and it doubles as the recovery path for a run chain that died
/// with messages still in its inbox.
pub(crate) fn flush_agent_inbox(session: &Arc<Session>, watch: &Arc<WatchRegistry>) {
    for wake in session.agents.flush_pending() {
        let (prompt, images) = absorb_inbox(&session.channels, &wake.name, &wake.items);
        let label = format!("{} #{} · {}", wake.name, wake.run, excerpt(&prompt));
        spawn_agent_loop(
            session.agents.clone(),
            watch.clone(),
            wake.name,
            wake.session,
            wake.history,
            prompt,
            images,
            label,
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

/// Inbox → turn prompt plus the images those instructions carried: a single hub instruction is
/// kept verbatim; mixed or multiple entries are annotated with their sources in order. Channel
/// entries also advance the member's read cursor (messages enter its context with this turn).
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
        [InboxItem::Direct { text, .. }] => text.clone(),
        _ => items
            .iter()
            .map(|item| match item {
                InboxItem::Direct { text, .. } => format!("[follow-up instruction] {text}"),
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
) -> WatchId {
    watch.register_with_conditions(
        Box::new(AgentWatch {
            cell,
            label,
            interval: Some(std::time::Duration::from_secs(5)),
        }),
        conditions,
        owner,
    )
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
    first_label: String,
    conditions: Vec<NotifyCondition>,
    owner: Option<String>,
) -> WatchId {
    let cell = Arc::new(AgentCell::new());
    let first_id = register_run_watch(&watch, first_label, cell.clone(), conditions, owner.clone());
    registry.set_run_watch(&name, first_id);
    let loop_registry = registry.clone();
    let loop_name = name.clone();
    let handle = tokio::spawn(async move {
        let name = loop_name;
        let mut history = history;
        let mut prompt = prompt;
        let mut images = images;
        let mut run = (first_id, cell);
        loop {
            let output = Arc::new(Mutex::new(String::new()));
            let live = Arc::new(Mutex::new(Vec::new()));
            loop_registry.set_live(&name, Some(live.clone()));
            let mut ui = subagent_hooks(
                output.clone(),
                live.clone(),
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
                    let spoke = !text.trim().is_empty();
                    loop_registry.set_live(&name, None);
                    watch.set_state(
                        run.0,
                        WatchState::Done,
                        Some("done".to_string()),
                        Some(serde_json::json!(non_empty(text))),
                    );
                    match loop_registry.finish(&name, outcome.messages, spoke) {
                        Some(next) => {
                            history = next.history;
                            (prompt, images) = absorb_inbox(&session.channels, &name, &next.items);
                            let cell = Arc::new(AgentCell::new());
                            let label = format!("{name} #{} · {}", next.run, excerpt(&prompt));
                            let id = register_run_watch(
                                &watch,
                                label,
                                cell.clone(),
                                Vec::new(),
                                owner.clone(),
                            );
                            loop_registry.set_run_watch(&name, id);
                            run = (id, cell);
                        }
                        None => break,
                    }
                }
                Err(e) => {
                    loop_registry.set_live(&name, None);
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
    registry.set_abort(&name, handle.abort_handle());
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
    ) -> Result<(String, String, Arc<Session>), ToolError> {
        let base = params
            .name
            .clone()
            .or_else(|| def.map(|d| d.name.clone()))
            .unwrap_or_else(|| "agent".to_string());
        let name = self.session.agents.claim_name(&base);
        let sub_session = self.build_sub_session(params, def, &name)?;
        let description = params
            .description
            .clone()
            .unwrap_or_else(|| excerpt(&params.prompt));
        self.session.agents.insert(
            &name,
            def.map(|d| d.name.clone()),
            description.clone(),
            sub_session.clone(),
        );
        Ok((name, description, sub_session))
    }

    fn launch_background(
        &self,
        params: &AgentInput,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let def = self.resolve_def(params)?;
        let (name, description, sub_session) = self.spawn_instance(params, def)?;
        let _ = self.session.agents.next_run(&name);
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
            format!("{name} · {description}"),
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
    ) -> Result<Arc<Session>, ToolError> {
        build_sub_session(
            &self.session,
            params.model.clone(),
            params.provider.clone(),
            params.thinking.clone(),
            def,
            instance,
            // An ad-hoc subagent has no past on disk: memory belongs to a crew
            // member, which is the thing a blueprint keeps across sessions.
            None,
        )
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
/// `memory` is the pointer a team member gets to its own past on disk (D51) — a
/// system block rather than a message, because nobody said it.
pub(crate) fn build_sub_session(
    parent: &Arc<Session>,
    model: Option<String>,
    provider: Option<String>,
    thinking: Option<String>,
    def: Option<&AgentDef>,
    instance: &str,
    memory: Option<String>,
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
    // Only when the feature is on: channel etiquette is noise for a solo subagent that will
    // never see a room.
    if parent.settings.experimental.agent_channels {
        system.push(SystemBlock {
            text: CHANNEL_NOTE.to_string(),
            cache: false,
        });
    }
    // Where this instance's own past lives, for the instances that have one.
    if let Some(memory) = memory {
        system.push(SystemBlock {
            text: memory,
            cache: false,
        });
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
}

impl AgentCell {
    fn new() -> Self {
        Self {
            chars: std::sync::atomic::AtomicUsize::new(0),
        }
    }
    fn record_chars(&self, n: usize) {
        self.chars.fetch_add(n, std::sync::atomic::Ordering::SeqCst);
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
        let (name, description, sub_session) = self.spawn_instance(&params, def)?;
        let _ = self.session.agents.next_run(&name);

        // Foreground sub-agents can also be watched: Running (characters produced) → Done/Failed.
        let cell = Arc::new(AgentCell::new());
        let conditions = params
            .notify_on
            .clone()
            .map(|p| vec![NotifyCondition::Contains(p)])
            .unwrap_or_default();
        let id = register_run_watch(
            &ctx.watch,
            format!("{name} · {description}"),
            cell.clone(),
            conditions,
            ctx.instance.clone(),
        );
        self.session.agents.set_run_watch(&name, id);
        let output = Arc::new(Mutex::new(String::new()));
        let live = Arc::new(Mutex::new(Vec::new()));
        self.session.agents.set_live(&name, Some(live.clone()));
        let mut ui = subagent_hooks(
            output.clone(),
            live.clone(),
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
        self.session.agents.set_live(&name, None);
        match sync_run {
            Ok(outcome) => {
                let text = output.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let content = non_empty(text);
                ctx.watch.set_state(
                    id,
                    WatchState::Done,
                    Some("done".to_string()),
                    Some(serde_json::json!(content.clone())),
                );
                // On the synchronous path tools run serially, so queued messages never reach here;
                // if one somehow does, hand it to the background loop (same continuation mechanism).
                let spoke = !content.trim().is_empty();
                if let Some(next) = self.session.agents.finish(&name, outcome.messages, spoke) {
                    let (prompt, images) = absorb_inbox(&sub_session.channels, &name, &next.items);
                    spawn_agent_loop(
                        self.session.agents.clone(),
                        ctx.watch.clone(),
                        name.clone(),
                        sub_session,
                        next.history,
                        prompt.clone(),
                        images,
                        format!("{name} #{} · {}", next.run, excerpt(&prompt)),
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
        description = "Target subagent instance name (the name returned by the Agent tool; see AgentControl list)"
    )]
    agent: String,
    #[schemars(description = "Follow-up instruction/message to send")]
    message: String,
    /// Reply wait: arms the follow-up watchdog (see `spawn_ack_watchdog`).
    #[serde(default)]
    #[schemars(
        description = "Reply wait in seconds, defaulting to 300 when omitted — the check is on by default, since a message nobody ever answers is the failure you would otherwise find out about last. Once the wait elapses the harness re-checks the same record AgentControl(action=messages) reports, and while you are still owed an answer — the message is queued, or it was read into a turn that ended saying nothing — it sends the receiver a follow-up asking it to reply, at most 3 rounds; anything other than an answer inside the wait comes back to you as a task notification. Shorten it when you are actively waiting on this instance, lengthen it for a long task that will be quiet for a while (clamped to 5-3600), or pass 0 to switch the check off for a message you need no answer to."
    )]
    ack_timeout: Option<u64>,
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

/// Main→sub continuation channel (hub-and-spoke, main session only): an idle instance is woken
/// with its full history to continue; a busy instance queues the message and it is delivered
/// when the turn ends.
pub struct SendMessageTool {
    session: Arc<Session>,
}

impl SendMessageTool {
    pub fn new(session: Arc<Session>) -> Self {
        Self { session }
    }
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> String {
        "SendMessage".to_string()
    }
    fn description(&self) -> String {
        "Send a follow-up instruction to a spawned subagent instance (a continuation that keeps its context). Returns a message_id: the message is queued and delivered at the end of this turn, batched with any other message sent to the same instance in this turn, so the receiver reads them together rather than one per turn. Neither queued nor delivered is an acknowledgement — a receiver can read a message and end its turn saying nothing. AgentControl(action=messages) reports which of those it is: queued, delivered but unanswered, answered (with the run that replied), or dropped because the instance stopped. You do not have to poll that yourself: the harness runs the same check five minutes after sending (tune with ack_timeout) and follows up on the receiver, up to 3 rounds, reporting back if no answer ever comes. The instance name comes from the Agent tool's return value or AgentControl list.".to_string()
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
            .deliver(&params.agent, &params.message, images, timeout)
            .map_err(ToolError::failed)?;
        let note = match timeout {
            Some(t) => format!(
                "delivered in a batch with the recipient's other messages at the end of this turn; if no reply arrives within {}s (including read-but-silent rounds), it is automatically re-checked and chased (up to {MAX_FOLLOW_UPS} rounds); the outcome is reported as a task notification",
                t.as_secs()
            ),
            None => "delivered in a batch with the recipient's other messages at the end of this turn;\
                      follow-up chasing is off (ack_timeout=0); check yourself with AgentControl(action=messages, agent=…) when needed"
                .to_string(),
        };
        if let Some(timeout) = timeout {
            spawn_ack_watchdog(
                self.session.clone(),
                ctx.watch.clone(),
                params.agent.clone(),
                id,
                timeout,
            );
        }
        Ok(ToolResult {
            content: serde_json::json!({
                "status": "queued",
                "message_id": id.0,
                "agent": params.agent,
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

#[async_trait]
impl Tool for AgentControlTool {
    fn name(&self) -> String {
        "AgentControl".to_string()
    }
    fn description(&self) -> String {
        "Manage subagent instances: list all (name/definition/status/queued-instruction count), check messages sent to one (per-message queued/delivered-but-unanswered/answered/dropped, how long it has been waiting, and whether SendMessage's ack_timeout is already chasing it — use this when an instance has gone quiet on you), stop one (aborts the current run, stops accepting instructions; history kept), delete one (stops and removes it; the name is released).".to_string()
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
                            format!(
                                "- {} ({}{def}{pending}{unacked}, {} @ {}): {}",
                                s.name,
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
                                    "queued (waiting {}s{}, will be delivered in a batch at the next turn boundary)",
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

    #[test]
    fn sub_session_inherits_model_and_shared_endpoint() {
        let (session, client) = parent_session();
        let _ = session.runtime.thinking_tx.send(Some("medium".into()));
        let tool = AgentTool::new(session.clone(), Vec::new());
        let sub = tool
            .build_sub_session(&params("do it"), None, "sub")
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
        let sub = tool.build_sub_session(&p, None, "sub").unwrap();
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
            .build_sub_session(&params("review"), Some(&d), "sub")
            .unwrap();
        // Default is append: parent system + persona + the subagent note block.
        let texts: Vec<&str> = sub.system.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(
            texts,
            ["parent system", "You are the reviewer.", SUBAGENT_NOTE],
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
        let sub = tool.build_sub_session(&p, Some(&d), "sub").unwrap();
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
            tool.build_sub_session(&p, None, "sub").is_err(),
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
        let err = sub_err(tool.build_sub_session(&p, None, "sub"));
        assert!(
            err.contains("requires a model") && err.contains("ds"),
            "crossing providers requires an explicit model: {err}"
        );
        // The definition provides a provider but no model → errors the same way.
        let mut d = def("reviewer");
        d.model = None;
        let tool = AgentTool::new(session.clone(), vec![d.clone()]);
        let err = sub_err(tool.build_sub_session(&params("review"), Some(&d), "sub"));
        assert!(
            err.contains("requires a model"),
            "the definition-side cross-provider case errors the same way: {err}"
        );
        // Same provider (the parent's current is ds) → inherits the model, no error.
        let _ = session.runtime.provider_tx.send("ds".into());
        let tool = AgentTool::new(session.clone(), Vec::new());
        let mut p = params("do it");
        p.provider = Some("ds".into());
        let sub = tool.build_sub_session(&p, None, "sub").unwrap();
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
        let sub = tool.build_sub_session(&p, None, "sub").unwrap();
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
        let sub = tool.build_sub_session(&p, None, "sub").unwrap();
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
        let sub = tool.build_sub_session(&p, None, "sub").unwrap();
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
        let sub = tool.build_sub_session(&p, None, "sub").unwrap();
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
            .build_sub_session(&params("review"), Some(&d), "sub")
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
            let err = sub_err(tool.build_sub_session(&p, None, "sub"));
            assert!(
                err.contains("invalid thinking level"),
                "invalid level {bad:?} should error: {err}"
            );
        }
        // An invalid definition-side value errors the same way.
        let mut d = def("reviewer");
        d.thinking = Some("bogus".into());
        let tool = AgentTool::new(session.clone(), vec![d.clone()]);
        let err = sub_err(tool.build_sub_session(&params("review"), Some(&d), "sub"));
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

    #[tokio::test]
    async fn agent_control_list_stop_delete() {
        let (session, _client) = parent_session();
        session
            .agents
            .insert("scout", None, "research".into(), session.clone());
        let ctl = AgentControlTool::new(session.clone());
        let ctx = crate::tool::ToolContext {
            home: std::env::temp_dir(),
            cwd: std::path::PathBuf::from("/tmp"),
            watch: session.watch.clone(),
            http: reqwest::Client::new(),
            tasks: session.tasks.clone(),
            hooks: crate::settings::HooksConfig::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
            instance: None,
        };
        assert!(ctl.is_read_only(&serde_json::json!({"action": "list"})));
        assert!(!ctl.is_read_only(&serde_json::json!({"action": "stop", "agent": "scout"})));
        let out = ctl
            .call(serde_json::json!({"action": "list"}), &ctx)
            .await
            .unwrap();
        let text = out.content.as_str().unwrap();
        assert!(text.contains("scout") && text.contains("running"), "{text}");
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
            .call(serde_json::json!({"agent": "scout", "message": "hi"}), &ctx)
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
    async fn send_message_queues_on_running_instance() {
        let (session, _client) = parent_session();
        session
            .agents
            .insert("worker", None, "do work".into(), session.clone());
        let send = SendMessageTool::new(session.clone());
        let ctx = hub_ctx(&session);
        // The acknowledgement wait is opt-in: omitting it keeps the plain fire-and-forget path.
        let schema = send.input_schema();
        assert!(schema["properties"]["ack_timeout"].is_object());
        assert_eq!(schema["required"], serde_json::json!(["agent", "message"]));
        let out = send
            .call(
                serde_json::json!({"agent": "worker", "message": "add more"}),
                &ctx,
            )
            .await
            .unwrap();
        // The receipt carries the message id; delivery itself waits for the turn boundary.
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
            .call(serde_json::json!({"agent": "nobody", "message": "x"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("worker"), "{err}");
    }

    /// The chase protects a sender who never thought to ask for it — that is the whole point of a
    /// default. Opting out has to be said out loud.
    #[tokio::test]
    async fn the_reply_check_is_on_by_default_and_zero_turns_it_off() {
        let (session, _client) = parent_session();
        session
            .agents
            .insert("worker", None, "do work".into(), session.clone());
        let send = SendMessageTool::new(session.clone());
        let ctx = hub_ctx(&session);
        let receipt = |out: ToolResult| -> serde_json::Value {
            serde_json::from_str(out.content.as_str().unwrap_or_default())
                .unwrap_or_else(|e| panic!("{e}"))
        };

        let out = send
            .call(
                serde_json::json!({"agent": "worker", "message": "default"}),
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
                serde_json::json!({"agent": "worker", "message": "no wait for a reply", "ack_timeout": 0}),
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
            http: reqwest::Client::new(),
            tasks: session.tasks.clone(),
            hooks: crate::settings::HooksConfig::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
            instance: None,
        }
    }

    /// A message that is never picked up is chased on the sender's own clock and then reported:
    /// three follow-ups ride along with it, and the give-up lands in the hub's notification queue
    /// rather than staying an unanswered "queued" nobody looks at again.
    #[tokio::test(start_paused = true)]
    async fn unacknowledged_message_is_chased_three_times_then_reported() {
        let (session, _client) = parent_session();
        // Running: the boundary flush cannot claim it, so the message really does stay queued.
        session
            .agents
            .insert("worker", None, "do work".into(), session.clone());
        let ctx = hub_ctx(&session);
        let out = SendMessageTool::new(session.clone())
            .call(
                serde_json::json!({"agent": "worker", "message": "check the logs", "ack_timeout": 1}),
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
        session
            .agents
            .insert("mute", None, "silent".into(), session.clone());
        let ctx = hub_ctx(&session);
        SendMessageTool::new(session.clone())
            .call(
                serde_json::json!({"agent": "mute", "message": "report progress", "ack_timeout": 5}),
                &ctx,
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        // A turn ends without a word and takes the queued message into the next one: delivered,
        // unanswered, and still Running — so the flush the watchdog retries stays a no-op here.
        assert!(session.agents.finish("mute", Vec::new(), false).is_some());
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
        session
            .agents
            .insert("worker", None, "do work".into(), session.clone());
        let ctx = hub_ctx(&session);
        SendMessageTool::new(session.clone())
            .call(
                serde_json::json!({"agent": "worker", "message": "check the logs", "ack_timeout": 60}),
                &ctx,
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        // The receiver picks it up at the boundary, then that run ends with something to say.
        assert!(session.agents.finish("worker", Vec::new(), true).is_some());
        assert!(session.agents.finish("worker", Vec::new(), true).is_none());
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
        let sub = build_sub_session(&session, None, None, None, None, "worker", None).unwrap();
        assert_eq!(sub.attachments.resolve("#[image 1]").len(), 1);

        // Follow-up: a queued instruction keeps its images until it is delivered.
        session
            .agents
            .insert("worker", None, "d".into(), sub.clone());
        let id = session
            .agents
            .deliver("worker", "compare #[image 1]", images.clone(), None)
            .unwrap_or_else(|e| panic!("{e}"));
        let (prompt, carried) = match session.agents.finish("worker", Vec::new(), true) {
            Some(next) => absorb_inbox(&sub.channels, "worker", &next.items),
            None => unreachable!("queued messages should be picked up at the turn boundary"),
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
            None,
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
            .build_sub_session(&params("review"), Some(&d), "sub")
            .unwrap();
        let texts: Vec<&str> = sub.system.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, ["You are the reviewer.", SUBAGENT_NOTE]);
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
        let sub = build_sub_session(&off, None, None, None, None, "solo", None)
            .unwrap_or_else(|e| panic!("spawn: {e}"));
        assert!(
            !sub.system.iter().any(|b| b.text == CHANNEL_NOTE),
            "channel etiquette must not be injected when channels are off"
        );

        let (mut on, _c2) = parent_session();
        let session = Arc::get_mut(&mut on).unwrap_or_else(|| panic!("exclusive"));
        session.settings.experimental.agent_channels = true;
        let sub = build_sub_session(&on, None, None, None, None, "member", None)
            .unwrap_or_else(|e| panic!("spawn: {e}"));
        assert!(sub.system.iter().any(|b| b.text == CHANNEL_NOTE));
        // Both failure modes have to survive edits to this text: the storm it was written
        // for, and the over-correction where nobody answers the human at all.
        assert!(
            CHANNEL_NOTE.contains("Never answer an answer"),
            "must name the reply-to-replies storm specifically, not just say \"keep it brief\""
        );
        assert!(
            CHANNEL_NOTE.contains("Only `Post` puts words in the room"),
            "must state that the turn body never reaches the channel — otherwise members think they already answered"
        );
        assert!(
            CHANNEL_NOTE.contains("`user` or `main` addressed the room"),
            "must spell out \"answer when a human speaks\", otherwise the silence rule overshoots"
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
        let sub = build_sub_session(&parent, None, None, None, None, "qa", Some(note.clone()))
            .unwrap_or_else(|e| panic!("spawn: {e}"));
        assert!(
            sub.system.iter().any(|b| b.text == note),
            "the pointer is in the system prompt"
        );
        assert!(
            sub.system.iter().all(|b| !b.cache),
            "a per-member tail block must not open another cache breakpoint"
        );

        let solo = build_sub_session(&parent, None, None, None, None, "solo", None)
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
        let sub = build_sub_session(&session, None, None, None, None, "worker", None).unwrap();
        let texts: Vec<&str> = sub.system.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, ["parent system", SUBAGENT_NOTE]);
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
        let sub = build_sub_session(&parent, None, None, None, None, "worker", None).unwrap();
        assert!(
            Arc::ptr_eq(&sub.runtime.mcp, &parent.runtime.mcp),
            "the MCP manager should be shared, otherwise subagents get no MCP tools"
        );
        assert!(
            Arc::ptr_eq(&sub.runtime.permissions, &parent.runtime.permissions),
            "the permission tables should be shared, otherwise /permissions changes after spawn never reach subagents"
        );
    }

    /// A subagent's Ask decision is forwarded to the attached prompt surface, stamped with the
    /// instance name — never silently auto-denied (or auto-allowed under bypass).
    #[tokio::test]
    async fn subagent_ask_forwards_to_attached_prompt() {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorder = seen.clone();
        let ask: Arc<crate::query::AskFn> = Arc::new(move |tool, reason| {
            recorder
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(format!("{tool}|{reason}"));
            Box::pin(async { true })
        });
        let watch = crate::watch::WatchRegistry::new();
        let id = register_run_watch(
            &watch,
            "l".into(),
            Arc::new(AgentCell::new()),
            Vec::new(),
            None,
        );
        let ui = subagent_hooks(
            Arc::new(Mutex::new(String::new())),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(AgentCell::new()),
            watch.clone(),
            id,
            "worker".into(),
            Some(ask),
        );
        assert!((ui.ask)("Write", "Write needs permission").await);
        assert_eq!(
            seen.lock().unwrap_or_else(|e| e.into_inner()).as_slice(),
            ["Write|worker · Write needs permission"]
        );

        // Nothing attached (embedded/test path): deny rather than block on a modal nobody shows.
        let ui = subagent_hooks(
            Arc::new(Mutex::new(String::new())),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(AgentCell::new()),
            watch,
            id,
            "worker".into(),
            None,
        );
        assert!(!(ui.ask)("Write", "Write needs permission").await);
    }
}
