//! The one submission path: what a composer line is, and what the core does
//! with it.
//!
//! Reading a line and routing it are two steps and one authority. [`compose`]
//! says what the line *is* — shell, command, direct send, prose — and [`route`]
//! says what happens to it. Neither is a frontend's to decide: a GUI's composer
//! and the terminal's produce the same [`Submission`], so a leading slash, shell
//! mode and the `@name`/`#room` grammar cannot mean one thing in one client and
//! something else in another (spec "One submission path").
//!
//! The rules the routing applies are the CLI's, unchanged:
//!
//! - prose on main starts work when idle and queues while busy;
//! - prose on an agent or a room is delivered to that conversation;
//! - direct sends do not wait behind main's busy state;
//! - shell and session actions always act in the console's context, even when
//!   submitted from another page;
//! - the origin page is retained (D135/D135a), so a queued command runs where it
//!   was typed rather than where the screen happens to be when it drains.

use crate::app::command::{ComposerMode, Submission};
use crate::app::conversation::ConvKey;
use crate::app::queue::{Enqueue, QueuedKind};

/// Who a composer line addresses when it opens with a sigil (D103).
///
/// CC's `parseDirectMemberMessage` (2.1.88 `utils/directMemberMessage.ts`) is the
/// shape: a bare `@name`, whitespace, and a non-empty rest. bingo adds the room
/// half, because a room is the one thing it has that CC does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectTarget {
    /// An instance's inbox, written to as the user.
    Agent(String),
    /// A room's log, posted to as the user (joining first if need be).
    Room(String),
}

impl DirectTarget {
    /// The address as it is written: the sigil is part of the name.
    pub fn label(&self) -> String {
        match self {
            Self::Agent(name) => format!("@{name}"),
            Self::Room(name) => format!("#{name}"),
        }
    }

    /// The conversation this delivery lands in.
    pub fn conversation(&self) -> ConvKey {
        match self {
            Self::Agent(name) => ConvKey::Agent(name.clone()),
            Self::Room(name) => ConvKey::Room(name.clone()),
        }
    }
}

/// The names a sigil can resolve against. Resolved against the live registries
/// rather than an accounting snapshot: an instance spawned two frames ago is
/// already addressable.
#[derive(Debug, Default, Clone)]
pub struct Addressable {
    pub agents: Vec<String>,
    pub rooms: Vec<String>,
}

/// What a composer line is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Composed {
    /// Nothing was typed.
    Empty,
    /// Shell mode's line, which is a command for the console's shell.
    Shell(String),
    /// A slash command, without its leading slash.
    Command(String),
    /// A direct send: it reaches the addressee and the model never sees it.
    Direct {
        target: DirectTarget,
        body: String,
    },
    Prose(String),
}

/// Read one submission.
///
/// The order is the grammar: shell mode and a leading slash decide what a line
/// *is* before its first word decides where it goes, and an unresolved sigil is
/// prose — `@utils explain this code` is a question about a directory, not a
/// failed delivery, and CC settles it the same way.
pub fn compose(input: &Submission, known: &Addressable) -> Composed {
    let (text, parses) = match input {
        Submission::Composer { mode, text, .. } => (text, Some(*mode)),
        // The explicit programmatic delivery: it parses none of these forms.
        Submission::SendProse { text, .. } => (text, None),
    };
    if text.trim().is_empty() {
        return Composed::Empty;
    }
    let Some(mode) = parses else {
        return Composed::Prose(text.clone());
    };
    if mode == ComposerMode::Shell {
        return Composed::Shell(text.trim().to_string());
    }
    if let Some(rest) = text.strip_prefix('/') {
        return Composed::Command(rest.to_string());
    }
    match direct(text, known) {
        Some((target, body)) => Composed::Direct { target, body },
        None => Composed::Prose(text.clone()),
    }
}

/// `@scout fix the lexer too` and `#build tests are green`, or `None` when the
/// line is an ordinary prompt.
///
/// **The whitespace may be a newline**, matching CC's `/^@([\w-]+)\s+(.+)$/s` —
/// a pasted block under a name is still a message to that name.
fn direct(text: &str, known: &Addressable) -> Option<(DirectTarget, String)> {
    let sigil = text.chars().next()?;
    if sigil != '@' && sigil != '#' {
        return None;
    }
    let cut = text.find(char::is_whitespace)?;
    let name = text.get(sigil.len_utf8()..cut)?;
    if name.is_empty() {
        return None;
    }
    let body = text[cut..].trim();
    if body.is_empty() {
        return None;
    }
    let (pool, target) = match sigil {
        '@' => (&known.agents, DirectTarget::Agent(name.to_string())),
        _ => (&known.rooms, DirectTarget::Room(name.to_string())),
    };
    if !pool.iter().any(|held| held == name) {
        return None;
    }
    Some((target, body.to_string()))
}

/// What the core **did** with a submission.
///
/// The caller never chooses between these, and by the time one exists everything
/// it names is real: the item the prose became, the turn the engine is running,
/// the message that entered an inbox, the entry on the queue. The wire's
/// [`crate::app::command::SubmitDisposition`] is a mapping over this rather than
/// a second answer (D154).
///
/// One arm is not performed, on purpose. A slash command's *view* — `/status`,
/// `/help`, the model picker — is the frontend's, so the line comes back with
/// the page it was typed on and each surface renders its own.
#[derive(Debug, Clone, PartialEq)]
pub enum Performed {
    /// Nothing was submitted.
    Nothing,
    /// The prose is an item, main's turn is open, and the engine has the work.
    Turn { turn: crate::app::ids::TurnId },
    /// The `!` line is an item and its run is open, always in the console's
    /// context.
    Shell {
        turn: crate::app::ids::TurnId,
        command: String,
    },
    /// A slash command, with the page it was submitted on.
    Command { line: String, on: ConvKey },
    /// Accepted behind the console's busy turn.
    Queued(crate::app::queue::Placement),
    /// It reached a conversation that runs no turn for it. `addressed` marks the
    /// `@name`/`#room` grammar, which leaves a receipt; the page's own prose is
    /// drawn on the screen it was typed on and needs none (D105).
    Delivered {
        target: DirectTarget,
        addressed: bool,
        /// The body, as the addressee received it: the sigil and the whitespace
        /// after it are the envelope and are not part of the message.
        text: String,
        item: crate::app::ids::ItemId,
    },
    /// The domain would not take the delivery, in the domain's own words. The
    /// sentence travels because it is the only thing that says *why* — the wire's
    /// error kind is a category, and a reader needs the reason.
    Undelivered {
        target: DirectTarget,
        addressed: bool,
        kind: crate::app_server::protocol::error::ProtocolErrorKind,
        why: String,
    },
    /// The core would not run it: this session has no engine.
    Unavailable,
}

/// Everything the core knows when it routes: which page the line was submitted
/// on, and whether the console is busy.
#[derive(Debug, Clone)]
pub struct Origin {
    pub page: ConvKey,
    pub main_busy: bool,
}

/// Where a composed line goes, before anything is done about it.
///
/// Split out from performing it so the decision can be read on its own: the
/// queueing half needs the actor's queue, and the rest of the rules do not need
/// anything at all.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Nothing,
    Turn {
        text: String,
    },
    Shell {
        command: String,
    },
    Command {
        line: String,
        on: ConvKey,
    },
    /// It waits behind the console's turn. The entry is put on the queue by the
    /// caller, which is what mints its identifier.
    Queue(Box<Enqueue>),
    Deliver {
        target: DirectTarget,
        text: String,
        addressed: bool,
    },
}

/// Route one composed line.
pub fn route(composed: Composed, origin: &Origin) -> Decision {
    let queued = |text: String, kind: QueuedKind, carries_attachments: bool| {
        Decision::Queue(Box::new(Enqueue {
            conversation: ConvKey::Main,
            origin: origin.page.clone(),
            text,
            kind,
            attachments: Vec::new(),
            carries_attachments,
        }))
    };
    match composed {
        Composed::Empty => Decision::Nothing,
        // A direct send is addressed to somebody else, so main's busy state is
        // not its business: it neither queues behind a running turn nor steers
        // it. It waits in the receiver's inbox, which is the domain's queue.
        Composed::Direct { target, body } => Decision::Deliver {
            target,
            text: body,
            addressed: true,
        },
        Composed::Shell(command) => {
            if origin.main_busy {
                // The `!` line enters the console's transcript and runs its
                // shell, so it waits behind the console's own turn — as a shell
                // line, because draining it as prose would send the command to
                // the model instead of running it.
                queued(command, QueuedKind::Shell, false)
            } else {
                Decision::Shell { command }
            }
        }
        Composed::Command(line) => {
            // Which lines skip a running turn is the command table's answer, not
            // a second list of names beside it (D146).
            if origin.main_busy && !crate::app::action::is_instant(&line) {
                queued(format!("/{line}"), QueuedKind::Command, false)
            } else {
                Decision::Command {
                    line,
                    on: origin.page.clone(),
                }
            }
        }
        Composed::Prose(text) => {
            // The one step that reads which page this is: prose belongs to the
            // conversation on screen. Main's is a turn of its own; anybody
            // else's is a delivery.
            match page_addressee(&origin.page) {
                Some(target) => Decision::Deliver {
                    target,
                    text,
                    addressed: false,
                },
                None if origin.main_busy => queued(text, QueuedKind::Prose, false),
                None => Decision::Turn { text },
            }
        }
    }
}

/// Who the composer addresses when the screen is not main's — the page's own
/// subject, spelled exactly as the `@name`/`#room` grammar spells it, because it
/// is the same delivery.
pub fn page_addressee(page: &ConvKey) -> Option<DirectTarget> {
    match page {
        ConvKey::Main => None,
        ConvKey::Agent(name) => Some(DirectTarget::Agent(name.clone())),
        ConvKey::Room(name) => Some(DirectTarget::Room(name.clone())),
    }
}

/// What a caller hands the core when it submits.
#[derive(Debug, Clone)]
pub struct SubmitRequest {
    /// The conversation it was submitted on, which is also the page it belongs
    /// to (D135a).
    pub conversation: ConvKey,
    pub input: Submission,
    /// The text names images the core has not been handed as assets yet.
    /// B5's `asset/registerPath` retires this.
    pub carries_attachments: bool,
}

/// How the rest of the process submits.
#[derive(Clone)]
pub struct SubmitHandle {
    control: tokio::sync::mpsc::UnboundedSender<crate::app::controller::Control>,
}

impl std::fmt::Debug for SubmitHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SubmitHandle")
    }
}

impl SubmitHandle {
    pub(crate) fn new(
        control: tokio::sync::mpsc::UnboundedSender<crate::app::controller::Control>,
    ) -> Self {
        Self { control }
    }

    /// The one submission path. What comes back is what the core *did*: by the
    /// time it answers, the turn is running, the message is in its inbox, or the
    /// entry is on the queue.
    pub fn submit(&self, request: SubmitRequest) -> crate::app::answer::Answer<Performed> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        let _ = self.control.send(crate::app::controller::Control::Submit {
            request: Box::new(request),
            reply,
        });
        crate::app::answer::Answer::new(answer, Performed::Nothing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Addressable {
        Addressable {
            agents: vec!["scout".to_string()],
            rooms: vec!["build".to_string()],
        }
    }

    fn composer(text: &str) -> Submission {
        Submission::Composer {
            mode: ComposerMode::Normal,
            text: text.to_string(),
            attachments: Vec::new(),
        }
    }

    fn on(page: ConvKey, main_busy: bool) -> Origin {
        Origin { page, main_busy }
    }

    #[test]
    fn a_sigil_addresses_somebody_only_when_the_name_resolves() {
        assert_eq!(
            compose(&composer("@scout fix the lexer"), &known()),
            Composed::Direct {
                target: DirectTarget::Agent("scout".to_string()),
                body: "fix the lexer".to_string(),
            }
        );
        assert_eq!(
            compose(&composer("#build tests are green"), &known()),
            Composed::Direct {
                target: DirectTarget::Room("build".to_string()),
                body: "tests are green".to_string(),
            }
        );
        assert_eq!(
            compose(&composer("@utils explain this code"), &known()),
            Composed::Prose("@utils explain this code".to_string()),
            "an unresolved name is a question about a directory, not a failed delivery"
        );
        assert_eq!(
            compose(&composer("@scout"), &known()),
            Composed::Prose("@scout".to_string()),
            "addressing somebody and saying nothing is a prompt"
        );
        assert_eq!(
            compose(&composer("@scout\nlook at this"), &known()),
            Composed::Direct {
                target: DirectTarget::Agent("scout".to_string()),
                body: "look at this".to_string(),
            },
            "a pasted block under a name is still a message to that name"
        );
    }

    /// A leading slash and shell mode decide what a line is before its first word
    /// decides where it goes.
    #[test]
    fn shell_mode_and_a_leading_slash_outrank_the_address_grammar() {
        assert_eq!(
            compose(
                &Submission::Composer {
                    mode: ComposerMode::Shell,
                    text: "@scout ls".to_string(),
                    attachments: Vec::new(),
                },
                &known()
            ),
            Composed::Shell("@scout ls".to_string())
        );
        assert_eq!(
            compose(&composer("/compact"), &known()),
            Composed::Command("compact".to_string())
        );
    }

    /// The programmatic path parses none of the composer's forms.
    #[test]
    fn send_prose_is_never_read_as_a_command_or_an_address() {
        for text in ["/compact", "@scout hello", "!ls"] {
            assert_eq!(
                compose(
                    &Submission::SendProse {
                        text: text.to_string(),
                        attachments: Vec::new(),
                    },
                    &known()
                ),
                Composed::Prose(text.to_string())
            );
        }
    }

    #[test]
    fn prose_starts_a_turn_on_main_and_is_delivered_anywhere_else() {
        assert_eq!(
            route(
                compose(&composer("run the tests"), &known()),
                &on(ConvKey::Main, false)
            ),
            Decision::Turn {
                text: "run the tests".to_string()
            }
        );
        assert_eq!(
            route(
                compose(&composer("run the tests"), &known()),
                &on(ConvKey::Agent("scout".to_string()), false)
            ),
            Decision::Deliver {
                target: DirectTarget::Agent("scout".to_string()),
                text: "run the tests".to_string(),
                addressed: false,
            },
            "prose belongs to the conversation on screen"
        );
    }

    /// A direct send is somebody else's mail: main being busy is not its business.
    #[test]
    fn a_direct_send_never_waits_behind_mains_turn() {
        assert_eq!(
            route(
                compose(&composer("@scout use tabs"), &known()),
                &on(ConvKey::Main, true)
            ),
            Decision::Deliver {
                target: DirectTarget::Agent("scout".to_string()),
                text: "use tabs".to_string(),
                addressed: true,
            }
        );
    }

    /// The console's commands and its shell are the console's, wherever the
    /// screen is — and the page they were typed on travels with them (D135a).
    #[test]
    fn a_queued_command_keeps_the_page_it_was_typed_on() {
        let page = ConvKey::Agent("scout".to_string());
        match route(
            compose(&composer("/compact"), &known()),
            &on(page.clone(), true),
        ) {
            Decision::Queue(entry) => {
                assert_eq!(
                    entry.conversation,
                    ConvKey::Main,
                    "it is the console's queue"
                );
                assert_eq!(entry.origin, page, "and it runs where it was typed");
                assert_eq!(entry.kind, QueuedKind::Command);
                assert_eq!(entry.text, "/compact", "the slash travels with it");
            }
            other => panic!("expected a queue entry, got {other:?}"),
        }
    }

    /// An instant command applies while a turn runs rather than joining the queue
    /// behind it.
    #[test]
    fn an_instant_command_bypasses_the_queue() {
        assert_eq!(
            route(
                compose(&composer("/think xhigh"), &known()),
                &on(ConvKey::Main, true)
            ),
            Decision::Command {
                line: "think xhigh".to_string(),
                on: ConvKey::Main,
            }
        );
    }

    #[test]
    fn shell_waits_behind_the_console_and_keeps_its_prefix() {
        let shell = Submission::Composer {
            mode: ComposerMode::Shell,
            text: "cargo test".to_string(),
            attachments: Vec::new(),
        };
        assert_eq!(
            route(compose(&shell, &known()), &on(ConvKey::Main, false)),
            Decision::Shell {
                command: "cargo test".to_string()
            }
        );
        match route(
            compose(&shell, &known()),
            &on(ConvKey::Agent("scout".to_string()), true),
        ) {
            Decision::Queue(entry) => {
                assert_eq!(entry.text, "cargo test");
                assert_eq!(
                    entry.kind,
                    QueuedKind::Shell,
                    "a drained shell line still runs"
                );
            }
            other => panic!("expected a queue entry, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_line_submits_nothing() {
        assert_eq!(
            route(
                compose(&composer("   \n "), &known()),
                &on(ConvKey::Main, false)
            ),
            Decision::Nothing
        );
    }
}

/// The submission path as a client reaches it: one request, one disposition, and
/// a queue entry that keeps the page it was submitted on.
#[cfg(test)]
mod actor_tests {
    use super::*;
    use crate::app::command::{AppCommand, AppQuery, SubmitDisposition};
    use crate::app::event::AppEventPayload;
    use crate::app::ids::ConversationId;
    use crate::app::snapshot::TurnOrigin;
    use crate::app::{
        AppCore, AppError, AppFrame, AppLink, AppReply, AppRequest, AttachRequest, RequestId,
        SessionSetup,
    };

    async fn attached(core: &AppCore) -> AppLink {
        let mut link = core
            .attach(AttachRequest::new("test"))
            .unwrap_or_else(|error| panic!("{error}"));
        link.request(AppRequest::Query {
            id: RequestId(1),
            query: AppQuery::ReadSession,
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

    /// A session that cannot run refuses to pretend it opened a turn.
    ///
    /// This is the whole difference an engine makes, and it is worth its own
    /// test: the core answers everything else out of its own state, so "no
    /// engine" has to be visible exactly here and nowhere else.
    #[tokio::test]
    async fn a_core_with_no_engine_refuses_the_run_rather_than_opening_it() {
        let core = AppCore::start(SessionSetup::default());
        let mut link = attached(&core).await;
        assert_eq!(
            submit(&mut link, 2, "run the tests").await,
            Err(AppError::Refused(
                crate::app_server::protocol::error::ProtocolErrorKind::ActionUnavailable
            ))
        );
        assert!(
            !core.turns().view().is_busy(&ConvKey::Main),
            "a refused submission leaves no turn behind"
        );
    }

    /// Idle main runs the prose; busy main queues it. The caller asks the same
    /// question both times and the core answers differently, which is the point.
    #[tokio::test]
    async fn a_submission_starts_work_when_idle_and_queues_while_busy() {
        let core = AppCore::start(SessionSetup::default());
        let engine = std::sync::Arc::new(crate::app::engine::Recorder::default());
        core.attach_engine(engine.clone());
        let mut link = attached(&core).await;
        match submit(&mut link, 2, "run the tests").await {
            Ok(AppReply::Submitted(SubmitDisposition::TurnStarted { turn_id })) => {
                assert_eq!(
                    engine.taken(),
                    vec![crate::app::engine::Run::Turn {
                        turn: turn_id,
                        text: "run the tests".to_string(),
                    }],
                    "the turn the reply names is the turn the engine was handed"
                );
            }
            other => panic!("idle main runs the prose, got {other:?}"),
        }
        core.turns().close(
            core.turns()
                .view()
                .active(&ConvKey::Main)
                .cloned()
                .unwrap_or_else(|| panic!("the turn is running")),
            crate::app::snapshot::TurnStatus::Completed,
            None,
        );
        // The close is a message on the same queue; the next `open` is behind it.
        let turn = core
            .turns()
            .open(ConvKey::Main, TurnOrigin::User, Vec::new())
            .await
            .unwrap_or_else(|| panic!("main was idle"));
        match submit(&mut link, 3, "and rename it").await {
            Ok(AppReply::Submitted(SubmitDisposition::Queued {
                position,
                steer_eligible,
                ..
            })) => {
                assert_eq!(position, 0);
                assert!(steer_eligible, "plain prose may ride along at a barrier");
            }
            other => panic!("expected a queued disposition, got {other:?}"),
        }
        core.turns()
            .close(turn, crate::app::snapshot::TurnStatus::Completed, None);
    }

    /// A submission on an unknown conversation is refused by name rather than
    /// routed to whatever the core happens to have open.
    #[tokio::test]
    async fn a_submission_names_the_conversation_it_belongs_to() {
        let core = AppCore::start(SessionSetup::default());
        let mut link = attached(&core).await;
        link.request(AppRequest::Command {
            id: RequestId(9),
            command: AppCommand::Submit {
                conversation_id: ConversationId::new("conv_404"),
                input: Submission::SendProse {
                    text: "hello".to_string(),
                    attachments: Vec::new(),
                },
            },
        })
        .unwrap_or_else(|error| panic!("{error}"));
        match link.recv().await {
            Some(AppFrame::Reply { result, .. }) => assert_eq!(
                result,
                Err(AppError::Refused(
                    crate::app_server::protocol::error::ProtocolErrorKind::ConversationNotFound
                ))
            ),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// The reply is written before the event the request caused (invariant #3),
    /// and the queue event names the page the entry was submitted on.
    #[tokio::test]
    async fn a_queued_entry_is_announced_after_the_reply_that_accepted_it() {
        let core = AppCore::start(SessionSetup::default());
        let mut link = attached(&core).await;
        let turn = core
            .turns()
            .open(ConvKey::Main, TurnOrigin::User, Vec::new())
            .await
            .unwrap_or_else(|| panic!("main was idle"));
        // The turn's own `turn/started` is already on its way, with the summary
        // its start moved; the ordering this test is about is the submission's.
        match link.recv().await {
            Some(AppFrame::Event(event)) => {
                assert!(matches!(event.payload, AppEventPayload::TurnStarted(_)));
            }
            other => panic!("expected the turn to be announced, got {other:?}"),
        }
        link.request(AppRequest::Command {
            id: RequestId(4),
            command: AppCommand::Submit {
                conversation_id: ConversationId::new("conv_1"),
                input: Submission::Composer {
                    mode: ComposerMode::Normal,
                    text: "/compact".to_string(),
                    attachments: Vec::new(),
                },
            },
        })
        .unwrap_or_else(|error| panic!("{error}"));

        let queue_id = loop {
            match link.recv().await {
                Some(AppFrame::Reply {
                    result:
                        Ok(AppReply::Submitted(SubmitDisposition::Queued {
                            queue_id,
                            steer_eligible,
                            ..
                        })),
                    ..
                }) => {
                    assert!(!steer_eligible, "a command cannot travel to a running turn");
                    break queue_id;
                }
                // Whatever the turn's own start left in flight is not this
                // request's; what must not appear before the reply is the queue
                // event the request caused.
                Some(AppFrame::Event(event)) => assert!(
                    !matches!(event.payload, AppEventPayload::QueueItemAdded(_)),
                    "the reply comes before the event it caused"
                ),
                other => panic!("the reply comes first, got {other:?}"),
            }
        };
        // The queue event follows the reply. A conversation summary may follow
        // it in the same batch — the queue moving is a change to the summary —
        // so the assertion is on order, not on adjacency.
        loop {
            match link.recv().await {
                Some(AppFrame::Event(event)) => match &event.payload {
                    AppEventPayload::QueueItemAdded(added) => {
                        assert_eq!(added.entry.id, queue_id);
                        assert_eq!(
                            added.entry.origin_conversation_id,
                            ConversationId::new("conv_1"),
                            "the entry keeps the page it was submitted on"
                        );
                        break;
                    }
                    AppEventPayload::ConversationUpdated(_)
                    | AppEventPayload::ConversationCreated(_) => {}
                    other => panic!("expected the queue event, got {other:?}"),
                },
                other => panic!("expected an event after the reply, got {other:?}"),
            }
        }
        core.turns()
            .close(turn, crate::app::snapshot::TurnStatus::Completed, None);
    }
}
