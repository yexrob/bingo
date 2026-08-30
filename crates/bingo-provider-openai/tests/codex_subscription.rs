//! The ChatGPT subscription end of the provider (ADR-0012 §6): the bearer
//! comes out of a credential store, a refusal renews it once, and the model
//! menu survives a catalogue that will not answer. Against a local mock; no
//! live network call is made here, and there is no ChatGPT account in this
//! workspace to make one against.

// An integration test is not `cfg(test)`; the test-only lint relief is spelled
// out, the way `crates/bingo/tests/cli.rs` spells it out.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use bingo_auth_oauth::tokens::unix_now;
use bingo_auth_oauth::{CredentialStore, Entry};
use bingo_provider_openai::{CodexConfig, OpenAiProvider};
use bingo_sdk::{
    AuthStatus, CancellationToken, Message, ModelEvent, ModelRequest, Provider, ProviderError, Role,
};
use futures::StreamExt;
use serde_json::json;
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> Vec<u8> {
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    std::fs::read(&file).unwrap_or_else(|e| panic!("read {}: {e}", file.display()))
}

fn request() -> ModelRequest {
    ModelRequest {
        model: "gpt-5.4".into(),
        max_tokens: 1024,
        system: Vec::new(),
        messages: vec![Message::text(Role::User, "hello")],
        tools: Vec::new(),
        reasoning: None,
        provider_options: Default::default(),
    }
}

/// A signed-in `auth.json` in a temporary directory, and the provider that
/// reads it. Both the endpoint and the issuer are the mock.
fn signed_in(server: &MockServer, directory: &TempDir, access: &str) -> OpenAiProvider {
    let store = Arc::new(CredentialStore::new(directory.path().to_path_buf()));
    store
        .write(
            "codex",
            Entry::OAuth {
                access: access.into(),
                refresh: "rt-1".into(),
                expires: unix_now() + 3_600,
                account_id: Some("acc_1".into()),
            },
        )
        .expect("a write");
    OpenAiProvider::codex(
        CodexConfig {
            base_url: Some(server.uri()),
            issuer: Some(server.uri()),
        },
        store,
    )
}

async fn serve_refresh(server: &MockServer, access: &str) {
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": access,
            "expires_in": 3600,
        })))
        .expect(1)
        .mount(server)
        .await;
}

async fn turn(provider: &OpenAiProvider) -> Result<Vec<ModelEvent>, ProviderError> {
    let stream = provider.stream(request(), CancellationToken::new()).await?;
    stream.collect::<Vec<_>>().await.into_iter().collect()
}

#[tokio::test]
async fn a_refused_request_is_retried_once_with_a_renewed_bearer() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("a temporary directory");
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer at-stale"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "message": "token expired", "code": "invalid_api_key" }
        })))
        .expect(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer at-renewed"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(fixture("text.sse"), "text/event-stream"),
        )
        .expect(1)
        .with_priority(2)
        .mount(&server)
        .await;
    serve_refresh(&server, "at-renewed").await;

    let provider = signed_in(&server, &directory, "at-stale");
    let events = turn(&provider).await.expect("the retried turn completes");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ModelEvent::TextDelta { .. })),
        "the second attempt streamed a body: {events:?}"
    );
    assert_eq!(provider.auth(), AuthStatus::Ready);
}

#[tokio::test]
async fn a_second_refusal_asks_the_person_to_sign_in_again() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("a temporary directory");
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "message": "still refused" }
        })))
        .expect(2)
        .mount(&server)
        .await;
    serve_refresh(&server, "at-renewed").await;

    let error = turn(&signed_in(&server, &directory, "at-stale"))
        .await
        .expect_err("a second refusal");
    assert_eq!(
        error,
        ProviderError::Auth {
            message: "Run `bingo login codex` to sign in again.".into()
        },
        "the way back is named once, not the endpoint's words twice"
    );
}

#[tokio::test]
async fn the_model_menu_is_the_subscriptions_own_catalogue() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("a temporary directory");
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .and(header("authorization", "Bearer at-1"))
        .and(header("originator", "bingo"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(fixture("codex_models.json"), "application/json"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let models = signed_in(&server, &directory, "at-1")
        .models()
        .await
        .expect("a catalogue");
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        [
            "gpt-5.6-sol",
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.3-codex-spark",
        ]
    );
    assert_eq!(models[0].display.as_deref(), Some("GPT-5.6 Sol"));
}

#[tokio::test]
async fn a_catalogue_that_will_not_answer_leaves_the_static_list() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("a temporary directory");
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let models = signed_in(&server, &directory, "at-1")
        .models()
        .await
        .expect("the static list rather than a failure");
    assert_eq!(models.len(), 9);
    assert_eq!(models[0].id, "gpt-5.6-sol");
    assert_eq!(models[8].id, "codex-auto-review");
}

#[tokio::test]
async fn a_signed_out_subscription_names_the_command_before_any_request() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("a temporary directory");
    let provider = OpenAiProvider::codex(
        CodexConfig {
            base_url: Some(server.uri()),
            issuer: Some(server.uri()),
        },
        Arc::new(CredentialStore::new(directory.path().to_path_buf())),
    );
    assert_eq!(
        provider.auth(),
        AuthStatus::Missing {
            hint: "Run `bingo login codex`, or `/login codex` in a session.".into()
        }
    );
    assert_eq!(
        turn(&provider).await.expect_err("no credential"),
        ProviderError::Auth {
            message: "Run `bingo login codex`, or `/login codex` in a session.".into()
        }
    );
    assert!(
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "nothing reached the wire"
    );
}
