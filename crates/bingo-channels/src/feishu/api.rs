//! Feishu's HTTP side: one client, one bearer, one way to read a refusal.
//!
//! Every open-api answer is `{"code": …, "msg": …}` with the payload under
//! `data`, and `code == 0` is the only success. Some refusals are worth
//! shrugging at — a rate limit mid-stream costs one frame, not the stream —
//! so the code survives as far as the caller that has to decide.

use serde_json::Value;

use super::bootstrap::{self, ClientConfig};
use super::token::Tokens;
use crate::error::ChannelError;

pub const BASE: &str = "https://open.feishu.cn";

/// Too many requests. The header `x-ogw-ratelimit-reset` says how long for;
/// a streaming frame is not worth waiting out, so it is dropped instead.
const RATE_LIMITED: i64 = 99_991_400;
/// The card is busy with an earlier update of the same stream.
const CARD_BUSY: i64 = 230_020;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// Feishu answered, with a code and its own words.
    #[error("feishu {code}: {message}")]
    Refused { code: i64, message: String },
    #[error("{0}")]
    Transport(String),
}

impl ApiError {
    /// Whether the right answer is to drop this one call and carry on. A
    /// streamed answer that stops at the first rate limit is worse than one
    /// that skips a frame.
    pub fn transient(&self) -> bool {
        matches!(self, ApiError::Refused { code, .. } if *code == RATE_LIMITED || *code == CARD_BUSY)
    }
}

impl From<ApiError> for ChannelError {
    fn from(error: ApiError) -> Self {
        match error {
            ApiError::Refused { .. } => ChannelError::Platform(error.to_string()),
            ApiError::Transport(message) => ChannelError::Transport(message),
        }
    }
}

pub struct Api {
    http: reqwest::Client,
    base: String,
    tokens: Tokens,
}

impl std::fmt::Debug for Api {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Api")
            .field("base", &self.base)
            .field("tokens", &self.tokens)
            .finish_non_exhaustive()
    }
}

impl Api {
    pub fn new(base: impl Into<String>, app_id: &str, app_secret: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.into(),
            tokens: Tokens::new(app_id, app_secret),
        }
    }

    pub fn app_id(&self) -> &str {
        self.tokens.app_id()
    }

    pub async fn get(&self, path: &str) -> Result<Value, ApiError> {
        self.send(reqwest::Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<Value, ApiError> {
        self.send(reqwest::Method::POST, path, Some(body)).await
    }

    pub async fn put(&self, path: &str, body: Value) -> Result<Value, ApiError> {
        self.send(reqwest::Method::PUT, path, Some(body)).await
    }

    pub async fn patch(&self, path: &str, body: Value) -> Result<Value, ApiError> {
        self.send(reqwest::Method::PATCH, path, Some(body)).await
    }

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, ApiError> {
        let bearer = self
            .tokens
            .bearer(&self.http, &self.base, std::time::Instant::now())
            .await?;
        let mut call = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(bearer);
        if let Some(body) = &body {
            call = call.json(body);
        }
        request(call).await
    }

    /// The long connection's endpoint. It takes the credentials in the body
    /// rather than a bearer, and its path is not under `/open-apis`.
    pub async fn endpoint(&self, app_secret: &str) -> Result<(String, ClientConfig), ChannelError> {
        let body = bootstrap::request(self.app_id(), app_secret);
        let answer = request(
            self.http
                .post(format!("{}{}", self.base, bootstrap::ENDPOINT))
                .json(&body),
        )
        .await
        .map_err(ChannelError::from)?;
        bootstrap::endpoint(&answer)
    }
}

/// One call, with Feishu's envelope unwrapped. The whole body comes back,
/// not just `data`: the bootstrap answer and the token answer both keep
/// what they need at the top level.
pub async fn request(call: reqwest::RequestBuilder) -> Result<Value, ApiError> {
    let response = call
        .send()
        .await
        .map_err(|e| ApiError::Transport(format!("feishu: {e}")))?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|e| ApiError::Transport(format!("feishu answered {status}: {e}")))?;
    match body["code"].as_i64() {
        Some(0) | None if status.is_success() => Ok(body),
        code => Err(ApiError::Refused {
            code: code.unwrap_or(i64::from(status.as_u16())),
            message: body["msg"]
                .as_str()
                .unwrap_or("no reason given")
                .to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn signed_in(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/open-apis/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "tenant_access_token": "t-1", "expire": 7200,
            })))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn a_call_carries_the_bearer_and_hands_back_the_whole_body() {
        let server = MockServer::start().await;
        signed_in(&server).await;
        Mock::given(method("POST"))
            .and(path("/open-apis/im/v1/messages"))
            .and(header("authorization", "Bearer t-1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "code": 0, "data": { "message_id": "om_1" } })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let api = Api::new(server.uri(), "cli_a", "secret");
        let answer = api
            .post("/open-apis/im/v1/messages", json!({}))
            .await
            .expect("a message");
        assert_eq!(answer["data"]["message_id"], "om_1");
    }

    #[tokio::test]
    async fn a_refusal_keeps_its_code_so_the_caller_can_decide() {
        let server = MockServer::start().await;
        signed_in(&server).await;
        Mock::given(method("PUT"))
            .and(path("/x"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "code": RATE_LIMITED, "msg": "too many requests" })),
            )
            .mount(&server)
            .await;
        let api = Api::new(server.uri(), "cli_a", "secret");
        let error = api.put("/x", json!({})).await.expect_err("a refusal");
        assert!(error.transient(), "{error}");
        assert_eq!(
            error.to_string(),
            "feishu 99991400: too many requests",
            "the code and the peer's own words"
        );
    }

    #[tokio::test]
    async fn a_refusal_that_is_not_a_rate_limit_is_not_shrugged_at() {
        let server = MockServer::start().await;
        signed_in(&server).await;
        Mock::given(method("POST"))
            .and(path("/x"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "code": 230_072, "msg": "too many edits" })),
            )
            .mount(&server)
            .await;
        let api = Api::new(server.uri(), "cli_a", "secret");
        let error = api.post("/x", json!({})).await.expect_err("a refusal");
        assert!(!error.transient(), "{error}");
        assert!(matches!(
            ChannelError::from(error),
            ChannelError::Platform(_)
        ));
    }

    #[tokio::test]
    async fn the_endpoint_is_asked_for_without_a_bearer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(bootstrap::ENDPOINT))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "data": { "URL": "wss://example.invalid/x", "ClientConfig": { "PingInterval": 60 } },
            })))
            .expect(1)
            .mount(&server)
            .await;
        let api = Api::new(server.uri(), "cli_a", "secret");
        let (url, config) = api.endpoint("secret").await.expect("an endpoint");
        assert_eq!(url, "wss://example.invalid/x");
        assert_eq!(config.ping_interval, std::time::Duration::from_secs(60));
    }
}
