use serde_json::json;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;
use crate::conversation::Conversation;
use crate::question::Choice;
use bingo_sdk::{Answer, InteractionId};

async fn ok(server: &MockServer, verb: &str, at: &str, data: serde_json::Value) {
    Mock::given(method(verb))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "code": 0, "data": data })))
        .mount(server)
        .await;
}

async fn signed_in(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/open-apis/auth/v3/tenant_access_token/internal"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0, "tenant_access_token": "t-1", "expire": 7200,
        })))
        .mount(server)
        .await;
}

async fn feishu(server: &MockServer) -> Feishu {
    signed_in(server).await;
    Feishu::new(Config {
        app_id: "cli_a".into(),
        app_secret: "secret".into(),
        base: server.uri(),
    })
}

async fn bodies(server: &MockServer, at: &str) -> Vec<serde_json::Value> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request: &&Request| request.url.path() == at)
        .filter_map(|request| serde_json::from_slice(&request.body).ok())
        .collect()
}

fn question() -> Question {
    Question {
        id: InteractionId::from_raw("int_1"),
        prompt: "Bash: run `cargo test`".into(),
        choices: vec![Choice {
            key: "1".into(),
            label: "Allow once".into(),
            answer: Answer::AllowOnce,
        }],
        free_text: false,
        rest: None,
    }
}

#[tokio::test]
async fn the_capabilities_are_the_ones_this_platform_really_has() {
    let server = MockServer::start().await;
    let feishu = feishu(&server).await;
    assert!(feishu.edit().is_some(), "cardkit, not the 20-edit cap");
    assert!(feishu.buttons().is_some());
    assert!(feishu.threads().is_some());
    assert!(
        feishu.typing().is_none(),
        "a card writing itself is the affordance"
    );
    assert_eq!(feishu.credential(), "cli_a", "public, never the secret");
    assert_eq!(feishu.limits().max_text.1, Encoding::Utf8Bytes);
}

#[tokio::test]
async fn a_channel_with_no_credential_refuses_before_it_dials() {
    let bare = Feishu::new(Config {
        app_id: "cli_a".into(),
        app_secret: String::new(),
        base: "http://127.0.0.1:1".into(),
    });
    let (post, _arrivals) = tokio::sync::mpsc::channel(1);
    let error = bare
        .run(
            Inbox::new(Feishu::ID, post),
            bingo_sdk::CancellationToken::new(),
        )
        .await
        .expect_err("a refusal");
    assert!(matches!(error, ChannelError::Refused(_)), "{error}");
    assert!(
        error.to_string().contains("BINGO_FEISHU_APP_SECRET"),
        "{error}"
    );
}

#[tokio::test]
async fn a_plain_message_goes_to_the_chat_as_text() {
    let server = MockServer::start().await;
    let feishu = feishu(&server).await;
    Mock::given(method("POST"))
        .and(path(MESSAGES))
        .and(query_param("receive_id_type", "chat_id"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "code": 0, "data": { "message_id": "om_1" } })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let posted = feishu
        .send(
            &Conversation::direct("oc_1"),
            "two tests failed",
            Mode::Once,
        )
        .await
        .expect("a message");
    assert_eq!(Handle::of(&posted), Some(Handle::Message("om_1".into())));
    assert_eq!(
        bodies(&server, MESSAGES).await[0],
        json!({
            "receive_id": "oc_1",
            "msg_type": "text",
            "content": r#"{"text":"two tests failed"}"#,
        })
    );
}

#[tokio::test]
async fn a_streamed_answer_is_a_card_entity_sent_by_id_and_written_in_full() {
    let server = MockServer::start().await;
    let feishu = feishu(&server).await;
    ok(&server, "POST", CARDS, json!({ "card_id": "ctp_1" })).await;
    ok(&server, "POST", MESSAGES, json!({ "message_id": "om_1" })).await;
    let element = format!("{CARDS}/ctp_1/elements/{}/content", card::ANSWER);
    ok(&server, "PUT", &element, json!({})).await;
    ok(
        &server,
        "PATCH",
        &format!("{CARDS}/ctp_1/settings"),
        json!({}),
    )
    .await;

    let to = Conversation::direct("oc_1");
    let posted = feishu.send(&to, "", Mode::Stream).await.expect("a card");
    assert_eq!(Handle::of(&posted), Some(Handle::Card("ctp_1".into())));
    assert_eq!(
        bodies(&server, MESSAGES).await[0]["content"],
        json!(r#"{"type":"card","data":{"card_id":"ctp_1"}}"#),
        "the card is sent by id, not written out"
    );

    let edit = feishu.edit().expect("an editor");
    edit.replace(&posted, "Two").await.expect("a write");
    edit.replace(&posted, "Two tests").await.expect("a write");
    edit.finish(&posted, "Two tests failed.")
        .await
        .expect("a finish");

    let writes = bodies(&server, &element).await;
    assert_eq!(
        writes
            .iter()
            .map(|body| (body["content"].clone(), body["sequence"].clone()))
            .collect::<Vec<_>>(),
        [
            (json!("Two"), json!(1)),
            (json!("Two tests"), json!(2)),
            (json!("Two tests failed."), json!(3)),
        ],
        "the whole text every time, under a sequence that only goes up"
    );
    assert_eq!(
        writes[0]["uuid"],
        json!("ctp_1-1"),
        "idempotent per sequence"
    );
    assert_eq!(
        bodies(&server, &format!("{CARDS}/ctp_1/settings")).await[0]["settings"],
        json!({ "config": { "streaming_mode": false } }),
        "the stream is closed, which is what re-opens the card to callbacks"
    );
}

#[tokio::test]
async fn a_rate_limited_frame_is_dropped_and_the_stream_carries_on() {
    let server = MockServer::start().await;
    let feishu = feishu(&server).await;
    ok(&server, "POST", CARDS, json!({ "card_id": "ctp_1" })).await;
    ok(&server, "POST", MESSAGES, json!({ "message_id": "om_1" })).await;
    let element = format!("{CARDS}/ctp_1/elements/{}/content", card::ANSWER);
    Mock::given(method("PUT"))
        .and(path(&element))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "code": 99_991_400, "msg": "too many requests" })),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(&element))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "code": 0 })))
        .with_priority(2)
        .mount(&server)
        .await;

    let posted = feishu
        .send(&Conversation::direct("oc_1"), "", Mode::Stream)
        .await
        .expect("a card");
    let edit = feishu.edit().expect("an editor");
    edit.replace(&posted, "Two")
        .await
        .expect("dropped, not failed");
    edit.replace(&posted, "Two tests").await.expect("a write");
    let writes = bodies(&server, &element).await;
    assert_eq!(
        writes[1]["sequence"],
        json!(2),
        "a spent sequence is never rewound, even by a failure"
    );
}

#[tokio::test]
async fn a_question_is_its_own_card_and_settling_it_edits_that_message() {
    let server = MockServer::start().await;
    let feishu = feishu(&server).await;
    ok(&server, "POST", MESSAGES, json!({ "message_id": "om_2" })).await;
    ok(&server, "PATCH", &format!("{MESSAGES}/om_2"), json!({})).await;

    let to = Conversation::direct("oc_1");
    let buttons = feishu.buttons().expect("buttons");
    let posted = buttons.ask(&to, &question()).await.expect("a card");
    assert_eq!(Handle::of(&posted), Some(Handle::Message("om_2".into())));

    let sent: serde_json::Value = serde_json::from_str(
        bodies(&server, MESSAGES).await[0]["content"]
            .as_str()
            .unwrap(),
    )
    .expect("the card");
    assert_eq!(
        sent["body"]["elements"][1]["actions"][0]["behaviors"][0]["type"],
        json!("callback"),
        "a button is a callback: {sent}"
    );

    buttons
        .settle(&posted, &question(), "approved in the TUI")
        .await
        .expect("a settle");
    let settled: serde_json::Value = serde_json::from_str(
        bodies(&server, &format!("{MESSAGES}/om_2")).await[0]["content"]
            .as_str()
            .unwrap(),
    )
    .expect("the card");
    let elements = settled["body"]["elements"].as_array().expect("elements");
    assert!(
        elements.iter().all(|e| e["tag"] != json!("action")),
        "no live button outlives its question: {settled}"
    );
    assert!(
        settled.to_string().contains("approved in the TUI"),
        "{settled}"
    );
}

#[tokio::test]
async fn a_card_cannot_be_settled_and_a_message_cannot_be_streamed_into() {
    let server = MockServer::start().await;
    let feishu = feishu(&server).await;
    let card = Handle::Card("ctp_1".into()).posted();
    let message = Handle::Message("om_1".into()).posted();
    assert!(matches!(
        feishu
            .buttons()
            .expect("buttons")
            .settle(&card, &question(), "x")
            .await,
        Err(ChannelError::Unsupported(_))
    ));
    assert!(matches!(
        feishu
            .edit()
            .expect("an editor")
            .replace(&message, "x")
            .await,
        Err(ChannelError::Unsupported(_))
    ));
}

#[tokio::test]
async fn a_reply_hangs_under_the_message_that_started_it() {
    let server = MockServer::start().await;
    let feishu = feishu(&server).await;
    let reply_path = format!("{MESSAGES}/om_parent/reply");
    ok(
        &server,
        "POST",
        &reply_path,
        json!({ "message_id": "om_3" }),
    )
    .await;
    let posted = feishu
        .threads()
        .expect("threads")
        .reply(
            &Conversation::group("oc_1"),
            &Handle::Message("om_parent".into()).posted(),
            "under it",
            Mode::Once,
        )
        .await
        .expect("a reply");
    assert_eq!(Handle::of(&posted), Some(Handle::Message("om_3".into())));
    assert_eq!(
        bodies(&server, &reply_path).await[0]["msg_type"],
        json!("text")
    );
}

#[tokio::test]
async fn the_bot_asks_who_it_is_before_it_listens() {
    let server = MockServer::start().await;
    let feishu = feishu(&server).await;
    Mock::given(method("GET"))
        .and(path(WHOAMI))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "code": 0, "bot": { "open_id": "ou_bot" } })),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(feishu.whoami().await.expect("an open id"), "ou_bot");
}

#[tokio::test]
async fn the_bootstrap_body_is_the_pascal_case_one() {
    let server = MockServer::start().await;
    let feishu = feishu(&server).await;
    Mock::given(method("POST"))
        .and(path(bootstrap::ENDPOINT))
        .and(body_json(json!({
            "AppID": "cli_a", "AppSecret": "secret", "ClientAssertion": "",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": { "URL": "wss://example.invalid/x", "ClientConfig": {} },
        })))
        .expect(1)
        .mount(&server)
        .await;
    let (url, _) = feishu.api.endpoint("secret").await.expect("an endpoint");
    assert_eq!(url, "wss://example.invalid/x");
}
