//! What the actor does with a submission it cannot answer out of its own state.
//!
//! Three of the six routes need something outside the loop. A turn and a shell
//! line need a provider and a shell; a delivery needs whoever it woke to be
//! started. Each is split the same way, and the split is B4's ruling ② made
//! literal:
//!
//! - **the core's half** is everything that is state — the item the prose became,
//!   the turn it opened, the message that entered an inbox, the post that entered
//!   a room's log. All of it happens inside this loop, in one message, so the
//!   reply a client gets names things that already exist;
//! - **the engine's half** is everything that is work — the model call, the shell
//!   run, the instance loop a deposit woke. It leaves through
//!   [`crate::app::engine::Engine`] and comes back as ordinary turn reports.
//!
//! A core with no engine attached refuses these three by name rather than opening
//! a turn nothing will ever run: `action/list` already says which actions need an
//! engine, and this is the same answer on the submission path.

use tokio::sync::oneshot;

use crate::app::answer::Answer;
use crate::app::command::SubmitDisposition;
use crate::app::conversation::ConvKey;
use crate::app::engine::{Posted, Run};
use crate::app::ids::{ItemId, TurnId};
use crate::app::queue::{QueuedInput, QueuedKind};
use crate::app::snapshot::{ItemBody, TurnOrigin};
use crate::app::submit::DirectTarget;
use crate::app::turn::TurnMsg;
use crate::app::{AppError, AppReply};
use crate::app_server::protocol::error::ProtocolErrorKind;

impl super::Controller {
    /// The engine, or the refusal a session without one owes.
    pub(super) fn engine(&self) -> Result<crate::app::engine::Attached, AppError> {
        self.engine
            .clone()
            .ok_or(AppError::Refused(ProtocolErrorKind::ActionUnavailable))
    }

    /// Prose on main: one item, one turn, one run.
    ///
    /// The order is the contract. The prose becomes an item *before* the turn
    /// exists, because a user message that opens a turn carries no `turnId` and
    /// the turn names it as an input item instead (spec "Item"). Then the turn
    /// opens, and only then does anything leave the loop — so by the time the
    /// engine has the work, every identifier the reply mentions is already real.
    pub(super) fn start_turn(
        &mut self,
        text: String,
        origin: TurnOrigin,
    ) -> Result<AppReply, AppError> {
        let engine = self.engine()?;
        let input = self.commit_body(
            &ConvKey::Main,
            ItemBody::UserMessage {
                text: text.clone(),
                attachments: Vec::new(),
            },
        );
        let turn = self.open_turn(origin, vec![input])?;
        engine.run(Run::Turn {
            turn: turn.clone(),
            text,
        });
        Ok(AppReply::Submitted(SubmitDisposition::TurnStarted {
            turn_id: turn,
        }))
    }

    /// A shell line. It always runs in the console's context, whichever page it
    /// was submitted from, and the model may answer it afterwards — which is why
    /// it is a turn rather than a command with no turn around it.
    ///
    /// The item is the line as it was typed, `!` and all: a client reading the
    /// conversation back has to see what was submitted, not the command stripped
    /// of the thing that made it one.
    pub(super) fn start_shell(
        &mut self,
        command: String,
        origin: TurnOrigin,
    ) -> Result<AppReply, AppError> {
        let engine = self.engine()?;
        let input = self.commit_body(
            &ConvKey::Main,
            ItemBody::UserMessage {
                text: format!("!{command}"),
                attachments: Vec::new(),
            },
        );
        let turn = self.open_turn(origin, vec![input])?;
        engine.run(Run::Shell {
            turn: turn.clone(),
            command,
        });
        Ok(AppReply::Submitted(SubmitDisposition::TurnStarted {
            turn_id: turn,
        }))
    }

    /// Open main's turn.
    ///
    /// It cannot already be busy: routing asked that a moment ago and answered
    /// "idle", and nothing runs between the two — this loop is the process's one
    /// ordering point, and both happen inside one of its messages. The refusal is
    /// what that invariant looks like if it is ever broken, rather than a second
    /// turn writing one conversation.
    fn open_turn(
        &mut self,
        origin: TurnOrigin,
        input_items: Vec<ItemId>,
    ) -> Result<TurnId, AppError> {
        let (reply, answer) = oneshot::channel();
        let changes = self.turns.handle(
            TurnMsg::Open {
                conversation: ConvKey::Main,
                origin,
                input_items,
                reply,
            },
            &mut self.mint,
        );
        self.announce_turn(changes);
        Answer::new(answer, None)
            .now()
            .ok_or(AppError::Refused(ProtocolErrorKind::BadArgument))
    }

    /// A message for somebody who runs no turn for it.
    ///
    /// Both lanes land the message inside this loop, because both inboxes are
    /// this loop's tables — and that is what makes the item the reply names a
    /// fact rather than a promise. What travels to the engine afterwards is only
    /// the waking.
    pub(super) fn serve_deliver(
        &mut self,
        target: DirectTarget,
        text: String,
        addressed: bool,
    ) -> Result<AppReply, AppError> {
        let engine = self.engine()?;
        let (item, run) = match &target {
            DirectTarget::Agent(name) => (self.deposit(name, &text)?, Run::Wake),
            DirectTarget::Room(name) => self.post(name, &text)?,
        };
        // An addressed send leaves a receipt on the page it was typed on; a
        // page's own prose is already drawn where it was typed and needs none
        // (D105).
        if addressed {
            self.commit_body(
                &ConvKey::Main,
                ItemBody::PeerMessage {
                    from: crate::channels::USER_NAME.to_string(),
                    to: Some(target.label()),
                    text,
                    delivery_id: None,
                },
            );
        }
        engine.run(run);
        Ok(AppReply::Submitted(SubmitDisposition::Delivered {
            message_id: item,
        }))
    }

    /// The private lane: one message into one instance's inbox.
    fn deposit(&mut self, name: &str, text: &str) -> Result<ItemId, AppError> {
        // The user's own send is not chased. A watchdog exists to tell a
        // *colleague* that its message went unanswered; the user is looking at
        // the page it landed on (D44 read the same way).
        let (message, answer) =
            crate::agents::Delivery::build(name, crate::channels::USER_NAME, text, None);
        self.agents.handle(message);
        let delivered = Answer::new(answer, Err(String::new())).now();
        let committed = self.absorb_deliveries_into();
        self.announce_agents();
        self.announce_deliveries();
        delivered.map_err(|_| AppError::Refused(ProtocolErrorKind::ConversationNotFound))?;
        last_in(committed, &ConvKey::Agent(name.to_string()))
            .ok_or(AppError::Refused(ProtocolErrorKind::ConversationNotFound))
    }

    /// The room lane: one post into one room's log, and one copy into every
    /// member's inbox.
    ///
    /// The deposits are the actor's, because an inbox is the actor's table. What
    /// they find is not: a mentioned member that took its copy owes an answer and
    /// gets a watchdog, and a mentioned member that is stopped is reported to the
    /// sender rather than waited on. Both of those are the engine's, so what the
    /// deposits found travels with the wake.
    fn post(&mut self, name: &str, text: &str) -> Result<(ItemId, Run), AppError> {
        use crate::agents::InboxItem;
        use crate::channels::{ChannelMsg, PostOutcome};
        // Speaking in a room is joining it (D103). The domain writes the
        // membership line into the room's own log, so every member sees the same
        // arrival the joiner does — which is exactly what keeps *reading* a room
        // free. It happens here rather than in a frontend because a client that
        // posts to a room it is only watching must join by the same door the
        // console's `#room` line goes through.
        let seated = self.channels.facts().into_iter().any(|room| {
            room.name == name
                && room
                    .members
                    .iter()
                    .any(|member| member == crate::channels::USER_NAME)
        });
        if !seated {
            let (reply, joined) = oneshot::channel();
            self.channels.handle(ChannelMsg::Invite {
                name: name.to_string(),
                member: crate::channels::USER_NAME.to_string(),
                reply,
            });
            self.absorb_posts();
            self.announce_rooms();
            Answer::new(joined, Err(String::new()))
                .now()
                .map_err(|_| AppError::Refused(ProtocolErrorKind::ConversationNotFound))?;
        }
        let (reply, answer) = oneshot::channel();
        self.channels.handle(ChannelMsg::Post {
            from: crate::channels::USER_NAME.to_string(),
            name: name.to_string(),
            text: text.to_string(),
            reply,
        });
        let posted = Answer::new(answer, Err(String::new()))
            .now()
            .map_err(|_| AppError::Refused(ProtocolErrorKind::ConversationNotFound))?;
        let PostOutcome::Sent {
            seq, deliveries, ..
        } = posted
        else {
            // Serial behind: the room moved under the sender. A client that was
            // reading the room has the missed posts already — they are items —
            // so what it needs is to be told its post did not go, not to be told
            // them again.
            return Err(AppError::Refused(ProtocolErrorKind::StaleRevision));
        };
        let (mut chase, mut undelivered) = (Vec::new(), Vec::new());
        for delivery in deliveries {
            let (reply, accepted) = oneshot::channel();
            self.agents.handle(crate::agents::AgentMsg::Deposit {
                name: delivery.member.clone(),
                item: InboxItem::Channel {
                    channel: name.to_string(),
                    from: delivery.msg.from.clone(),
                    text: delivery.msg.text.clone(),
                    seq: delivery.msg.seq,
                },
                reply,
            });
            let accepted = Answer::new(accepted, false).now();
            if delivery.mentioned {
                if accepted {
                    chase.push(delivery.member);
                } else {
                    undelivered.push(delivery.member);
                }
            }
        }
        let committed = self.absorb_posts_into();
        self.announce_rooms();
        self.announce_agents();
        let room = ConvKey::Room(name.to_string());
        let item = last_in(committed, &room)
            .ok_or(AppError::Refused(ProtocolErrorKind::ConversationNotFound))?;
        Ok((
            item,
            Run::Posted(Box::new(Posted {
                room: name.to_string(),
                from: crate::channels::USER_NAME.to_string(),
                text: text.to_string(),
                seq,
                chase,
                undelivered,
            })),
        ))
    }

    /// The queue's turn boundary.
    ///
    /// A turn ending is what lets the next entry start, and only a core that runs
    /// turns may do this — while the console still owns its own run loop it also
    /// owns its own drain, and two drains would take the same entry twice.
    ///
    /// Slash lines drain in place and keep going, because applying one is not a
    /// turn: the drain stops at the first entry that actually starts work.
    pub(super) fn drain_main(&mut self) {
        if self.engine.is_none() {
            return;
        }
        while !self.turns.is_busy(&ConvKey::Main) {
            let Some(next) = self.next_queued() else {
                return;
            };
            let started = match next.kind {
                // The turn a drain opens says where it came from: it is the
                // queue's, not a second thing the user just typed.
                QueuedKind::Prose => self.start_turn(next.text, TurnOrigin::Queue).is_ok(),
                QueuedKind::Shell => self.start_shell(next.text, TurnOrigin::Queue).is_ok(),
                QueuedKind::Command => {
                    // The page it was typed on travels with it (D135a): a
                    // `/compact` queued on an instance's page summarises that
                    // instance's history, not whatever is on screen when it
                    // drains.
                    let origin = self.conversations.id(&mut self.mint, &next.on);
                    let line = next.text.strip_prefix('/').unwrap_or(&next.text);
                    let _ = self.serve_command_line(line, &origin);
                    false
                }
            };
            if started {
                return;
            }
        }
    }

    fn next_queued(&mut self) -> Option<QueuedInput> {
        let (reply, answer) = oneshot::channel();
        let changes = self.queue.handle(
            crate::app::queue::QueueMsg::DrainFront {
                conversation: ConvKey::Main,
                reply,
            },
            &mut self.mint,
        );
        self.announce_queue(changes);
        Answer::new(answer, None).now()
    }
}

/// The last item committed into one conversation by an absorption.
fn last_in(committed: Vec<(ConvKey, ItemId)>, conversation: &ConvKey) -> Option<ItemId> {
    committed
        .into_iter()
        .rev()
        .find(|(key, _)| key == conversation)
        .map(|(_, id)| id)
}

/// The submission path with an engine behind it: what the core does itself, and
/// what it hands over.
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::app::command::{AppCommand, ComposerMode, Submission, SubmitDisposition};
    use crate::app::conversation::ConvKey;
    use crate::app::engine::{Recorder, Run};
    use crate::app::event::{AppEvent, AppEventPayload};
    use crate::app::ids::{ConversationId, ItemId};
    use crate::app::snapshot::{ItemBody, TurnOrigin, TurnStatus};
    use crate::app::{
        AppCore, AppError, AppFrame, AppLink, AppReply, AppRequest, AttachRequest, RequestId,
        SessionSetup,
    };
    use crate::channels::{ChannelMode, MAIN_NAME, USER_NAME};

    /// A core that can run, and the engine that remembers what it was asked to.
    fn running() -> (AppCore, Arc<Recorder>) {
        let core = AppCore::start(SessionSetup::default());
        let engine = Arc::new(Recorder::default());
        core.attach_engine(engine.clone());
        (core, engine)
    }

    async fn attached(core: &AppCore) -> AppLink {
        let mut link = core
            .attach(AttachRequest::new("run"))
            .unwrap_or_else(|error| panic!("{error}"));
        link.request(AppRequest::Query {
            id: RequestId(1),
            query: crate::app::command::AppQuery::ReadSession,
        })
        .unwrap_or_else(|error| panic!("{error}"));
        match link.recv().await {
            Some(AppFrame::Reply { .. }) => link,
            other => panic!("expected the session cut, got {other:?}"),
        }
    }

    async fn submit(link: &mut AppLink, id: u64, text: &str) -> Result<AppReply, AppError> {
        link.request(AppRequest::Command {
            id: RequestId(id),
            command: AppCommand::Submit {
                conversation_id: ConversationId::new("conv_1"),
                input: Submission::Composer {
                    mode: ComposerMode::Normal,
                    text: text.to_string(),
                    attachments: Vec::new(),
                },
            },
        })
        .unwrap_or_else(|error| panic!("{error}"));
        loop {
            match link.recv().await {
                Some(AppFrame::Reply { id: seen, result }) if seen == RequestId(id) => {
                    return result;
                }
                Some(_) => {}
                None => panic!("the core closed"),
            }
        }
    }

    async fn drain(link: &mut AppLink) -> Vec<AppEvent> {
        let mut seen = Vec::new();
        while let Ok(Some(frame)) =
            tokio::time::timeout(std::time::Duration::from_millis(200), link.recv()).await
        {
            if let AppFrame::Event(event) = frame {
                seen.push(*event);
            }
        }
        seen
    }

    fn items(events: &[AppEvent]) -> Vec<&crate::app::snapshot::Item> {
        events
            .iter()
            .filter_map(|event| match &event.payload {
                AppEventPayload::ItemCompleted(changed) => Some(&changed.item),
                _ => None,
            })
            .collect()
    }

    /// Prose on main: the item exists, the turn exists, and only then does the
    /// work leave — so the identifiers the reply carries are never promises.
    #[tokio::test]
    async fn the_prose_is_an_item_and_the_turn_is_open_before_the_engine_hears_of_it() {
        let (core, engine) = running();
        let mut link = attached(&core).await;
        let turn = match submit(&mut link, 2, "run the tests").await {
            Ok(AppReply::Submitted(SubmitDisposition::TurnStarted { turn_id })) => turn_id,
            other => panic!("expected a turn, got {other:?}"),
        };
        let events = drain(&mut link).await;
        let input: Vec<&ItemId> = items(&events)
            .into_iter()
            .filter(|item| matches!(&item.body, ItemBody::UserMessage { text, .. } if text == "run the tests"))
            .map(|item| &item.id)
            .collect();
        assert_eq!(input.len(), 1, "the prose became exactly one item");
        let started = events
            .iter()
            .find_map(|event| match &event.payload {
                AppEventPayload::TurnStarted(changed) => Some(&changed.turn),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the turn is announced"));
        assert_eq!(&started.id, &turn);
        assert_eq!(started.origin, TurnOrigin::User);
        assert_eq!(
            started.input_item_ids,
            vec![input[0].clone()],
            "the turn names the item it opened with (spec \"Item\")"
        );
        assert_eq!(
            engine.taken(),
            vec![Run::Turn {
                turn,
                text: "run the tests".to_string()
            }]
        );
    }

    /// A shell line is main's turn wherever it was typed, and the item keeps the
    /// `!` the user actually submitted.
    #[tokio::test]
    async fn a_shell_line_opens_mains_turn_and_keeps_what_was_typed() {
        let (core, engine) = running();
        let mut link = attached(&core).await;
        link.request(AppRequest::Command {
            id: RequestId(3),
            command: AppCommand::Submit {
                conversation_id: ConversationId::new("conv_1"),
                input: Submission::Composer {
                    mode: ComposerMode::Shell,
                    text: "cargo test".to_string(),
                    attachments: Vec::new(),
                },
            },
        })
        .unwrap_or_else(|error| panic!("{error}"));
        let turn = loop {
            match link.recv().await {
                Some(AppFrame::Reply {
                    result: Ok(AppReply::Submitted(SubmitDisposition::TurnStarted { turn_id })),
                    ..
                }) => break turn_id,
                Some(_) => {}
                None => panic!("the core closed"),
            }
        };
        let events = drain(&mut link).await;
        assert!(
            items(&events).into_iter().any(
                |item| matches!(&item.body, ItemBody::UserMessage { text, .. } if text == "!cargo test")
            ),
            "the transcript shows the line as it was submitted"
        );
        assert_eq!(
            engine.taken(),
            vec![Run::Shell {
                turn,
                command: "cargo test".to_string()
            }]
        );
    }

    /// A direct send: the message is in the inbox and in the instance's
    /// conversation before the reply names it, and all the engine gets is the
    /// waking.
    #[tokio::test]
    async fn a_direct_send_lands_in_the_inbox_and_only_the_waking_travels() {
        let (core, engine) = running();
        core.agents()
            .insert(
                "scout",
                crate::agents::AgentKind::Crew,
                None,
                "scout".to_string(),
                crate::app::collab_tests::test_session(&core),
            )
            .await;
        let mut link = attached(&core).await;
        let message = match submit(&mut link, 4, "@scout look at the lexer").await {
            Ok(AppReply::Submitted(SubmitDisposition::Delivered { message_id })) => message_id,
            other => panic!("expected a delivery, got {other:?}"),
        };
        assert_eq!(engine.taken(), vec![Run::Wake]);
        let events = drain(&mut link).await;
        let landed = items(&events)
            .into_iter()
            .find(|item| item.id == message)
            .unwrap_or_else(|| panic!("the reply names an item that exists"));
        match &landed.body {
            ItemBody::PeerMessage { from, to, text, .. } => {
                assert_eq!(from, USER_NAME);
                assert_eq!(to.as_deref(), Some("scout"));
                assert_eq!(text, "look at the lexer");
            }
            other => panic!("expected the message, got {other:?}"),
        }
        assert_eq!(
            core.agents().pending_of("scout").len(),
            1,
            "the inbox holds it"
        );
    }

    /// A post: the room's log has it, every member's inbox has a copy, and the
    /// mention the post owes travels with the wake rather than being inferred
    /// downstream.
    #[tokio::test]
    async fn a_post_reaches_the_log_and_the_mention_it_owes_travels_with_the_wake() {
        let (core, engine) = running();
        core.agents()
            .insert(
                "scout",
                crate::agents::AgentKind::Crew,
                None,
                "scout".to_string(),
                crate::app::collab_tests::test_session(&core),
            )
            .await;
        core.channels()
            .create(
                "build",
                vec![MAIN_NAME.to_string(), "scout".to_string()],
                ChannelMode::Free,
            )
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        core.channels()
            .invite("build", USER_NAME)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let mut link = attached(&core).await;
        let message = match submit(&mut link, 5, "#build @scout tests are green").await {
            Ok(AppReply::Submitted(SubmitDisposition::Delivered { message_id })) => message_id,
            other => panic!("expected a delivery, got {other:?}"),
        };
        match engine.taken().as_slice() {
            [Run::Posted(posted)] => {
                assert_eq!(posted.room, "build");
                assert_eq!(posted.from, USER_NAME);
                assert_eq!(
                    posted.chase,
                    vec!["scout".to_string()],
                    "a mentioned member that took its copy owes an answer"
                );
                assert!(posted.undelivered.is_empty());
            }
            other => panic!("expected one post to finish, got {other:?}"),
        }
        let events = drain(&mut link).await;
        let posted = items(&events)
            .into_iter()
            .find(|item| item.id == message)
            .unwrap_or_else(|| panic!("the reply names an item that exists"));
        assert!(matches!(
            &posted.body,
            ItemBody::RoomMessage { from, text, .. }
                if from == USER_NAME && text == "@scout tests are green"
        ));
        assert_eq!(
            core.agents()
                .list()
                .into_iter()
                .find(|status| status.name == "scout")
                .map(|status| status.pending),
            Some(1),
            "every member got its copy"
        );
    }

    /// Speaking in a room you are only watching joins you first, and the join is
    /// announced in the room's own log (D103).
    ///
    /// It used to be the console that did this, which meant a client posting over
    /// the wire was refused by name — "join the room before speaking in it" — for
    /// doing exactly what a `#room` line does in the terminal. One door.
    #[tokio::test]
    async fn speaking_in_a_room_joins_it_and_the_room_is_told() {
        let (core, _engine) = running();
        core.channels()
            .create("build", vec![MAIN_NAME.to_string()], ChannelMode::Free)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(
            !core.channels().is_member("build", USER_NAME),
            "the user starts outside the room"
        );
        let mut link = attached(&core).await;
        assert!(
            matches!(
                submit(&mut link, 5, "#build tests are green").await,
                Ok(AppReply::Submitted(SubmitDisposition::Delivered { .. }))
            ),
            "the post goes through"
        );
        assert!(
            core.channels().is_member("build", USER_NAME),
            "speaking made them a member"
        );
        let log = core.channels().log_of("build");
        assert!(
            log.iter().any(|message| message.text.contains("joined")),
            "the membership line is in the room's own log: {:?}",
            log.iter().map(|m| &m.text).collect::<Vec<_>>()
        );
        assert_eq!(
            log.last().map(|message| message.text.as_str()),
            Some("tests are green"),
            "and the post follows it"
        );
    }

    /// The queue's turn boundary: one turn ending starts the next entry, in the
    /// order it was queued, and the terminal event comes first.
    #[tokio::test]
    async fn the_queue_drains_when_the_turn_that_was_blocking_it_ends() {
        let (core, engine) = running();
        let mut link = attached(&core).await;
        let first = match submit(&mut link, 6, "first").await {
            Ok(AppReply::Submitted(SubmitDisposition::TurnStarted { turn_id })) => turn_id,
            other => panic!("expected a turn, got {other:?}"),
        };
        assert!(matches!(
            submit(&mut link, 7, "second").await,
            Ok(AppReply::Submitted(SubmitDisposition::Queued { .. }))
        ));
        let _ = drain(&mut link).await;
        core.turns()
            .close(first.clone(), TurnStatus::Completed, None);
        let events = drain(&mut link).await;
        let order: Vec<&str> = events
            .iter()
            .filter_map(|event| match &event.payload {
                AppEventPayload::TurnCompleted(_) => Some("completed"),
                AppEventPayload::TurnStarted(_) => Some("started"),
                _ => None,
            })
            .collect();
        assert_eq!(
            order,
            vec!["completed", "started"],
            "the turn that ended is announced before the one its ending started"
        );
        match engine.taken().as_slice() {
            [
                Run::Turn { text: first, .. },
                Run::Turn { text: second, .. },
            ] => {
                assert_eq!(first, "first");
                assert_eq!(second, "second", "the queue drained in order");
            }
            other => panic!("expected two runs, got {other:?}"),
        }
        assert!(core.turns().view().is_busy(&ConvKey::Main));
    }

    /// A slash line that needs the engine survives the queue.
    ///
    /// This is the silent failure B7d found: the drain applies a queued command
    /// through the same action table a typed one goes through, and every action
    /// that needs a model used to fall through to `ActionUnavailable` there —
    /// so a `/compact` typed behind a running turn did nothing at all, and said
    /// nothing about it either.
    #[tokio::test]
    async fn a_queued_compaction_is_performed_when_the_queue_drains() {
        let (core, engine) = running();
        let mut link = attached(&core).await;
        let first = match submit(&mut link, 9, "first").await {
            Ok(AppReply::Submitted(SubmitDisposition::TurnStarted { turn_id })) => turn_id,
            other => panic!("expected a turn, got {other:?}"),
        };
        assert!(
            matches!(
                submit(&mut link, 10, "/compact").await,
                Ok(AppReply::Submitted(SubmitDisposition::Queued { .. }))
            ),
            "a compaction waits: it rewrites what the running turn is reading"
        );
        let _ = drain(&mut link).await;
        core.turns().close(first, TurnStatus::Completed, None);
        let events = drain(&mut link).await;
        match engine.taken().as_slice() {
            [Run::Turn { .. }, Run::Act(act)] => {
                assert_eq!(
                    act.action,
                    crate::app::command::Action::ConversationCompact { instructions: None }
                );
                assert_eq!(act.on, ConvKey::Main, "on the page it was typed on (D135a)");
                assert!(act.operation.is_some(), "under an operation of its own");
            }
            other => panic!("expected the queued compaction to be handed over, got {other:?}"),
        }
        let opened = events.iter().any(|event| {
            matches!(&event.payload, AppEventPayload::OperationStarted(started)
                if started.operation.kind == crate::app::snapshot::OperationKind::Compact)
        });
        assert!(opened, "and the client is told it is running: {events:#?}");
    }

    /// A core with no engine keeps the console's arrangement: it queues, and the
    /// drain is not its to run.
    #[tokio::test]
    async fn a_core_with_no_engine_never_drains_the_queue() {
        let core = AppCore::start(SessionSetup::default());
        let turn = core
            .turns()
            .open(ConvKey::Main, TurnOrigin::User, Vec::new())
            .await
            .unwrap_or_else(|| panic!("main was idle"));
        let mut link = attached(&core).await;
        assert!(matches!(
            submit(&mut link, 8, "queued behind it").await,
            Ok(AppReply::Submitted(SubmitDisposition::Queued { .. }))
        ));
        core.turns().close(turn, TurnStatus::Completed, None);
        let _ = drain(&mut link).await;
        assert!(
            !core.turns().view().is_busy(&ConvKey::Main),
            "the console owns its own drain until it attaches"
        );
    }
}
