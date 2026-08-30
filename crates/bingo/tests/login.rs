//! Black-box: `/login codex paste` over JSON-RPC (ADR-0012 §5). The sign-in
//! is a dialog like any other; the pasted credential is stored, and the
//! same process can switch to the provider at once.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use bingo_sdk::{
    Activation, Answer, AnswerSpec, Event, HostApi, Input, IntentId, IntentOutcome,
    InteractionKind, LoginFlow, OpenOptions, Origin,
};
use futures::StreamExt;

mod support;

use support::{LIMIT, Server, ack_for, create, ready, who};

const IDLE: &str = r#"{"responses":[{"steps":[{"text":"unused"}]}]}"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_pasted_credential_is_stored_and_the_provider_is_usable_at_once() {
    let scratch = tempfile::tempdir().unwrap();
    let settings = scratch.path().join("settings.json");
    std::fs::write(
        &settings,
        r#"{ "codex": { "baseUrl": "http://127.0.0.1:9", "issuer": "http://127.0.0.1:9" } }"#,
    )
    .unwrap();
    let mut server = Server::spawn_with(IDLE, &["--settings", settings.to_str().unwrap()]);
    let kernel = ready(&mut server).await;
    let mut session = kernel
        .open(create(server.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();

    let login = IntentId::mint();
    session.handle.submit(
        login.clone(),
        Input::text("/login codex paste", Origin::surface("test")),
    );
    let dialog = {
        let deadline = tokio::time::sleep(LIMIT);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                frame = session.events.next() => {
                    let frame = frame.expect("the stream stays open");
                    session.snapshot.apply(&frame);
                    if let Event::InteractionOpened { interaction } = frame.event {
                        break interaction;
                    }
                }
                _ = &mut deadline => panic!("no sign-in dialog opened"),
            }
        }
    };
    assert!(matches!(
        &dialog.kind,
        InteractionKind::Login { provider, flow: LoginFlow::Paste } if provider == "codex"
    ));
    assert_eq!(dialog.turn, None);
    assert_eq!(dialog.answers, vec![AnswerSpec::Text, AnswerSpec::Cancel]);

    session.handle.answer(
        IntentId::mint(),
        dialog.id,
        Answer::Text {
            text: "sk-pasted".into(),
        },
        Activation::Programmatic,
    );
    let IntentOutcome::Applied { result } = ack_for(&mut session, &login).await else {
        panic!("the sign-in is applied");
    };
    assert!(result["item"].is_string(), "{result}");
    let stored: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(server.cwd().join(".bingo/data/auth.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(stored["codex"]["type"], "api");
    assert_eq!(stored["codex"]["key"], "sk-pasted");

    // The same process sees the credential: the session may move to codex.
    let switch = IntentId::mint();
    session.handle.submit(
        switch.clone(),
        Input::text("/model codex/gpt-5.4", Origin::surface("test")),
    );
    let IntentOutcome::Applied { result } = ack_for(&mut session, &switch).await else {
        panic!("the switch is applied");
    };
    assert_eq!(result["message"], "model: codex/gpt-5.4");
}
