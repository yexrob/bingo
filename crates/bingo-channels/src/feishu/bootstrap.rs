//! Getting on the long connection, and staying on it (ADR-0016 §6).
//!
//! The credentials are POSTed once — in PascalCase, which is not a typo — and
//! what comes back is a single-use `wss://` URL with the credentials already
//! in it, plus the intervals the peer wants this client to keep. Everything
//! here is pure: the shapes, the ladder and the deadlines are decided from
//! values, and `ws.rs` is what holds a socket.

use std::time::Duration;

use serde_json::{Value, json};

use crate::error::ChannelError;

/// Where the endpoint is asked for. The path is not under `/open-apis`.
pub const ENDPOINT: &str = "/callback/ws/endpoint";

/// What the peer wants of this client. Every field arrives in seconds and is
/// hot-updated by a pong, so the connection's timers follow the server's mind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientConfig {
    /// `-1` is "for ever", which is the only value ever seen.
    pub reconnect_count: i64,
    pub reconnect_interval: Duration,
    /// The window the first reconnect picks a random moment inside.
    pub reconnect_nonce: Duration,
    pub ping_interval: Duration,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            reconnect_count: -1,
            reconnect_interval: Duration::from_secs(120),
            reconnect_nonce: Duration::from_secs(30),
            ping_interval: Duration::from_secs(120),
        }
    }
}

impl ClientConfig {
    /// How long an idle socket may stay quiet before it is treated as dead.
    ///
    /// Twice the ping interval plus a margin, re-armed on **every** inbound
    /// frame, pongs included. An outbound ping proves nothing about the
    /// inbound path, and a laptop that slept through two of them is not a
    /// laptop with a working connection.
    pub fn read_deadline(&self) -> Duration {
        self.ping_interval * 2 + Duration::from_secs(5)
    }

    /// What to wait before attempt `attempt`. The first is a random moment
    /// inside the nonce window, so a fleet of clients does not come back in
    /// lockstep; every one after it is the flat interval.
    pub fn backoff(&self, attempt: u32, entropy: u64) -> Duration {
        if attempt > 0 {
            return self.reconnect_interval;
        }
        let window = self.reconnect_nonce.as_millis().max(1) as u64;
        Duration::from_millis(entropy % window)
    }

    /// Whatever of this a pong carried; the rest stays as it was.
    pub fn updated(&self, value: &Value) -> Self {
        let seconds = |key: &str, current: Duration| {
            value[key]
                .as_u64()
                .filter(|s| *s > 0)
                .map_or(current, Duration::from_secs)
        };
        Self {
            reconnect_count: value["ReconnectCount"]
                .as_i64()
                .unwrap_or(self.reconnect_count),
            reconnect_interval: seconds("ReconnectInterval", self.reconnect_interval),
            reconnect_nonce: seconds("ReconnectNonce", self.reconnect_nonce),
            ping_interval: seconds("PingInterval", self.ping_interval),
        }
    }
}

/// The bootstrap body. PascalCase, and `ClientAssertion` empty: this is what
/// the SDKs send and what the endpoint accepts.
pub fn request(app_id: &str, app_secret: &str) -> Value {
    json!({ "AppID": app_id, "AppSecret": app_secret, "ClientAssertion": "" })
}

/// The URL to dial, from the endpoint's answer. It carries the credentials
/// and is single-use, so it is never stored and never logged.
pub fn endpoint(body: &Value) -> Result<(String, ClientConfig), ChannelError> {
    match body["code"].as_i64() {
        Some(0) => {}
        _ => {
            return Err(ChannelError::Refused(format!(
                "feishu refused the long connection: {} ({})",
                body["msg"].as_str().unwrap_or("no reason given"),
                body["code"]
            )));
        }
    }
    let url = body["data"]["URL"]
        .as_str()
        .ok_or_else(|| ChannelError::Transport("the endpoint answered without a URL".into()))?;
    Ok((
        url.to_string(),
        ClientConfig::default().updated(&body["data"]["ClientConfig"]),
    ))
}

/// What a refused handshake means. The peer answers a rejected upgrade with
/// three response headers and no body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Nothing this process does will fix it.
    Fatal(String),
    /// Worth trying again on the ladder.
    Retry(String),
}

/// The connection-limit code, which is fatal however many times it is tried.
const CONNECTION_LIMIT: &str = "1000040350";

pub fn handshake(status: Option<&str>, message: Option<&str>, autherr: Option<&str>) -> Refusal {
    let why = |what: &str| {
        format!(
            "feishu refused the long connection ({what}): {}",
            message.unwrap_or("no reason given")
        )
    };
    match (status, autherr) {
        (Some("403"), _) => Refusal::Fatal(why("forbidden")),
        (Some("514"), Some(CONNECTION_LIMIT)) => Refusal::Fatal(why(
            "this app already has as many long connections as it may have",
        )),
        (Some("514"), _) => Refusal::Retry(why("try again")),
        (status, _) => Refusal::Retry(why(status.unwrap_or("no status"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The answer as the endpoint gives it, PascalCase and all.
    fn answer() -> Value {
        json!({
            "code": 0,
            "data": {
                "URL": "wss://open.feishu.cn/callback/ws?device_id=1&service_id=2",
                "ClientConfig": {
                    "ReconnectCount": -1,
                    "ReconnectInterval": 120,
                    "ReconnectNonce": 30,
                    "PingInterval": 120,
                },
            },
        })
    }

    #[test]
    fn the_request_is_the_pascal_case_one_the_endpoint_accepts() {
        insta::assert_json_snapshot!("feishu-bootstrap-request", request("cli_a", "secret-a"));
    }

    #[test]
    fn the_endpoint_answer_is_a_url_and_the_peers_intervals() {
        let (url, config) = endpoint(&answer()).expect("an endpoint");
        assert!(url.starts_with("wss://"));
        assert_eq!(config, ClientConfig::default());
    }

    #[test]
    fn a_refusal_carries_the_peers_own_words() {
        let refused = json!({ "code": 1000040343, "msg": "app not found" });
        let error = endpoint(&refused).expect_err("a refusal");
        assert!(error.to_string().contains("app not found"), "{error}");
        assert!(matches!(error, ChannelError::Refused(_)));
    }

    #[test]
    fn an_answer_without_a_url_is_a_transport_error_not_a_panic() {
        assert!(matches!(
            endpoint(&json!({ "code": 0, "data": {} })),
            Err(ChannelError::Transport(_))
        ));
    }

    #[test]
    fn a_pong_hot_updates_only_what_it_carries() {
        let config = ClientConfig::default().updated(&json!({ "PingInterval": 30 }));
        assert_eq!(config.ping_interval, Duration::from_secs(30));
        assert_eq!(
            config.reconnect_interval,
            ClientConfig::default().reconnect_interval,
            "what the pong did not say is what it was"
        );
        assert_eq!(
            config.read_deadline(),
            Duration::from_secs(65),
            "twice the new ping interval, plus the margin"
        );
    }

    #[test]
    fn the_first_reconnect_is_jittered_and_the_rest_are_flat() {
        let config = ClientConfig::default();
        assert!(config.backoff(0, 12_345) < config.reconnect_nonce);
        assert_eq!(config.backoff(0, 0), Duration::ZERO);
        for attempt in 1..5 {
            assert_eq!(config.backoff(attempt, 12_345), Duration::from_secs(120));
        }
    }

    #[test]
    fn a_forbidden_or_limit_bound_handshake_is_fatal_and_the_rest_are_worth_retrying() {
        assert!(matches!(
            handshake(Some("403"), Some("bad app"), None),
            Refusal::Fatal(_)
        ));
        assert!(matches!(
            handshake(Some("514"), Some("too many"), Some(CONNECTION_LIMIT)),
            Refusal::Fatal(_)
        ));
        assert!(matches!(
            handshake(Some("514"), Some("busy"), Some("1000040999")),
            Refusal::Retry(_)
        ));
        assert!(matches!(handshake(None, None, None), Refusal::Retry(_)));
        let Refusal::Fatal(message) = handshake(Some("403"), Some("bad app"), None) else {
            panic!("fatal");
        };
        assert!(message.contains("bad app"), "{message}");
    }
}
