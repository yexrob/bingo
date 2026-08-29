//! Following a child: its own frames, folded with the one reducer, until a
//! turn ends. Nothing here keeps a view of its own — a child's reply is
//! derived from its journal at the moment it is asked for (ADR-0002).

use std::sync::Arc;

use bingo_sdk::{
    Attachment, CancellationToken, ClientIdentity, Delivery, Event, HostHandle, Input, IntentId,
    ItemBody, KernelError, OpenOptions, SessionId, SessionSelector, SessionState, ToolError,
    ToolHost, TurnId,
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

/// The child's reply to the turn it is running: its assistant text, once that
/// turn ends. Cancelling the waiting call stops the wait, never the child.
pub async fn next_reply(
    attachment: &mut Attachment,
    cancel: &CancellationToken,
) -> Result<String, ToolError> {
    loop {
        let frame = tokio::select! {
            () = cancel.cancelled() => return Err(ToolError::Cancelled),
            frame = attachment.events.next() => frame,
        };
        let Some(frame) = frame else {
            return Err(ToolError::Failed("the agent's session ended".into()));
        };
        attachment.snapshot.apply(&frame);
        if let Event::TurnCompleted { turn, .. } = &frame.event {
            return Ok(reply_to(&attachment.snapshot, turn));
        }
    }
}

/// The reply a turn produced: its assistant items, in the order it wrote them.
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

/// The reply to the last turn that said anything, for a child that is already
/// idle when it is asked.
pub fn last_reply(state: &SessionState) -> String {
    let last = state
        .items
        .iter()
        .rev()
        .find(|item| matches!(item.body, ItemBody::Assistant { .. }));
    match last {
        Some(item) => match &item.turn {
            Some(turn) => reply_to(state, turn),
            None => text_of(&item.body).to_string(),
        },
        None => String::new(),
    }
}

fn text_of(body: &ItemBody) -> &str {
    match body {
        ItemBody::Assistant { text } => text,
        _ => "",
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

/// What a caller reads when it waited for the agent itself: who answered,
/// which session it was, and what it said.
pub fn replied(name: &str, session: &SessionId, reply: &str) -> String {
    match reply.trim() {
        "" => format!("{name} ({session}) finished without saying anything."),
        reply => format!("{name} ({session}) replied:\n{reply}"),
    }
}

fn finished(reply: &str) -> String {
    match reply.trim() {
        "" => "finished, with nothing to say.".to_string(),
        reply => format!("finished.\n\n{reply}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fleet, Recorder, assistant, turn_completed};

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
        assert_eq!(reply, "one\ntwo");
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
        assert_eq!(last_reply(&attachment.snapshot), "the diff is fine");
    }

    #[test]
    fn a_reply_that_says_nothing_still_reads_as_finished() {
        assert!(finished("  ").contains("nothing to say"));
        let session = SessionId::from_raw("ses_child");
        assert_eq!(
            replied("reviewer", &session, "done"),
            "reviewer (ses_child) replied:\ndone"
        );
    }
}
