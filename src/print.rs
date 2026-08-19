//! `--print`: the third client.
//!
//! One prompt, one turn, the answer on stdout. It is the smallest thing that can
//! be called a frontend, which is exactly why it is worth building out of the
//! same parts as the other two: it attaches to [`AppCore`], submits through
//! `conversation/submit`, and prints what the core publishes. Nothing here
//! decides anything — not whether the line starts a turn, not what a permission
//! prompt may offer, not when the turn is over.
//!
//! Before B8 this path called `run_query` directly through a host of its own
//! (`query::headless_hooks`), which meant a headless run had its own idea of
//! what a submission was and its own subset of what a run reports. The subset
//! was never wrong, but it was a third answer to questions the core already
//! answers, and a third answer is how the first two come apart.
//!
//! What stays local, and belongs to: stdout carries the model's prose and
//! nothing else, everything else goes to stderr, and a prompt is asked as one
//! line of text with a letter for an answer. That is this frontend's
//! presentation, the way rows and dialogs are the console's.

use std::io::{BufRead, Write};

use crate::app::command::{AppCommand, AppQuery, Submission};
use crate::app::event::AppEventPayload;
use crate::app::ids::{ConversationId, TurnId};
use crate::app::snapshot::{
    ActivationKind, ConversationKind, Interaction, InteractionDecision, InteractionPrompt,
    TurnStatus,
};
use crate::app::{
    AppCore, AppError, AppFrame, AppLink, AppReply, AppRequest, AttachRequest, RequestId,
};

/// Why a headless run ended without an answer.
#[derive(Debug, thiserror::Error)]
pub enum PrintError {
    /// The turn reached its terminal state as a failure. The code is the core's
    /// own, computed from the error the run actually hit.
    #[error("{message}")]
    Turn { code: String, message: String },
    /// The core stopped, or refused the submission.
    #[error("{0}")]
    Core(#[from] AppError),
    /// The core answered something no reader of this method can be given.
    #[error("the session answered a submission with something else")]
    Unexpected,
}

impl PrintError {
    /// The stable code this failure reports under.
    ///
    /// The error registry maps a *type* to a code, and a turn's failure is not a
    /// type — it is a code the core computed from an error that no longer
    /// exists by the time the process is exiting. This is how that code survives
    /// the trip out to the non-TTY error contract.
    pub fn code(&self) -> &str {
        match self {
            Self::Turn { code, .. } => code,
            Self::Core(_) | Self::Unexpected => crate::error::GENERIC,
        }
    }
}

/// Run one prompt to its end.
///
/// Returns when the turn the submission opened reaches its one terminal state.
pub async fn run(core: &AppCore, prompt: &str) -> Result<(), PrintError> {
    let mut client = Client::attach(core)?;
    let main = client.main_conversation().await?;
    let turn = client.submit(&main, prompt).await?;
    client.follow(&main, turn).await
}

/// The attachment, and the counter that correlates its requests.
struct Client {
    link: AppLink,
    next: u64,
}

impl Client {
    fn attach(core: &AppCore) -> Result<Self, AppError> {
        Ok(Self {
            link: core.attach(AttachRequest::new("print"))?,
            next: 0,
        })
    }

    fn mint(&mut self) -> RequestId {
        self.next += 1;
        RequestId(self.next)
    }

    /// Take the snapshot cut, and find the conversation a prompt is for.
    ///
    /// The cut is not optional: an attachment sees no event until it has taken
    /// one, because everything before it is in the snapshot and replaying it
    /// would be saying the same thing twice.
    async fn main_conversation(&mut self) -> Result<ConversationId, PrintError> {
        let id = self.mint();
        self.link.request(AppRequest::Query {
            id,
            query: AppQuery::ReadSession,
        })?;
        let AppReply::Session(snapshot) = self.reply(id).await? else {
            return Err(PrintError::Unexpected);
        };
        snapshot
            .conversations
            .active
            .iter()
            .find(|conversation| conversation.kind == ConversationKind::Main)
            .map(|conversation| conversation.id.clone())
            .ok_or(PrintError::Unexpected)
    }

    /// Submit the prompt, and say which turn is now this run's.
    ///
    /// `SendProse` and not `Composer`: a headless argument is text handed to a
    /// program, not a line typed into a composer, so the shell prefix and the
    /// `@name`/`#room` grammar are not read out of it. The routing is the core's
    /// either way.
    async fn submit(&mut self, main: &ConversationId, prompt: &str) -> Result<TurnId, PrintError> {
        let id = self.mint();
        self.link.request(AppRequest::Command {
            id,
            command: AppCommand::Submit {
                conversation_id: main.clone(),
                input: Submission::SendProse {
                    text: prompt.to_string(),
                    attachments: Vec::new(),
                },
            },
        })?;
        match self.reply(id).await? {
            AppReply::Submitted(crate::app::command::SubmitDisposition::TurnStarted {
                turn_id,
            }) => Ok(turn_id),
            _ => Err(PrintError::Unexpected),
        }
    }

    /// Print what the run says, answer what it asks, and stop when it ends.
    async fn follow(&mut self, main: &ConversationId, turn: TurnId) -> Result<(), PrintError> {
        let mut said = false;
        while let Some(frame) = self.link.recv().await {
            let AppFrame::Event(event) = frame else {
                continue;
            };
            match &event.payload {
                // Main's prose, and only main's: a background instance streaming
                // into its own conversation is not this run's answer.
                AppEventPayload::ItemTextDelta(delta) if &delta.conversation_id == main => {
                    let mut out = std::io::stdout();
                    let _ = out.write_all(delta.delta.as_bytes());
                    let _ = out.flush();
                    said = true;
                }
                AppEventPayload::FeedbackRaised(raised) => {
                    eprintln!("[bingo] warning: {}", raised.feedback.message);
                }
                AppEventPayload::InteractionOpened(opened) => {
                    let decision = ask(&opened.interaction);
                    let id = self.mint();
                    self.link.request(AppRequest::Command {
                        id,
                        command: AppCommand::RespondInteraction {
                            interaction_id: opened.interaction.id.clone(),
                            activation: ActivationKind::Programmatic,
                            decision,
                        },
                    })?;
                }
                AppEventPayload::TurnCompleted(changed) if changed.turn.id == turn => {
                    // The answer ends with a newline — but only if there was
                    // one. A run that failed before it said anything leaves
                    // stdout empty, which is what a caller piping this reads as
                    // "no answer".
                    if said {
                        let mut out = std::io::stdout();
                        let _ = out.write_all(b"\n");
                        let _ = out.flush();
                    }
                    return match (&changed.turn.status, &changed.turn.error) {
                        (TurnStatus::Failed, Some(error)) => Err(PrintError::Turn {
                            code: error.code.clone(),
                            message: error.message.clone(),
                        }),
                        (TurnStatus::Failed, None) => Err(PrintError::Turn {
                            code: crate::error::TURN_LOST.to_string(),
                            message: "the turn ended unexpectedly".to_string(),
                        }),
                        _ => Ok(()),
                    };
                }
                _ => {}
            }
        }
        // The link ended before the turn did: the session is gone, and nothing
        // else is coming.
        Err(PrintError::Core(AppError::Stopped))
    }

    /// The answer to one request, skipping the events that overtake it.
    ///
    /// A reply is written before any event that request caused (spec invariant
    /// #3), but events from *other* work are already on the way — the frames in
    /// front of this one are dropped, because nothing has been asked for yet and
    /// there is nothing yet to print them against.
    async fn reply(&mut self, wanted: RequestId) -> Result<AppReply, PrintError> {
        while let Some(frame) = self.link.recv().await {
            if let AppFrame::Reply { id, result } = frame
                && id == wanted
            {
                return result.map_err(PrintError::Core);
            }
        }
        Err(PrintError::Core(AppError::Stopped))
    }
}

/// One prompt, asked on stderr and answered from stdin.
///
/// The wording is the headless host's, unchanged: `s` is offered only when a
/// session rule could actually be installed, because offering it otherwise
/// promises a silence the gate cannot deliver.
fn ask(interaction: &Interaction) -> InteractionDecision {
    match &interaction.prompt {
        InteractionPrompt::Permission {
            tool,
            reason,
            session_scope,
            ..
        } => {
            let keys = if session_scope.is_some() {
                "[y/s/N]"
            } else {
                "[y/N]"
            };
            eprintln!(
                "Allow {} to run? ({}) {keys} ",
                tool.name,
                reason.clone().unwrap_or_default()
            );
            match line().as_str() {
                "y" | "yes" => InteractionDecision::AllowOnce,
                "s" | "session" => match session_scope {
                    Some(scope) => InteractionDecision::AllowSession {
                        scope_id: scope.id.clone(),
                    },
                    None => InteractionDecision::Deny { feedback: None },
                },
                _ => InteractionDecision::Deny { feedback: None },
            }
        }
        InteractionPrompt::Question {
            title,
            question,
            options,
            ..
        } => {
            eprintln!("[bingo] {title}: {question}");
            for (i, option) in options.iter().enumerate() {
                match &option.description {
                    Some(described) if !described.is_empty() => {
                        eprintln!("  {}. {} ({described})", i + 1, option.label)
                    }
                    _ => eprintln!("  {}. {}", i + 1, option.label),
                }
            }
            eprintln!(
                "  {}. Other (free text)\nChoose [1-{}] or type text directly (Enter = skip): ",
                options.len() + 1,
                options.len() + 1
            );
            let answer = line();
            match answer.parse::<usize>() {
                Ok(n) if n >= 1 && n <= options.len() => InteractionDecision::Answer {
                    option_id: Some(options[n - 1].id.clone()),
                    text: None,
                },
                _ if answer.is_empty() => InteractionDecision::Cancel,
                _ => InteractionDecision::Answer {
                    option_id: None,
                    text: Some(answer),
                },
            }
        }
        // A destructive confirmation nobody typed for: refused, which is what
        // fail-closed means here.
        InteractionPrompt::Confirmation { title, detail, .. } => {
            eprintln!("[bingo] {title}: {detail} — refused (no terminal to confirm on)");
            InteractionDecision::Cancel
        }
    }
}

/// One trimmed line from stdin. A closed or unreadable stdin is an empty
/// answer, which every prompt above reads as a refusal.
fn line() -> String {
    let mut line = String::new();
    if let Err(error) = std::io::stdin().lock().read_line(&mut line) {
        eprintln!("[bingo] warning: cannot read answer from stdin: {error}");
        return String::new();
    }
    line.trim().to_ascii_lowercase()
}
