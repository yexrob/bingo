//! The work behind the actions the core cannot perform itself.
//!
//! Thirteen of the twenty-eight actions in [`crate::app::action`] need a model,
//! a transcript rewrite or a network round trip. The ledger half of each is the
//! actor's — it decides the action may run, opens the operation, and records
//! what came back — and the half that leaves the process is here, for the same
//! reason a turn's is (B4 ruling ②).
//!
//! Every function answers in the words its own surface prints. That is what
//! makes this one implementation rather than two: the terminal renders the
//! answer with its slash tiers, the core records it as an item, and neither owns
//! the sentence.

use std::sync::Arc;

use crate::app::conversation::ConvKey;
use crate::query::Session;

/// How loudly one line of a report is said.
///
/// The three tiers the terminal already had, named where the work is rather than
/// where it is drawn: a result, a longer notice worth keeping on screen, and a
/// failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Output,
    Info,
    Error,
}

/// One thing an action's work has to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Said {
    pub tier: Tier,
    pub text: String,
}

impl Said {
    pub fn output(text: impl Into<String>) -> Self {
        Self {
            tier: Tier::Output,
            text: text.into(),
        }
    }

    pub fn info(text: impl Into<String>) -> Self {
        Self {
            tier: Tier::Info,
            text: text.into(),
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            tier: Tier::Error,
            text: text.into(),
        }
    }

    /// How a client that reads codes rather than prose classifies it.
    pub fn level(&self) -> crate::app::snapshot::NoticeLevel {
        match self.tier {
            Tier::Error => crate::app::snapshot::NoticeLevel::Error,
            _ => crate::app::snapshot::NoticeLevel::Info,
        }
    }
}

/// Do the work of one action the core handed over, and report it back.
///
/// The report is two things, because an operation and an item answer different
/// questions: the operation says the work is running and how it ended, and the
/// item is what it did — which is what a client reads a conversation for.
pub async fn perform(session: &Arc<Session>, act: crate::app::engine::Act) {
    use crate::app::command::Action;
    let crate::app::engine::Act {
        operation,
        on,
        action,
    } = act;
    let said = match action {
        Action::ConversationCompact { .. } => {
            let planned = plan_compaction(session, &on);
            match planned {
                Err(said) => said,
                Ok(plan) => {
                    if let Some(operation) = &operation {
                        session
                            .operations
                            .progress(operation, plan.waiting.clone(), 0, 1);
                    }
                    let done = compact(session.clone(), plan).await;
                    if let Some(outcome) = done.outcome {
                        session.operations.record(
                            on.clone(),
                            crate::app::snapshot::ItemBody::Compaction {
                                before_tokens: outcome.before,
                                after_tokens: outcome.after,
                                replaced_messages: outcome.replaced,
                                duration_ms: outcome.duration.as_millis().min(u128::from(u64::MAX))
                                    as u64,
                            },
                        );
                    }
                    done.said
                }
            }
        }
        Action::SessionReset => {
            let done = reset_session(session);
            warn(session, &on, done.warnings);
            done.said
        }
        Action::SessionRename { name } => {
            let done = rename_session(session, &name);
            warn(session, &on, done.warnings);
            done.said
        }
        Action::SessionShare {
            public,
            open,
            output,
        } => match prepare_share(session, output) {
            Err(said) => said,
            Ok(export) => {
                for note in &export.notes {
                    session.operations.record(on.clone(), notice(note));
                }
                if public {
                    publish_share(export, open).await
                } else {
                    export_share(&export, open)
                }
            }
        },
        // Always the browser flow from here: it is the one that needs no input
        // the protocol cannot carry, and its authorization URL is published as
        // progress so a client with no browser can still follow it.
        Action::ProviderLogin { provider } => {
            match plan_login(session, &provider, Flow::Loopback) {
                Err(said) => said,
                Ok(planned) => {
                    let waiting = progress_sink(session, operation.clone());
                    login_provider(session, planned, waiting).await
                }
            }
        }
        Action::ConversationRewind { target, mode } => {
            let done = rewind_conversation(session, &target, mode);
            if done.removed > 0 {
                session.operations.record(
                    on.clone(),
                    crate::app::snapshot::ItemBody::Rewind {
                        mode,
                        removed_items: done.removed,
                        target_item_id: match &target {
                            crate::app::command::RewindTarget::Item { item_id } => {
                                Some(item_id.clone())
                            }
                            crate::app::command::RewindTarget::Latest => None,
                        },
                    },
                );
            }
            done.said
        }
        Action::McpReconnect { server } => reconnect_mcp(session, server.as_deref()).await,
        crew @ (Action::TeamStart { .. }
        | Action::TeamAssign { .. }
        | Action::TeamStop { .. }
        | Action::TeamScaffold { .. }
        | Action::TeamMemoryGarbageCollect) => team(session, &crew),
        // The table said this action needs an engine and the core handed it
        // over; an arm missing here is a dispatch bug, and saying so beats
        // finishing an operation that never ran.
        other => Said::error(format!("{} is not implemented by this engine", other.id())),
    };
    // The report before the terminal state, for the reason the empty-response
    // warning goes out before its turn closes (D152): a client that reads the
    // end and stops looking must already have been told what happened.
    let failed = said.tier == Tier::Error;
    let message = said.text.clone();
    session.operations.record(on, notice(&said));
    if let Some(operation) = &operation {
        session.operations.finish(
            operation,
            if failed {
                crate::app::snapshot::OperationStatus::Failed
            } else {
                crate::app::snapshot::OperationStatus::Completed
            },
            failed.then(|| crate::app::snapshot::TurnError {
                code: crate::error::GENERIC.to_string(),
                message: crate::error::sanitize_msg(&message),
            }),
        );
    }
}

/// The waiting lines of a piece of work, as the operation's own progress.
fn progress_sink(
    session: &Arc<Session>,
    operation: Option<crate::app::ids::OperationId>,
) -> Waiting {
    let operations = session.operations.clone();
    Arc::new(move |lines: Vec<String>| {
        if let Some(operation) = &operation {
            operations.progress(operation, lines.join("\n"), 0, 1);
        }
    })
}

/// One line of a report, as an item in a conversation's log.
fn notice(said: &Said) -> crate::app::snapshot::ItemBody {
    crate::app::snapshot::ItemBody::Notice {
        code: crate::error::GENERIC.to_string(),
        level: said.level(),
        text: said.text.clone(),
    }
}

/// What the work could not carry over, said beside the result rather than
/// instead of it.
fn warn(session: &Arc<Session>, on: &ConvKey, warnings: Vec<String>) {
    for warning in warnings {
        session.operations.record(
            on.clone(),
            crate::app::snapshot::ItemBody::Notice {
                code: crate::error::GENERIC.to_string(),
                level: crate::app::snapshot::NoticeLevel::Warning,
                text: warning,
            },
        );
    }
}

// ---------------------------------------------------------------------------
// conversation.compact
// ---------------------------------------------------------------------------

/// Whose context a compaction rewrites.
///
/// `/compact` is the one command that follows the page instead of the console
/// (D135): it rewrites a context, and rewriting the wrong one destroys work that
/// cannot be got back.
enum Target {
    /// The console's own history, which lives in the transcript.
    Console,
    /// An instance's, which lives in the registry: the history is read before
    /// anything is announced, and put back only if the instance never moved.
    Instance {
        name: String,
        session: Arc<Session>,
        history: Vec<crate::api::types::Message>,
    },
}

/// A compaction that is going to be attempted.
pub struct Compaction {
    target: Target,
    /// The line a surface shows while it runs.
    pub waiting: String,
    /// The instance this is about, for a surface that keys its progress by it.
    pub instance: Option<String>,
}

/// What a compaction produced.
pub struct Compacted {
    pub said: Said,
    /// The rewritten context's new occupancy, for the page it belongs to.
    pub usage: Option<crate::context_usage::ContextUsage>,
    /// The rewrite's own numbers, when there was one.
    pub outcome: Option<crate::compact::CompactOutcome>,
}

/// The checks made before a surface says it is working.
///
/// `Err` is an answer rather than a failure: a room has no context behind it and
/// an instance mid-turn owns the history the rewrite would overwrite, and both
/// are said in one line without anything being started.
pub fn plan_compaction(session: &Arc<Session>, on: &ConvKey) -> Result<Compaction, Said> {
    match on {
        ConvKey::Main => Ok(Compaction {
            target: Target::Console,
            waiting: "⏳ compacting the context…".to_string(),
            instance: None,
        }),
        // A room is a log, not a turn loop: there is no context behind it to
        // summarise, and quietly compacting the console's instead would be the
        // exact wrong-target loss this ruling exists to prevent.
        ConvKey::Room(room) => Err(Said::info(format!(
            "#{room} is a log, not a context: nothing to compact"
        ))),
        ConvKey::Agent(name) => plan_instance(session, name),
    }
}

fn plan_instance(session: &Arc<Session>, name: &str) -> Result<Compaction, Said> {
    let Some((history, _, state)) = session.agents.view_of(name) else {
        return Err(Said::error(format!(
            "[error] code={} msg=no instance named {name}",
            crate::error::SLASH_ERROR_BAD_ARGUMENT
        )));
    };
    if state == crate::agents::AgentState::Running {
        return Err(Said::error(format!(
            "[error] code={} msg=@{name} is mid-turn and owns the history a compaction would rewrite (esc to stop it, then retry)",
            crate::error::SLASH_ERROR_BAD_ARGUMENT
        )));
    }
    // The same floor the loop's own gate applies, reported as what it is.
    if history.len() <= crate::compact::KEEP_RECENT {
        return Err(Said::output(format!(
            "@{name}'s context is too short; no compaction needed."
        )));
    }
    let Some(instance) = session.agents.session_of(name) else {
        // Deleted between the two reads. Every other exit here speaks, and a
        // keystroke that silently did nothing is what the queue-info line was
        // added for.
        return Err(Said::error(format!("no instance named {name}")));
    };
    Ok(Compaction {
        waiting: format!("⏳ compacting @{name}'s context…"),
        instance: Some(name.to_string()),
        target: Target::Instance {
            name: name.to_string(),
            session: instance,
            history,
        },
    })
}

/// Summarise the older half of a context and put the result back.
pub async fn compact(session: Arc<Session>, plan: Compaction) -> Compacted {
    match plan.target {
        Target::Console => compact_console(&session).await,
        Target::Instance {
            name,
            session,
            history,
        } => compact_instance(name, session, history).await,
    }
}

fn nothing(said: Said) -> Compacted {
    Compacted {
        said,
        usage: None,
        outcome: None,
    }
}

async fn compact_console(session: &Arc<Session>) -> Compacted {
    let transcript = session.runtime.transcript.borrow().clone();
    let mut messages = match &transcript {
        Some(transcript) => transcript.load_messages().unwrap_or_default(),
        None => Vec::new(),
    };
    // The same floor `maybe_compact` applies, so "too short" is reported as that
    // and not as a model-call failure.
    if messages.len() <= crate::compact::KEEP_RECENT {
        return nothing(Said::output(
            "the conversation is too short; no compaction needed.",
        ));
    }
    let old_len = messages.len();
    // The command reports its own outcome below, so the shared notification
    // channel stays quiet here.
    let Some(outcome) = crate::compact::compact_now(session, &mut messages, &mut |_| {}).await
    else {
        return nothing(Said::error("compaction failed (model call error)."));
    };
    let summary = messages
        .first()
        .map(|message| {
            message
                .content
                .iter()
                .filter_map(|block| match block {
                    crate::api::types::ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let kept = messages.len().saturating_sub(1);
    Compacted {
        // Persistence happened inside the rewrite as an appended marker; the
        // canonical lines are untouched (D74).
        said: Said::info(format!(
            "✓ compacted {old_len} messages → summary + the latest {kept}.\nSummary: {summary}"
        )),
        usage: Some(usage_of(session, &messages)),
        outcome: Some(outcome),
    }
}

async fn compact_instance(
    name: String,
    session: Arc<Session>,
    mut messages: Vec<crate::api::types::Message>,
) -> Compacted {
    let old_len = messages.len();
    let Some(outcome) = crate::compact::compact_now(&session, &mut messages, &mut |_| {}).await
    else {
        return nothing(Said::error("compaction failed (model call error)."));
    };
    let kept = messages.len().saturating_sub(1);
    let usage = usage_of(&session, &messages);
    if !session.agents.replace_history(&name, messages).await {
        return nothing(Said::error(format!(
            "@{name} started a turn while it was being compacted; its context is unchanged."
        )));
    }
    Compacted {
        said: Said::info(format!(
            "✓ compacted @{name}: {old_len} messages → summary + the latest {kept}."
        )),
        usage: Some(usage),
        outcome: Some(outcome),
    }
}

fn usage_of(
    session: &Arc<Session>,
    messages: &[crate::api::types::Message],
) -> crate::context_usage::ContextUsage {
    crate::context_usage::ContextUsage::for_model(
        crate::compact::estimate_tokens(&session.system, messages, &[]),
        &session.client.models(),
        &session.runtime.model.borrow().clone(),
    )
}

// ---------------------------------------------------------------------------
// mcp.reconnect
// ---------------------------------------------------------------------------

/// Reconnect one configured MCP server, or every one of them.
///
/// The manager lives outside the actor, so what it stands at afterwards is
/// *reported in* rather than read out — a reconnect that left `config/read`
/// saying "not connected" would be a lie a client has no way to catch.
pub async fn reconnect_mcp(session: &Arc<Session>, server: Option<&str>) -> Said {
    let mut manager = session.runtime.mcp.lock().await;
    let configured = manager.configured();
    let targets: Vec<String> = match server {
        Some(name) => {
            if !configured.iter().any(|known| known == name) {
                return Said::error(format!("no MCP server \"{name}\"."));
            }
            if manager.is_disabled(name) {
                return Said::error(format!(
                    "{name} is disabled; run /mcp enable {name} before reconnecting."
                ));
            }
            vec![name.to_string()]
        }
        None => configured
            .iter()
            .filter(|name| !manager.is_disabled(name))
            .cloned()
            .collect(),
    };
    if targets.is_empty() {
        return Said::error("no MCP server is configured and enabled.".to_string());
    }
    let mut failures = Vec::new();
    let mut tools = 0usize;
    for name in &targets {
        match manager.reconnect(name).await {
            Ok(()) => {
                if let crate::mcp::McpStatus::Connected { tool_count } = manager.status(name) {
                    tools += tool_count;
                }
            }
            Err(error) => failures.push(format!("{error}")),
        }
    }
    session.core.report_mcp(mcp_states(&manager));
    if let Some(first) = failures.first() {
        return Said::error(format!("✗ {first}"));
    }
    Said::output(match targets.as_slice() {
        [one] => format!("✓ {one} reconnected · {tools} tools"),
        many => format!("✓ {} MCP servers reconnected · {tools} tools", many.len()),
    })
}

/// What the manager stands at, in the shape the core publishes.
fn mcp_states(manager: &crate::mcp::McpManager) -> Vec<crate::app::snapshot::McpServerState> {
    manager
        .configured()
        .into_iter()
        .map(|name| {
            let status = manager.status(&name);
            let (wire, tools, error) = match &status {
                crate::mcp::McpStatus::Connected { tool_count } => (
                    crate::app::snapshot::McpStatus::Connected,
                    *tool_count as u32,
                    None,
                ),
                crate::mcp::McpStatus::Failed { detail } => (
                    crate::app::snapshot::McpStatus::Error,
                    0,
                    Some(crate::error::sanitize_msg(detail)),
                ),
                // Disabled and never-connected are one wire state, because the
                // wire says separately whether a server is enabled: two ways to
                // read one fact is how they come to disagree.
                crate::mcp::McpStatus::Disabled | crate::mcp::McpStatus::NotConnected => {
                    (crate::app::snapshot::McpStatus::Disconnected, 0, None)
                }
            };
            crate::app::snapshot::McpServerState {
                enabled: !manager.is_disabled(&name),
                name,
                status: wire,
                tools,
                error,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// team.*
// ---------------------------------------------------------------------------

/// The five `/team` mutations. The chart, the crew and the memory all live in
/// the project directory, which is why this is the engine's half rather than the
/// actor's: it reads files and starts loops.
pub fn team(session: &Arc<Session>, action: &crate::app::command::Action) -> Said {
    let lines = crate::team_cmd::act(session, &session.cwd(), action);
    Said::info(lines.join("\n"))
}

// ---------------------------------------------------------------------------
// The session family: reset, rename, share
// ---------------------------------------------------------------------------

/// Bind the session's task store to a transcript.
///
/// A session with no transcript files its tasks under the project instead, so
/// there is always a key and never a hole.
pub fn bind_tasks(session: &Arc<Session>, transcript: Option<&crate::transcript::Transcript>) {
    let key = transcript
        .map(crate::transcript::Transcript::name)
        .filter(|key| !key.is_empty())
        .unwrap_or_else(|| crate::tasks::project_task_key(&session.cwd()));
    session.tasks.rebind(&key);
}

/// Bind the two files a session keeps beside its transcript: the share document
/// and the room log a resume replays (Amendment #6).
///
/// Neither failing is worth refusing over — both are enhancements to a session
/// that is otherwise complete — so a failure comes back as a warning beside the
/// result rather than instead of it.
pub fn bind_share(
    session: &Arc<Session>,
    transcript: Option<&crate::transcript::Transcript>,
) -> Vec<String> {
    session.agents.detach_share();
    session.channels.detach_share();
    let Some(transcript) = transcript else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    let path = crate::share::shares_dir(&session.home).join(format!("{}.json", transcript.name()));
    match crate::share::ShareStore::load_or_create(&path) {
        Ok(store) => {
            session.channels.align_with_share(store.clone());
            session.agents.attach_share(store.clone());
            session.channels.attach_share(store);
        }
        Err(error) => warnings.push(format!(
            "share store unavailable ({error}); bingo share will have the conversation view only"
        )),
    }
    // Replay first, so a resumed session comes back to the rooms it left with
    // its unread marks intact, and only then start appending to the log it just
    // read.
    let rooms = crate::app::roomlog::path(&session.home, &transcript.name());
    session
        .channels
        .restore_rooms(crate::app::roomlog::replay(&rooms));
    session.channels.attach_sidecar(rooms);
    warnings
}

/// What a session-level rewrite left behind: the line to print, and what could
/// not be carried over.
///
/// The transcript itself is not here because it is not returned anywhere: it is
/// *published* on `runtime.transcript`, which is where every reader of "which
/// transcript is this session on" already looks.
pub struct Rebound {
    pub said: Said,
    pub warnings: Vec<String>,
}

/// `session.reset`: a fresh transcript, with everything the old one carried
/// rebound to it.
pub fn reset_session(session: &Arc<Session>) -> Rebound {
    let transcript = crate::transcript::create(&session.home, &session.cwd()).ok();
    let _ = session.runtime.transcript_tx.send(transcript.clone());
    bind_tasks(session, transcript.as_ref());
    let warnings = bind_share(session, transcript.as_ref());
    Rebound {
        said: Said::output("✓ conversation cleared; starting a new session."),
        warnings,
    }
}

/// `session.rename`: the transcript file, and the two sidecars that answer to
/// its name.
pub fn rename_session(session: &Arc<Session>, name: &str) -> Rebound {
    let Some(current) = session.runtime.transcript.borrow().clone() else {
        return Rebound {
            said: Said::error("this session has no transcript; cannot rename."),
            warnings: Vec::new(),
        };
    };
    let previous = current.name();
    let renamed = match current.rename(name) {
        Ok(renamed) => renamed,
        Err(error) => {
            return Rebound {
                said: Said::error(format!("rename failed: {error}")),
                warnings: Vec::new(),
            };
        }
    };
    let name = renamed.name();
    let mut warnings = Vec::new();
    if let Err(error) = session.tasks.rename_key(&previous, &name) {
        warnings.push(format!(
            "task data could not follow the renamed session ({error}); tasks remain under the previous session name"
        ));
    }
    if let Err(error) = crate::share::rename_session_sidecars(&session.home, &previous, &name) {
        warnings.push(format!(
            "share data could not follow the renamed session ({error}); export may omit agent/channel history"
        ));
    }
    let _ = session.runtime.transcript_tx.send(Some(renamed.clone()));
    warnings.extend(bind_share(session, Some(&renamed)));
    Rebound {
        said: Said::output(format!("✓ session renamed: {name}")),
        warnings,
    }
}

/// A rendered export, and where it is going.
pub struct Export {
    pub html: String,
    pub out: std::path::PathBuf,
    /// What the render had to do without, said before the export itself.
    pub notes: Vec<Said>,
    base: String,
    id: String,
}

/// Render the session as one page. The human-facing export is the full canonical
/// conversation, not the compacted projection the model sees.
pub fn prepare_share(
    session: &Arc<Session>,
    output: Option<std::path::PathBuf>,
) -> Result<Export, Said> {
    let Some(transcript) = session.runtime.transcript.borrow().clone() else {
        return Err(Said::output(
            "no session to export yet (the new session has not been persisted; send a message first).",
        ));
    };
    let messages = match transcript.load_canonical() {
        Ok(messages) => messages,
        Err(error) => return Err(Said::error(format!("failed to read the session: {error}"))),
    };
    let stem = transcript.name();
    let path = crate::share::shares_dir(&session.home).join(format!("{stem}.json"));
    let mut notes = Vec::new();
    let doc = match crate::share::ShareStore::load_or_create(&path) {
        Ok(store) => store.snapshot(),
        Err(error) => {
            // The export still happens; what it loses is the collaboration half.
            notes.push(Said::error(format!(
                "cannot read the share document ({error}); exporting the conversation view only."
            )));
            crate::share::ShareDoc::new(stem.clone())
        }
    };
    // Legacy-session fallback: without a share document, derive the team, DM and
    // channel data from the main transcript.
    let doc = if doc.agents.is_empty() && doc.channels.is_empty() {
        crate::share::derive_share_doc(&stem, &messages)
    } else {
        doc
    };
    Ok(Export {
        html: crate::share_html::render(&doc, &messages),
        out: output.unwrap_or_else(|| session.cwd().join(format!("{stem}.html"))),
        notes,
        base: session
            .settings
            .share
            .base_url
            .clone()
            .unwrap_or_else(|| crate::share::DEFAULT_SHARE_BASE.to_string()),
        id: crate::share::share_id(&stem),
    })
}

/// The note every export carries: what is in the file, before anyone sends it on.
const SHARE_NOTE: &str = "note: this file contains the full conversation and tool outputs (possibly sensitive); review it before sharing.";

/// Local export, which is the safe default. `open` only opens the file it wrote.
pub fn export_share(export: &Export, open: bool) -> Said {
    let overwritten = export.out.exists();
    if let Err(error) = crate::share::write_html_atomic(&export.out, &export.html) {
        return Said::error(format!("write failed: {error}"));
    }
    let mut lines = vec![format!(
        "✓ exported: {}{}",
        export.out.display(),
        if overwritten { " (overwritten)" } else { "" }
    )];
    if open {
        match crate::share::open_in_browser(&export.out.display().to_string()) {
            Ok(_) => lines.push("opened in the browser.".to_string()),
            Err(error) => lines.push(format!("cannot open the browser: {error}")),
        }
    }
    lines.push(SHARE_NOTE.to_string());
    Said::info(lines.join("\n"))
}

/// Publish a public link. Explicit opt-in, and a failed upload falls back to the
/// local file rather than losing the export.
pub async fn publish_share(export: Export, open: bool) -> Said {
    match crate::share::upload_share(&export.base, &export.id, &export.html).await {
        Ok(url) => {
            let mut lines = vec![format!("✓ published: {url}")];
            if open {
                match crate::share::open_in_browser(&url) {
                    Ok(_) => lines.push("opened in the browser.".to_string()),
                    Err(error) => lines.push(format!("cannot open the browser: {error}")),
                }
            }
            // The URL must survive long enough to copy — info tier.
            Said::info(lines.join("\n"))
        }
        Err(error) => {
            let mut lines = vec![format!(
                "upload failed ({error}); falling back to a local file."
            )];
            let overwritten = export.out.exists();
            match crate::share::write_html_atomic(&export.out, &export.html) {
                Ok(()) => lines.push(format!(
                    "✓ exported: {}{}",
                    export.out.display(),
                    if overwritten { " (overwritten)" } else { "" }
                )),
                Err(write) => lines.push(format!("write failed: {write}")),
            }
            if open && crate::share::open_in_browser(&export.out.display().to_string()).is_ok() {
                lines.push("opened in the browser.".to_string());
            }
            lines.push(SHARE_NOTE.to_string());
            Said::error(lines.join("\n"))
        }
    }
}

// ---------------------------------------------------------------------------
// provider.login
// ---------------------------------------------------------------------------

/// Which way a login is being done.
///
/// **This never crosses a wire, and that is the point** (B5 ruling ③). The
/// action a client sends names a provider and nothing else: `--device-auth` is a
/// login mechanic, and `--manual <token>` carries a *credential*, which has no
/// business in a request body, an event, a snapshot or an operation's progress.
///
/// So the manual token has one route into this process and it is not the
/// protocol: the terminal reads it from its own input surface and passes it here
/// as a function argument, on the thread that read it. Nothing serialises it,
/// so there is no frame it could leak into. A remote client cannot paste a token
/// over this protocol at all — it authenticates by device flow, whose secrets
/// are one-time and public by design, or it writes the credential file itself.
pub enum Flow {
    /// The default: a local callback and a browser.
    Loopback,
    /// Headless: a URL and a one-time code, polled until authorised.
    DeviceAuth,
    /// A token the user already holds. In-process only.
    Manual(String),
}

/// Where a login's waiting lines go while it waits.
///
/// The lines are the work's, because they carry the URL and the code the user
/// has to act on: the terminal pins them, and the core publishes them as the
/// operation's progress. A one-time device code is meant to be read aloud, so it
/// is not a credential and this is not a leak.
pub type Waiting = Arc<dyn Fn(Vec<String>) + Send + Sync>;

/// A login that is going to be attempted.
pub struct Login {
    provider: String,
    flow: Flow,
    /// The provider takes a subscription key rather than an OAuth token.
    api_preset: bool,
}

/// What is settled before anything is attempted: whether the provider exists,
/// which kind of credential it takes, and whether this build can do that flow.
///
/// Separate from the attempt because these three answers are immediate, and a
/// surface that has to wait for a round trip to be told "no such provider" is
/// answering a different question from the one that was asked.
pub fn plan_login(session: &Arc<Session>, provider: &str, flow: Flow) -> Result<Login, Said> {
    let preset = crate::api::providers::presets::preset(provider);
    // Effective config = user settings ⊕ built-in preset (D34 §6.5): presets make
    // official subscriptions loginable with zero config.
    if !session.settings.providers.contains_key(provider) && preset.is_none() {
        return Err(Said::error(format!(
            "provider \"{provider}\" not found (see /provider for the list)"
        )));
    }
    let api_preset = preset
        .map(|preset| preset.oauth_kind.is_none())
        .unwrap_or(false);
    let planned = Login {
        provider: provider.to_string(),
        flow,
        api_preset,
    };
    // A pasted credential answers for both kinds, so it never reaches the gate.
    if matches!(planned.flow, Flow::Manual(_)) {
        return Ok(planned);
    }
    let oauth_kind = session
        .settings
        .providers
        .get(provider)
        .and_then(|config| config.oauth.as_ref().map(|oauth| oauth.kind.clone()))
        .or_else(|| preset.and_then(|preset| preset.oauth_kind.map(str::to_string)));
    // OAuth gate: codex only in v1; apiKey presets guide the key paste.
    let Some(oauth_kind) = oauth_kind else {
        return Err(Said::info(format!(
            "provider \"{provider}\" requires an API key (subscription key):\n  1. get one at opencode.ai/auth\n  2. /provider login {provider} --manual <key>"
        )));
    };
    if oauth_kind != "codex" {
        return Err(Said::error(format!(
            "unsupported oauth.kind \"{oauth_kind}\" (v1 supports only codex)"
        )));
    }
    Ok(planned)
}

/// Authenticate with a provider.
pub async fn login_provider(session: &Arc<Session>, login: Login, waiting: Waiting) -> Said {
    let Login {
        provider,
        flow,
        api_preset,
    } = login;
    let provider = provider.as_str();
    let home = session.home.clone();
    let config = crate::api::auth::OauthFlowConfig::codex();
    // Share the session client's TokenProvider: saving through the same instance
    // updates the adapter's cache and account mirror, so the login takes effect
    // in this session without a restart.
    let shared = session.client.token_provider(provider);
    let token_provider = |config: crate::api::auth::OauthFlowConfig| {
        shared.unwrap_or_else(|| {
            Arc::new(crate::api::auth::TokenProvider::new(
                &home, provider, config,
            ))
        })
    };

    if let Flow::Manual(token) = flow {
        // A pasted key works for an apiKey preset as well as an oauth one.
        if api_preset {
            let store = crate::auth::AuthStore::new(&home);
            return match store.set(provider, crate::auth::AuthEntry::Api { key: token }) {
                Ok(()) => Said::output(format!("✓ saved {provider}'s API key (subscription key)")),
                Err(error) => Said::error(format!("✗ save failed: {error}")),
            };
        }
        let tokens = crate::api::auth::TokenSet {
            access_token: token,
            refresh_token: String::new(),
            id_token: None,
            expires_at: None,
            account_id: None,
        };
        return match token_provider(config).save(&tokens).await {
            Ok(()) => Said::output(format!(
                "✓ saved {provider}'s login info (a --manual token does not auto-refresh)"
            )),
            Err(error) => Said::error(format!("✗ save failed: {error}")),
        };
    }

    let http = reqwest::Client::new();
    let tokens = match flow {
        Flow::DeviceAuth => {
            let device = crate::api::auth::DeviceFlow::new(&http, &config);
            let (prompt, id, interval) = match device.start().await {
                Ok(started) => started,
                Err(error) => return Said::error(format!("✗ sign-in failed: {error}")),
            };
            // The code is valid for fifteen minutes and has to stay readable for
            // all of them.
            waiting(vec![
                format!("sign in to {provider} (device authorization)"),
                format!("  1. open {}", prompt.verification_url),
                format!("  2. enter code {} (valid for 15 minutes)", prompt.user_code),
                "⏳ waiting for authorization… (Esc will not cancel; the panel disappears when done)"
                    .to_string(),
            ]);
            match device.poll(&id, &prompt.user_code, interval).await {
                Ok(tokens) => tokens,
                Err(error) => return Said::output(format!("✗ sign-in failed: {error}")),
            }
        }
        _ => {
            let loopback = crate::api::auth::LoopbackPkce::new(&http, &config);
            let (url, _redirect, _verifier, handle) = match loopback.authorize_url().await {
                Ok(started) => started,
                Err(error) => return Said::error(format!("✗ sign-in failed: {error}")),
            };
            // With the URL itself: on an SSH or headless host the browser never
            // opens and this line is the only way through.
            waiting(vec![
                format!(
                    "sign in to {provider}: complete the authorization in the browser (tried to open it automatically)"
                ),
                format!("  {url}"),
                format!("  browser did not open? /provider login {provider} --device-auth"),
            ]);
            let _ = crate::share::open_in_browser(&url);
            match handle.await {
                Ok(Ok(tokens)) => tokens,
                Ok(Err(error)) => return Said::error(format!("✗ sign-in failed: {error}")),
                Err(error) => return Said::error(format!("✗ sign-in interrupted: {error}")),
            }
        }
    };
    match token_provider(config).save(&tokens).await {
        Ok(()) => Said::output(format!("✓ signed in to {provider}")),
        Err(error) => Said::output(format!("✗ save failed: {error}")),
    }
}

// ---------------------------------------------------------------------------
// conversation.rewind
// ---------------------------------------------------------------------------

/// How many restored files a report names before it counts the rest.
const RESTORE_LIST_MAX: usize = 8;

/// What a rewind did, or would do.
pub struct Rewound {
    pub said: Said,
    /// Projected history entries the cut removes. Zero for a preview, which
    /// removes nothing.
    pub removed: u32,
}

/// Go back to an earlier checkpoint, or say what going back would cost.
///
/// `apply` restores the files *and* the conversation, because that is what "go
/// back to it" means: a conversation restored over files that failed to come
/// back would describe a state the disk is not in, which is why the files move
/// first and a failure there stops everything.
pub fn rewind_conversation(
    session: &Arc<Session>,
    target: &crate::app::command::RewindTarget,
    mode: crate::app::snapshot::RewindMode,
) -> Rewound {
    use crate::app::command::RewindTarget;
    use crate::app::snapshot::RewindMode;
    let nothing = |said: Said| Rewound { said, removed: 0 };
    let Some(transcript) = session.runtime.transcript.borrow().clone() else {
        return nothing(Said::info(
            "this session has no transcript; nothing to rewind",
        ));
    };
    let dir = crate::rewind::session_dir(&session.home, &transcript.name());
    let entries = transcript.load_projection().unwrap_or_default();
    let points = crate::rewind::checkpoints_of(&entries, &dir, crate::rewind::REWIND_MAX);
    let point = match target {
        RewindTarget::Latest => points.first(),
        // A checkpoint's identity is the transcript line its message was written
        // on, and an item id is minted by this session with no record of which
        // line became it. Guessing the correspondence is exactly the wrong-target
        // loss D135 exists to prevent, so this refuses instead. B8's parity
        // ledger owns the gap.
        RewindTarget::Item { .. } => {
            return nothing(Said::error(
                "rewinding to a named item is not supported yet; rewind to the latest checkpoint",
            ));
        }
    };
    let Some(point) = point else {
        return nothing(Said::info("no turns to rewind to yet"));
    };
    let removed = entries.len().saturating_sub(point.index) as u32;
    if mode == RewindMode::Preview {
        return nothing(Said::info(format!(
            "⏪ would restore to {}: {} message{} and {} file{} would go back",
            point.label,
            removed,
            if removed == 1 { "" } else { "s" },
            point.coverage.files,
            if point.coverage.files == 1 { "" } else { "s" }
        )));
    }
    let mut lines = Vec::new();
    match crate::rewind::restore(&dir, point.line) {
        Ok(restored) => {
            lines.push(format!(
                "⏪ restored {} file{}",
                restored.len(),
                if restored.len() == 1 { "" } else { "s" }
            ));
            lines.extend(restored.iter().take(RESTORE_LIST_MAX).map(|file| {
                format!(
                    "   {} {}",
                    if file.removed { "removed" } else { "reverted" },
                    file.path.display()
                )
            }));
            if restored.len() > RESTORE_LIST_MAX {
                lines.push(format!("   … {} more", restored.len() - RESTORE_LIST_MAX));
            }
        }
        Err(error) => {
            return nothing(Said::error(format!(
                "[error] rewind could not restore files: {error}"
            )));
        }
    }
    if let Err(error) = transcript.truncate_at_line(point.line) {
        return nothing(Said::error(format!(
            "[error] rewind could not rewrite the session: {error}"
        )));
    }
    session.queue.clear(crate::app::conversation::ConvKey::Main);
    // The turns those snapshots belong to are gone from the conversation, so the
    // pre-images they hold address nothing any more.
    crate::rewind::drop_from(&dir, point.line);
    lines.push(format!(
        "⏪ code and conversation restored to {}",
        point.label
    ));
    Rewound {
        said: Said::output(lines.join("\n")),
        removed,
    }
}

#[cfg(test)]
mod tests {
    /// The red line, asserted rather than trusted: a pasted credential is not
    /// part of the action a login line becomes, so it cannot reach a wire frame,
    /// an event, a snapshot or an operation's payload.
    #[test]
    fn a_pasted_token_is_never_part_of_the_action_a_login_line_becomes() {
        let parsed =
            crate::app::action::parse_in("provider login opencode-go --manual sk-secret", &[])
                .unwrap_or_else(|error| panic!("{error}"));
        let crate::app::action::Command::Act(action) = parsed else {
            panic!("a login line is an action");
        };
        let wire = serde_json::to_string(&action).unwrap_or_else(|error| panic!("{error}"));
        assert!(
            !wire.contains("sk-secret"),
            "the token stays on the line the terminal read: {wire}"
        );
    }
}
