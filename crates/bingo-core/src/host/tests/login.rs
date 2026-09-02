//! `/login` and `/logout` through a real host (ADR-0012 §5): a provider's
//! sign-in asks through the session's own dialog while no turn runs.

use std::time::Duration;

use super::commands::Client;
use super::*;

static SIGNING: PluginManifest = PluginManifest {
    id: "test.signing",
    version: "0",
    sdk: "^0.1",
    provides: &["provider:signing"],
    requires: &[],
    config: None,
};

/// A provider whose `Paste` sign-in waits for the pasted text and whose
/// browser sign-in finishes on its own, as a real loopback flow would.
struct Signing;

#[async_trait]
impl Provider for Signing {
    fn id(&self) -> &str {
        "signing"
    }

    fn endpoint(&self, _model: &str) -> EndpointCapabilities {
        EndpointCapabilities::default()
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<ModelStream, ProviderError> {
        Err(ProviderError::Unsupported {
            message: "stream".into(),
        })
    }

    fn auth(&self) -> AuthStatus {
        AuthStatus::Ready
    }

    async fn login(
        &self,
        prompter: Arc<dyn Prompter>,
        method: Option<LoginMethod>,
    ) -> Result<String, ProviderError> {
        let cancelled = |message: String| ProviderError::Auth { message };
        if method == Some(LoginMethod::Paste) {
            let kind = InteractionKind::Login {
                provider: "signing".into(),
                flow: LoginFlow::Paste,
            };
            return match prompter
                .ask(kind, vec![AnswerSpec::Text, AnswerSpec::Cancel])
                .await
            {
                Ok(Answer::Text { text }) => Ok(format!("Signed in to signing as {text}.")),
                Ok(_) => Err(cancelled("Sign-in cancelled.".into())),
                Err(e) => Err(cancelled(e.message)),
            };
        }
        let kind = InteractionKind::Login {
            provider: "signing".into(),
            flow: LoginFlow::Browser {
                url: "http://localhost:1455/authorize".into(),
            },
        };
        tokio::select! {
            _ = prompter.ask(kind, vec![AnswerSpec::Cancel]) => Err(cancelled("Sign-in cancelled.".into())),
            _ = tokio::time::sleep(Duration::from_millis(50)) => Ok("Signed in to signing.".into()),
        }
    }

    async fn logout(&self) -> Result<String, ProviderError> {
        Ok("Signed out of signing.".into())
    }
}

async fn host_with_signing() -> Arc<Host> {
    let provider = ScriptedProvider::new(vec![Script::Events(text("hi"))]);
    let plugins = vec![
        TestPlugin::boxed(&PROVIDER, vec![Contribution::Provider(provider)]),
        TestPlugin::boxed(&SIGNING, vec![Contribution::Provider(Arc::new(Signing))]),
    ];
    let config = HostConfig::new(env()).with_layer("cli", json!({ "model": "m" }));
    Host::build(plugins, config).await.unwrap()
}

/// Submit a line and fold frames until its ack, answering the one
/// interaction it opens with `answer`; returns the ack and what came before.
async fn ack_answering(
    client: &mut Client,
    line: &str,
    answer: Option<Answer>,
) -> (IntentOutcome, Vec<Frame>) {
    let intent = IntentId::mint();
    client
        .handle
        .submit(intent.clone(), Input::text(line, Origin::surface("test")));
    let mut seen = Vec::new();
    while let Some(frame) = client.events.next().await {
        client.state.apply(&frame);
        match &frame.event {
            Event::IntentAck { intent: i, outcome } if i == &intent => {
                return (outcome.clone(), seen);
            }
            Event::InteractionOpened { interaction } => {
                if let Some(answer) = answer.clone() {
                    client.handle.answer(
                        IntentId::mint(),
                        interaction.id.clone(),
                        answer,
                        Activation::Programmatic,
                    );
                }
            }
            _ => {}
        }
        seen.push(frame);
    }
    panic!("the stream ended before the ack");
}

fn opened(frames: &[Frame]) -> &Interaction {
    frames
        .iter()
        .find_map(|f| match &f.event {
            Event::InteractionOpened { interaction } => Some(interaction),
            _ => None,
        })
        .expect("a sign-in dialog opened")
}

#[tokio::test]
async fn a_pasted_credential_signs_in_and_the_receipt_is_recorded() {
    let host = host_with_signing().await;
    let mut client = Client::open(&host).await;
    let (ack, before) = ack_answering(
        &mut client,
        "/login signing paste",
        Some(Answer::Text {
            text: "tok-1".into(),
        }),
    )
    .await;

    let dialog = opened(&before);
    assert_eq!(dialog.turn, None, "a command's question is under no turn");
    assert!(matches!(
        &dialog.kind,
        InteractionKind::Login { provider, flow: LoginFlow::Paste } if provider == "signing"
    ));
    let IntentOutcome::Applied { result } = ack else {
        panic!("not applied: {ack:?}");
    };
    let item = ItemId::from_raw(result["item"].as_str().unwrap());
    let recorded = client.state.items.iter().find(|i| i.id == item).unwrap();
    assert_eq!(
        recorded.body,
        ItemBody::Action {
            name: "login".into(),
            args: json!("signing"),
            result: Some(json!("Signed in to signing as tok-1.")),
        }
    );
    assert!(client.state.interactions.is_empty());
    assert_eq!(
        client.state.summary.provider.as_deref(),
        Some("scripted"),
        "signing in changes no session's provider"
    );
}

#[tokio::test]
async fn a_flow_that_finishes_on_its_own_closes_the_dialog_it_opened() {
    let host = host_with_signing().await;
    let mut client = Client::open(&host).await;
    let (ack, before) = ack_answering(&mut client, "/login signing", None).await;
    assert!(matches!(
        &opened(&before).kind,
        InteractionKind::Login {
            flow: LoginFlow::Browser { .. },
            ..
        }
    ));
    assert!(matches!(ack, IntentOutcome::Applied { .. }), "{ack:?}");
    assert!(
        before.iter().any(|f| matches!(
            f.event,
            Event::InteractionCancelled {
                reason: CancelReason::CommandEnded,
                ..
            }
        )),
        "the dialog is cancelled when the command that asked ends"
    );
    assert!(client.state.interactions.is_empty());
}

#[tokio::test]
async fn cancelling_the_dialog_fails_the_login_and_an_instant_command_does_not() {
    let host = host_with_signing().await;
    let mut client = Client::open(&host).await;
    let login = IntentId::mint();
    client.handle.submit(
        login.clone(),
        Input::text("/login signing paste", Origin::surface("test")),
    );
    let dialog = loop {
        let frame = client.events.next().await.unwrap();
        client.state.apply(&frame);
        if let Event::InteractionOpened { interaction } = frame.event {
            break interaction;
        }
    };

    // An instant command runs meanwhile and leaves the dialog alone.
    let (ack, _) = client.ack("/think high").await;
    assert!(matches!(ack, IntentOutcome::Applied { .. }));
    assert_eq!(client.state.interactions.len(), 1);

    client.handle.answer(
        IntentId::mint(),
        dialog.id,
        Answer::Cancel,
        Activation::Programmatic,
    );
    let ack = loop {
        let frame = client.events.next().await.unwrap();
        client.state.apply(&frame);
        if let Event::IntentAck { intent, outcome } = frame.event
            && intent == login
        {
            break outcome;
        }
    };
    assert!(
        matches!(&ack, IntentOutcome::Rejected { error } if error.code == ErrorCode::AuthRequired),
        "{ack:?}"
    );
    assert!(client.state.interactions.is_empty());
}

#[tokio::test]
async fn login_names_a_registered_provider_and_logout_answers_a_receipt() {
    let host = host_with_signing().await;
    let mut client = Client::open(&host).await;

    let (ack, _) = client.ack("/login nope").await;
    assert!(
        matches!(&ack, IntentOutcome::Rejected { error } if error.code == ErrorCode::ProviderUnavailable),
        "{ack:?}"
    );
    let (ack, _) = client.ack("/login scripted").await;
    assert!(
        matches!(&ack, IntentOutcome::Rejected { error } if error.code == ErrorCode::InvalidInput && error.message.contains("login")),
        "a provider without a sign-in says so: {ack:?}"
    );
    let (ack, _) = client.ack("/login signing sms").await;
    assert!(
        matches!(&ack, IntentOutcome::Rejected { error } if error.message.contains("sms")),
        "{ack:?}"
    );

    let (ack, _) = client.ack("/logout signing").await;
    let IntentOutcome::Applied { result } = ack else {
        panic!("not applied: {ack:?}");
    };
    let item = ItemId::from_raw(result["item"].as_str().unwrap());
    let recorded = client.state.items.iter().find(|i| i.id == item).unwrap();
    assert!(matches!(
        &recorded.body,
        ItemBody::Action { name, result: Some(receipt), .. }
            if name == "logout" && receipt == "Signed out of signing."
    ));
}
