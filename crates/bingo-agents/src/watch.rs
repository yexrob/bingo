//! Following a child: its own frames, folded with the one reducer, until a
//! turn ends. Nothing here keeps a view of its own — a child's reply is
//! derived from its journal at the moment it is asked for (ADR-0002).

use std::sync::Arc;

use bingo_sdk::{
    Attachment, CancellationToken, ClientIdentity, Delivery, Event, HostHandle, Input, IntentId,
    InterruptReason, ItemBody, KernelError, OpenOptions, SessionId, SessionSelector, SessionState,
    ToolError, ToolHost, ToolOutput, TurnId, TurnStatus,
};
use futures::StreamExt;

use crate::message;

/// How this plugin identifies itself when it attaches to a child.
fn identity() -> ClientIdentity {
    ClientIdentity {
        name: "agents".into(),
        surface: message::SURFACE.into(),
    }
}

/// An attachment to a child. Opened before anything is delivered to it, so no
/// frame of the turn that follows can be missed.
pub async fn follow(host: &HostHandle, child: &SessionId) -> Result<Attachment, KernelError> {
    host.open(
        SessionSelector::ById { id: child.clone() },
        identity(),
        OpenOptions::default(),
    )
    .await
}

/// What a child's turn came to: how it ended, and the assistant text it
/// wrote on the way. A turn that failed or was cut short is not an answer,
/// whatever it managed to say first.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub status: TurnStatus,
    pub text: String,
}

impl Reply {
    /// Whether the caller reads this as a failure of the call it made.
    pub fn is_error(&self) -> bool {
        !matches!(self.status, TurnStatus::Completed)
    }
}

/// The child's reply to the turn it is running, once that turn ends.
/// Cancelling the waiting call stops the wait, never the child.
pub async fn next_reply(
    attachment: &mut Attachment,
    cancel: &CancellationToken,
) -> Result<Reply, ToolError> {
    loop {
        let frame = tokio::select! {
            () = cancel.cancelled() => return Err(ToolError::Cancelled),
            frame = attachment.events.next() => frame,
        };
        let Some(frame) = frame else {
            return Err(ToolError::Failed("the agent's session ended".into()));
        };
        attachment.snapshot.apply(&frame);
        if let Event::TurnCompleted { turn, status, .. } = &frame.event {
            return Ok(Reply {
                status: status.clone(),
                text: reply_to(&attachment.snapshot, turn),
            });
        }
    }
}

/// The text a turn produced: its assistant items, in the order it wrote them.
fn reply_to(state: &SessionState, turn: &TurnId) -> String {
    let texts: Vec<&str> = state
        .items
        .iter()
        .filter(|item| item.turn.as_ref() == Some(turn))
        .filter_map(|item| match &item.body {
            ItemBody::Assistant { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    texts.join("\n")
}

/// The reply to the last turn, for a child that is already idle when it is
/// asked: the folded state knows how that turn ended, and its last item
/// which turn it was.
pub fn last_reply(state: &SessionState) -> Reply {
    let text = state
        .items
        .iter()
        .rev()
        .find_map(|item| item.turn.as_ref())
        .map(|turn| reply_to(state, turn))
        .unwrap_or_default();
    Reply {
        status: state.last_turn.clone().unwrap_or(TurnStatus::Completed),
        text,
    }
}

/// The background watcher: when the turn this spawn started ends, the child's
/// reply wakes the parent as a peer message. The kernel's fold puts `[from
/// <name>]` above it, so the text says only what happened.
pub async fn report(
    mut attachment: Attachment,
    host: Arc<dyn ToolHost>,
    parent: SessionId,
    name: String,
) {
    let text = match next_reply(&mut attachment, &CancellationToken::new()).await {
        Ok(reply) => finished(&reply),
        Err(error) => {
            tracing::debug!(agent = %name, %error, "an agent's turn was not followed to its end");
            return;
        }
    };
    let input = Input::text(text, message::origin(Some(name.clone())));
    if let Err(error) = host.deliver(&parent, IntentId::mint(), input, Delivery::Wake) {
        tracing::warn!(agent = %name, %error, "an agent finished and its parent was gone");
    }
}

/// What a caller gets back from a call that waited for the agent itself: an
/// error result when the turn was not completed, so the model does not read a
/// crash as an answer.
pub fn output(name: &str, session: &SessionId, reply: &Reply) -> ToolOutput {
    let text = replied(name, session, reply);
    if reply.is_error() {
        ToolOutput::error(text)
    } else {
        ToolOutput::text(text)
    }
}

/// Who answered, which session it was, and what came of it.
pub fn replied(name: &str, session: &SessionId, reply: &Reply) -> String {
    let who = format!("{name} ({session})");
    if let Some(cut) = cut_short(reply) {
        return format!("{who} {cut}");
    }
    match reply.text.trim() {
        "" => format!("{who} finished without saying anything."),
        text => format!("{who} replied:\n{text}"),
    }
}

/// The message a background agent's end sends its parent.
fn finished(reply: &Reply) -> String {
    if let Some(cut) = cut_short(reply) {
        return cut;
    }
    match reply.text.trim() {
        "" => "finished, with nothing to say.".to_string(),
        text => format!("finished.\n\n{text}"),
    }
}

/// A turn that did not complete, as either reader hears of it — the verdict,
/// then whatever the agent had said before it. `None` for one that did.
fn cut_short(reply: &Reply) -> Option<String> {
    let verdict = match &reply.status {
        TurnStatus::Completed => return None,
        TurnStatus::Failed { error } => format!("failed: {}", error.message),
        TurnStatus::Interrupted { reason } => format!("was interrupted: {}", interrupted(*reason)),
    };
    Some(match reply.text.trim() {
        "" => verdict,
        text => format!("{verdict}\n\nIt had said:\n{text}"),
    })
}

fn interrupted(reason: InterruptReason) -> &'static str {
    match reason {
        InterruptReason::UserCancel => "a person stopped it",
        InterruptReason::NewInput => "new input took over its turn",
        InterruptReason::Shutdown => "the host shut down",
        InterruptReason::Budget => "it ran out of budget",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fleet, Recorder, assistant, turn_completed, turn_failed};
    use bingo_sdk::{ErrorCode, KernelError};

    fn completed(text: &str) -> Reply {
        Reply {
            status: TurnStatus::Completed,
            text: text.into(),
        }
    }

    fn failed(text: &str) -> Reply {
        Reply {
            status: TurnStatus::Failed {
                error: KernelError::new(ErrorCode::AuthRequired, "no key"),
            },
            text: text.into(),
        }
    }

    #[tokio::test]
    async fn the_reply_is_the_assistant_text_of_the_turn_that_ended() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        fleet.script([assistant("one"), assistant("two"), turn_completed()]);

        let mut attachment = follow(&fleet.handle(), &child).await.unwrap();
        let reply = next_reply(&mut attachment, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(reply, completed("one\ntwo"));
    }

    #[tokio::test]
    async fn a_cancelled_wait_stops_waiting() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        fleet.script([assistant("said nothing yet")]);

        let mut attachment = follow(&fleet.handle(), &child).await.unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = next_reply(&mut attachment, &cancel)
            .await
            .expect_err("stop");
        assert!(matches!(error, ToolError::Cancelled), "{error}");
    }

    #[tokio::test]
    async fn a_finished_background_agent_wakes_its_parent_without_signing_the_text() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        fleet.script([assistant("the diff is fine"), turn_completed()]);
        let host = Recorder::new(&fleet);

        let attachment = follow(&fleet.handle(), &child).await.unwrap();
        report(
            attachment,
            host.clone(),
            root.clone(),
            "reviewer".to_string(),
        )
        .await;

        let delivered = host.delivered();
        assert_eq!(delivered.len(), 1);
        let (to, input, delivery) = &delivered[0];
        assert_eq!(to, &root);
        assert_eq!(*delivery, Delivery::Wake);
        let Input::Text { text, origin, .. } = input else {
            panic!("a peer delivers text");
        };
        assert_eq!(text, "finished.\n\nthe diff is fine");
        assert_eq!(origin.principal.as_deref(), Some("reviewer"));
        assert_eq!(origin.surface, message::SURFACE);
    }

    #[tokio::test]
    async fn a_background_agent_that_failed_tells_its_parent_so() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        fleet.script([assistant("half a review"), turn_failed("no key")]);
        let host = Recorder::new(&fleet);

        let attachment = follow(&fleet.handle(), &child).await.unwrap();
        report(attachment, host.clone(), root, "reviewer".to_string()).await;

        let Input::Text { text, .. } = &host.delivered()[0].1 else {
            panic!("a peer delivers text");
        };
        assert_eq!(text, "failed: no key\n\nIt had said:\nhalf a review");
    }

    #[tokio::test]
    async fn a_child_whose_stream_ends_reports_nothing() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        fleet.script_ending([assistant("interrupted")]);
        let host = Recorder::new(&fleet);

        let attachment = follow(&fleet.handle(), &child).await.unwrap();
        report(attachment, host.clone(), root, "reviewer".to_string()).await;
        assert!(host.delivered().is_empty());
    }

    #[tokio::test]
    async fn an_idle_child_s_last_reply_is_what_it_said_last() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        fleet.said(&child, "the diff is fine");

        let attachment = follow(&fleet.handle(), &child).await.unwrap();
        assert!(attachment.snapshot.turn.is_none(), "the turn ended");
        assert_eq!(
            last_reply(&attachment.snapshot),
            completed("the diff is fine")
        );
    }

    #[tokio::test]
    async fn an_idle_child_whose_last_turn_failed_remembers_that() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        fleet.failed(&child, "no key");

        let attachment = follow(&fleet.handle(), &child).await.unwrap();
        let reply = last_reply(&attachment.snapshot);
        assert!(reply.is_error());
        assert_eq!(reply.text, "");
    }

    #[test]
    fn a_reply_that_says_nothing_still_reads_as_finished() {
        assert!(finished(&completed("  ")).contains("nothing to say"));
        let session = SessionId::from_raw("ses_child");
        assert_eq!(
            replied("reviewer", &session, &completed("done")),
            "reviewer (ses_child) replied:\ndone"
        );
        assert!(!output("reviewer", &session, &completed("done")).is_error);
    }

    #[test]
    fn a_turn_that_did_not_complete_is_never_read_as_an_answer() {
        let session = SessionId::from_raw("ses_child");
        assert_eq!(
            replied("reviewer", &session, &failed("")),
            "reviewer (ses_child) failed: no key"
        );
        assert_eq!(
            finished(&failed("half")),
            "failed: no key\n\nIt had said:\nhalf"
        );
        let stopped = Reply {
            status: TurnStatus::Interrupted {
                reason: InterruptReason::UserCancel,
            },
            text: String::new(),
        };
        assert_eq!(finished(&stopped), "was interrupted: a person stopped it");
        let out = output("reviewer", &session, &failed("half"));
        assert!(out.is_error, "a failure is an error result for the caller");
        let text = out.parts[0].as_text().unwrap_or_default();
        assert!(
            text.starts_with("reviewer (ses_child) failed: no key"),
            "{text}"
        );
    }
}
