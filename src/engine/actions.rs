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
    session.operations.record(
        on,
        crate::app::snapshot::ItemBody::Notice {
            code: crate::error::GENERIC.to_string(),
            level: said.level(),
            text: said.text,
        },
    );
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
