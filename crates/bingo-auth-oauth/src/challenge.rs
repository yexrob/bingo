//! What a resource server says when it is asked without a token, and how to
//! make it say it (ADR-0050 §1).
//!
//! A `401` is the only thing that turns "this server is unreachable" into
//! "this server wants a sign-in", so it is read here rather than guessed
//! from a message: [`parse`] is pure over the `WWW-Authenticate` value, and
//! [`probe`] is the one request that fetches one. The probe speaks MCP's
//! `initialize` because that is the request the server will refuse — a bare
//! `GET` may be answered by a proxy that never sees the protocol.

use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, WWW_AUTHENTICATE};
use serde_json::json;

use crate::error::AuthError;

/// The MCP revision this probe claims. It is not the revision a session then
/// negotiates — [`probe`] never keeps the session it opens — only what makes
/// a well-formed `initialize` for a server on either revision.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// The `Bearer` challenge of RFC 9728 §5.1, as far as this client acts on it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Challenge {
    /// Where the protected-resource metadata is. A server on the 2025-06-18
    /// revision sends none, and the well-known ladder finds it instead.
    pub resource_metadata: Option<String>,
    pub scope: Option<String>,
    pub error: Option<String>,
}

/// What the resource server answered when asked with what we have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Probe {
    /// It did not ask for a token, so nothing here has anything to add.
    Ok,
    Unauthorized(Challenge),
}

/// `Bearer realm="…", resource_metadata="…", scope="…"` → what it names.
///
/// Anything that is not one of the three parameters is skipped rather than
/// refused: a challenge carrying a `realm` this client has no use for is
/// still a challenge, and a header this parser cannot read at all is an
/// empty challenge — the well-known ladder covers it.
pub fn parse(header: &str) -> Challenge {
    let mut challenge = Challenge::default();
    for (name, value) in parameters(header) {
        match name.as_str() {
            "resource_metadata" => challenge.resource_metadata = Some(value),
            "scope" => challenge.scope = Some(value),
            "error" => challenge.error = Some(value),
            _ => {}
        }
    }
    challenge
}

/// The `name=value` pairs of a challenge, lowercased names and unquoted
/// values. Split on commas that are not inside a quoted string, because a
/// `scope` list is space-separated but an `error_description` is not.
fn parameters(header: &str) -> Vec<(String, String)> {
    let rest = header
        .split_once(char::is_whitespace)
        .map_or("", |(_scheme, rest)| rest);
    split_outside_quotes(rest)
        .into_iter()
        .filter_map(|pair| pair.split_once('='))
        .map(|(name, value)| {
            (
                name.trim().to_ascii_lowercase(),
                value.trim().trim_matches('"').to_string(),
            )
        })
        .filter(|(_, value)| !value.is_empty())
        .collect()
}

fn split_outside_quotes(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut quoted) = (0, false);
    for (at, character) in text.char_indices() {
        match character {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                parts.push(&text[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// Ask the server for an `initialize` with the headers we have, and read what
/// it says about signing in. A `401` is the answer this exists for; every
/// other status — success, a refusal for another reason, a body this client
/// never reads — is [`Probe::Ok`], because none of them is a sign-in.
pub async fn probe(
    http: &reqwest::Client,
    url: &str,
    headers: HeaderMap,
) -> Result<Probe, AuthError> {
    let response = http
        .post(url)
        .headers(headers)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json, text/event-stream")
        .header("MCP-Protocol-Version", PROTOCOL_VERSION)
        .json(&initialize())
        .send()
        .await?;
    if response.status().as_u16() != 401 {
        return Ok(Probe::Ok);
    }
    Ok(Probe::Unauthorized(
        response
            .headers()
            .get(WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())
            .map(parse)
            .unwrap_or_default(),
    ))
}

fn initialize() -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "bingo", "version": env!("CARGO_PKG_VERSION") },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn a_challenge_names_the_metadata_the_scope_and_the_error() {
        assert_eq!(
            parse(
                r#"Bearer realm="mcp", resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource", scope="mcp:tools mcp:read", error="invalid_token""#
            ),
            Challenge {
                resource_metadata: Some(
                    "https://mcp.example.com/.well-known/oauth-protected-resource".into()
                ),
                scope: Some("mcp:tools mcp:read".into()),
                error: Some("invalid_token".into()),
            }
        );
    }

    /// The 2025-06-18 revision: a bare challenge is still a challenge.
    #[test]
    fn a_challenge_that_names_nothing_is_empty_rather_than_a_failure() {
        assert_eq!(parse("Bearer"), Challenge::default());
        assert_eq!(parse(""), Challenge::default());
        assert_eq!(parse(r#"Bearer realm="mcp""#), Challenge::default());
    }

    /// A comma inside a quoted value ends no parameter — an
    /// `error_description` is prose, and the `scope` after it must survive.
    #[test]
    fn a_comma_inside_a_quoted_value_does_not_end_the_parameter() {
        let challenge = parse(
            r#"Bearer error="invalid_token", error_description="expired, sign in again", scope="a b""#,
        );
        assert_eq!(challenge.error.as_deref(), Some("invalid_token"));
        assert_eq!(challenge.scope.as_deref(), Some("a b"));
    }

    #[test]
    fn a_parameter_name_in_any_case_is_read_and_an_unquoted_value_too() {
        let challenge = parse("Bearer Scope=mcp:tools, Resource_Metadata=https://r/x");
        assert_eq!(challenge.scope.as_deref(), Some("mcp:tools"));
        assert_eq!(challenge.resource_metadata.as_deref(), Some("https://r/x"));
    }

    async fn mcp(server: &MockServer, response: ResponseTemplate) {
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(response)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn a_401_comes_back_as_the_challenge_it_carried() {
        let server = MockServer::start().await;
        mcp(
            &server,
            ResponseTemplate::new(401).insert_header(
                "www-authenticate",
                r#"Bearer resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource""#,
            ),
        )
        .await;
        let probed = probe(
            &reqwest::Client::new(),
            &format!("{}/mcp", server.uri()),
            HeaderMap::new(),
        )
        .await
        .expect("a probe");
        let Probe::Unauthorized(challenge) = probed else {
            panic!("a 401 is a challenge, not {probed:?}");
        };
        assert_eq!(
            challenge.resource_metadata.as_deref(),
            Some("https://mcp.example.com/.well-known/oauth-protected-resource")
        );
    }

    #[tokio::test]
    async fn a_server_that_answers_asks_for_no_sign_in() {
        let server = MockServer::start().await;
        mcp(&server, ResponseTemplate::new(200).set_body_string("{}")).await;
        assert_eq!(
            probe(
                &reqwest::Client::new(),
                &format!("{}/mcp", server.uri()),
                HeaderMap::new()
            )
            .await
            .expect("a probe"),
            Probe::Ok
        );
    }

    /// A `403` is a scope the token lacks, not a sign-in that never happened
    /// (the step-up is a non-goal of M85); a `500` is the server's own day.
    #[tokio::test]
    async fn only_a_401_is_a_sign_in_and_the_other_refusals_are_not() {
        for status in [403, 404, 500] {
            let server = MockServer::start().await;
            mcp(&server, ResponseTemplate::new(status)).await;
            assert_eq!(
                probe(
                    &reqwest::Client::new(),
                    &format!("{}/mcp", server.uri()),
                    HeaderMap::new()
                )
                .await
                .expect("a probe"),
                Probe::Ok,
                "{status}"
            );
        }
    }

    #[tokio::test]
    async fn a_server_nothing_is_listening_on_is_a_transport_failure() {
        let probed = probe(
            &reqwest::Client::new(),
            "http://127.0.0.1:1/mcp",
            HeaderMap::new(),
        )
        .await;
        assert!(matches!(probed, Err(AuthError::Transport(_))), "{probed:?}");
    }
}
