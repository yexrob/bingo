//! The ask door (ADR-0039): who answers a question no tool defines.
//!
//! Nothing here waits on a clock. An interaction has no timeout — a gate
//! question with nobody attached waits exactly as long as its session lives —
//! so "unanswerable" is the policy's own refusing stance, and the questions
//! that end without a person end because a person interrupted or the session
//! closed under them.

use super::*;

static STANCE: PluginManifest = PluginManifest {
    id: "test.stance",
    version: "0",
    sdk: "^0.1",
    provides: &["policy:stance"],
    requires: &[],
    config: None,
};

/// Stands where it was told to, and asks about every call.
struct StancePolicy(Stance);

#[async_trait]
impl PermissionPolicy for StancePolicy {
    fn id(&self) -> &str {
        "stance"
    }
    async fn decide(&self, _: PolicyInput<'_>) -> Decision {
        Decision::Ask {
            reason: Reason::Default,
            scope: None,
        }
    }
    async fn stance(&self, _: &SessionId) -> Stance {
        self.0
    }
}

async fn standing(stance: Stance, scripts: Vec<Script>) -> Arc<Host> {
    let plugins = vec![
        TestPlugin::boxed(
            &PROVIDER,
            vec![Contribution::Provider(ScriptedProvider::new(scripts))],
        ),
        TestPlugin::boxed(
            &STANCE,
            vec![Contribution::Policy(Arc::new(StancePolicy(stance)))],
        ),
    ];
    let config = HostConfig::new(env()).with_layer("cli", json!({ "model": "m" }));
    Host::build(plugins, config).await.unwrap()
}

async fn attach(host: &Host) -> Attachment {
    host.open(
        SessionSelector::Create {
            spec: spec("/work"),
        },
        who(),
        OpenOptions::default(),
    )
    .await
    .unwrap()
}

fn option(id: &str, role: Option<AnswerRole>) -> QuestionOption {
    QuestionOption {
        id: id.into(),
        label: format!("the agent's word for {id}"),
        description: None,
        role,
        preview: None,
    }
}

/// An asker's own options, two of them marked: this is what a
/// `session/request_permission` becomes (ADR-0039 §3).
fn question(options: Vec<QuestionOption>) -> InteractionKind {
    InteractionKind::Question(Question {
        question: "may I write to /etc/hosts?".into(),
        header: Some("Permission".into()),
        options,
        free_text: false,
        multi: false,
    })
}

fn marked() -> InteractionKind {
    question(vec![
        option("allow_once", Some(AnswerRole::Allowing)),
        option("allow_always", None),
        option("reject_once", Some(AnswerRole::Refusing)),
    ])
}

fn answers() -> Vec<AnswerSpec> {
    vec![AnswerSpec::Choice, AnswerSpec::Cancel]
}

fn picked(id: &str) -> Answer {
    Answer::Choice {
        ids: vec![id.into()],
    }
}

/// Every frame the session published up to a mark the door cannot have
/// raced: the actor reads its mailbox in order, so anything the ask opened is
/// already here by the time the mark arrives.
async fn frames_to_the_mark(host: &Host, attachment: &mut Attachment) -> Vec<Event> {
    host.extend(&attachment.session, "test", "mark", json!(1))
        .await
        .unwrap();
    let mut seen = Vec::new();
    while let Some(frame) = attachment.events.next().await {
        let mark = matches!(&frame.event, Event::Extension { kind, .. } if kind == "mark");
        seen.push(frame.event);
        if mark {
            break;
        }
    }
    seen
}

/// The interaction the next question opens.
async fn opened(attachment: &mut Attachment) -> InteractionId {
    while let Some(frame) = attachment.events.next().await {
        if let Event::InteractionOpened { interaction } = frame.event {
            return interaction.id;
        }
    }
    panic!("no question was ever opened");
}

#[tokio::test]
async fn an_allowing_stance_answers_the_question_and_opens_nothing() {
    let host = standing(Stance::Allow, vec![]).await;
    let mut attachment = attach(&host).await;
    let answer = host
        .ask(&attachment.session.clone(), marked(), answers())
        .await
        .unwrap();
    assert_eq!(answer, picked("allow_once"));
    let frames = frames_to_the_mark(&host, &mut attachment).await;
    assert!(
        !frames
            .iter()
            .any(|e| matches!(e, Event::InteractionOpened { .. })),
        "nobody was asked, so no interaction was opened: {frames:?}"
    );
    assert!(
        !frames
            .iter()
            .any(|e| matches!(e, Event::ItemStarted { .. })),
        "and nothing was journaled for it: {frames:?}"
    );
}

#[tokio::test]
async fn a_refusing_stance_answers_with_the_fail_closed_option_at_once() {
    let host = standing(Stance::Refuse, vec![]).await;
    let mut attachment = attach(&host).await;
    let answer = host
        .ask(&attachment.session.clone(), marked(), answers())
        .await
        .unwrap();
    assert_eq!(answer, picked("reject_once"));
    let frames = frames_to_the_mark(&host, &mut attachment).await;
    assert!(
        !frames
            .iter()
            .any(|e| matches!(e, Event::InteractionOpened { .. })),
        "a session with nobody at it asks nobody: {frames:?}"
    );
}

#[tokio::test]
async fn a_question_between_turns_reaches_the_person_and_their_answer_comes_back() {
    let host = standing(Stance::Ask, vec![]).await;
    let mut attachment = attach(&host).await;
    let session = attachment.session.clone();
    let asking = tokio::spawn({
        let host = Arc::clone(&host);
        async move { host.ask(&session, marked(), answers()).await }
    });

    let id = opened(&mut attachment).await;
    attachment.handle.answer(
        IntentId::mint(),
        id,
        picked("allow_always"),
        Activation::Pointer,
    );
    assert_eq!(
        asking.await.unwrap().unwrap(),
        picked("allow_always"),
        "the person's own choice, whichever option it was"
    );
}

#[tokio::test]
async fn a_question_under_a_running_turn_is_refused_when_the_turn_is_interrupted() {
    let host = standing(Stance::Ask, vec![Script::Hang(vec![])]).await;
    let mut attachment = attach(&host).await;
    let session = attachment.session.clone();
    attachment
        .handle
        .submit(IntentId::mint(), Input::text("hi", Origin::surface("test")));
    while let Some(frame) = attachment.events.next().await {
        if matches!(frame.event, Event::TurnStarted { .. }) {
            break;
        }
    }

    let asking = tokio::spawn({
        let host = Arc::clone(&host);
        async move { host.ask(&session, marked(), answers()).await }
    });
    opened(&mut attachment).await;
    attachment
        .handle
        .interrupt(IntentId::mint(), InterruptScope::Head);
    assert_eq!(
        asking.await.unwrap().unwrap(),
        picked("reject_once"),
        "the asker hears the refusal, not an error it would have to read as one"
    );
}

#[tokio::test]
async fn a_question_the_session_closes_under_is_refused_rather_than_left_waiting() {
    let host = standing(Stance::Ask, vec![]).await;
    let mut attachment = attach(&host).await;
    let session = attachment.session.clone();
    let asking = tokio::spawn({
        let host = Arc::clone(&host);
        let session = session.clone();
        async move { host.ask(&session, marked(), answers()).await }
    });
    opened(&mut attachment).await;
    host.delete(&session).await.unwrap();
    assert_eq!(asking.await.unwrap().unwrap(), picked("reject_once"));
}

#[tokio::test]
async fn a_question_that_marks_no_option_is_never_answered_for_the_person() {
    let host = standing(Stance::Allow, vec![]).await;
    let attachment = attach(&host).await;
    let unmarked = question(vec![option("allow_once", None)]);
    let error = host
        .ask(&attachment.session, unmarked, answers())
        .await
        .expect_err("a question with nothing to answer it by");
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(error.message.contains("allowing"), "{}", error.message);
}

#[tokio::test]
async fn a_session_this_host_does_not_run_is_never_answered_for() {
    let host = standing(Stance::Allow, vec![]).await;
    let error = host
        .ask(&SessionId::from_raw("ses_nowhere"), marked(), answers())
        .await
        .expect_err("no such session");
    assert_eq!(error.code, ErrorCode::SessionNotFound);
}
