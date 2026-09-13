//! The messages a merged forward carried (ADR-0051 §1).
//!
//! A `merge_forward` arrives as a title and a promise. `GET
//! /open-apis/im/v1/messages/:id` is where the promise is kept: `data.items`
//! is the merge_forward message itself followed by its children, each with its
//! own `msg_type`, `body.content` and `sender`
//! (open.feishu.cn, 获取指定消息的内容, `im-v1/message/get`).
//!
//! The endpoint needs `im:message`, a scope some tenants will not grant, so a
//! refusal is a warning and the title stands on its own. What the children
//! carried besides words is left where it is: a forward is context, and
//! fetching a day of somebody else's attachments is not what was asked for.

use serde_json::Value;

use super::api::Api;
use super::content;

/// The most merged messages that are read. A bundle can hold a whole day of a
/// chat, and what the person meant by forwarding it is at the top.
const MOST: usize = 50;

/// What the merged messages are put under, so the words above them stay the
/// person's own.
const HEADING: &str = "--- forwarded ---";

/// The bundle `id` forwarded, as lines, or nothing where it could not be read.
pub async fn lines(api: &Api, id: &str, me: &str) -> Option<String> {
    let answer = match api.get(&format!("/open-apis/im/v1/messages/{id}")).await {
        Ok(answer) => answer,
        Err(why) => {
            tracing::warn!(%id, %why, "the merged messages could not be read");
            return None;
        }
    };
    let lines: Vec<String> = answer["data"]["items"]
        .as_array()?
        .iter()
        // The bundle answers with itself at the head of its own children.
        .filter(|item| item["message_id"].as_str() != Some(id))
        .take(MOST)
        .map(|item| line(item, me))
        .collect();
    (!lines.is_empty()).then(|| format!("{HEADING}\n{}", lines.join("\n")))
}

/// One merged message: who said it, and what they said, read by the same
/// normaliser as a message that arrived on its own. It is one line, because a
/// forward is a list and a paragraph inside a list item is a list no longer.
fn line(item: &Value, me: &str) -> String {
    let spoken = content::spoken(
        item["message_id"].as_str().unwrap_or_default(),
        item["msg_type"].as_str().unwrap_or_default(),
        item["body"]["content"].as_str().unwrap_or_default(),
        &item["mentions"],
        me,
    );
    let said = spoken.text.replace('\n', " ");
    match item["sender"]["id"].as_str().filter(|id| !id.is_empty()) {
        Some(who) => format!("- {who}: {said}"),
        None => format!("- {said}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const ME: &str = "ou_bot";
    const BUNDLE: &str = "om_bundle";

    async fn api(server: &MockServer) -> Api {
        Mock::given(method("POST"))
            .and(path("/open-apis/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "tenant_access_token": "t-1", "expire": 7200,
            })))
            .mount(server)
            .await;
        Api::new(server.uri(), "cli_1", "secret")
    }

    async fn answering(server: &MockServer, response: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path(format!("/open-apis/im/v1/messages/{BUNDLE}")))
            .respond_with(response)
            .mount(server)
            .await;
    }

    /// One item as `im/v1/messages` answers with it: the body is a JSON string
    /// inside the JSON, exactly as an event carries it.
    fn item(id: &str, sender: &str, msg_type: &str, content: Value) -> Value {
        json!({
            "message_id": id,
            "upper_message_id": BUNDLE,
            "msg_type": msg_type,
            "create_time": "1757000000000",
            "sender": { "id": sender, "id_type": "open_id", "sender_type": "user" },
            "body": { "content": content.to_string() },
        })
    }

    #[tokio::test]
    async fn a_bundle_is_read_by_the_same_normaliser_as_a_message_of_its_own() {
        let server = MockServer::start().await;
        let api = api(&server).await;
        answering(
            &server,
            ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "data": { "items": [
                    item(BUNDLE, "ou_a", "merge_forward", json!({ "title": "Tuesday" })),
                    item("om_1", "ou_a", "text", json!({ "text": "first" })),
                    item("om_2", "ou_b", "post", json!({
                        "title": "T",
                        "content": [[{ "tag": "text", "text": "second" }], [{ "tag": "text", "text": "and more" }]],
                    })),
                    item("om_3", "ou_c", "sticker", json!({ "file_key": "s" })),
                ] },
            })),
        )
        .await;
        assert_eq!(
            lines(&api, BUNDLE, ME).await.expect("a bundle"),
            "--- forwarded ---\n\
             - ou_a: first\n\
             - ou_b: T second and more\n\
             - ou_c: [sticker]",
            "the bundle answers with itself at the head of its own children"
        );
    }

    #[tokio::test]
    async fn a_bundle_bigger_than_anybody_meant_to_forward_is_read_down_to_its_top() {
        let server = MockServer::start().await;
        let api = api(&server).await;
        let items: Value = (0..MOST + 10)
            .map(|n| item(&format!("om_{n}"), "ou_a", "text", json!({ "text": n })))
            .collect();
        answering(
            &server,
            ResponseTemplate::new(200)
                .set_body_json(json!({ "code": 0, "data": { "items": items } })),
        )
        .await;
        let bundle = lines(&api, BUNDLE, ME).await.expect("a bundle");
        assert_eq!(bundle.lines().count(), MOST + 1, "and its heading");
    }

    /// `im:message` is sensitive on some tenants, so the fetch is allowed to
    /// come back with nothing and the title has to stand on its own.
    #[tokio::test]
    async fn a_bundle_no_scope_allows_leaves_the_title_to_speak_for_it() {
        let server = MockServer::start().await;
        let api = api(&server).await;
        answering(
            &server,
            ResponseTemplate::new(403)
                .set_body_json(json!({ "code": 99991672, "msg": "no permission" })),
        )
        .await;
        assert_eq!(lines(&api, BUNDLE, ME).await, None);
    }

    #[tokio::test]
    async fn a_bundle_of_nothing_adds_nothing() {
        let server = MockServer::start().await;
        let api = api(&server).await;
        answering(
            &server,
            ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "data": { "items": [item(BUNDLE, "ou_a", "merge_forward", json!({}))] },
            })),
        )
        .await;
        assert_eq!(lines(&api, BUNDLE, ME).await, None);
    }
}
