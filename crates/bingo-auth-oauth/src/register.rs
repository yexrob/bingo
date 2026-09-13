//! Becoming a client of an authorization server nobody registered us with
//! (RFC 7591, ADR-0050 §1).
//!
//! bingo is a native public client: it holds no secret it could keep, so it
//! registers with `token_endpoint_auth_method: none` and is identified by the
//! redirect URI it named. Deprecated by MCP's 2026-07-28 revision in favour
//! of Client ID Metadata Documents, which need an HTTPS URL bingo does not
//! have (ADR-0050 §5), and kept because it is what the servers of today do.

use serde_json::{Value, json};

use crate::error::AuthError;

/// What the server calls us afterwards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    pub client_id: String,
    /// Some servers issue one even to a client that asked for none; it is
    /// kept because the token endpoint may then require it.
    pub client_secret: Option<String>,
}

/// The body of a registration request, as this client sends it. Separate
/// from the request so the shape has a test with no socket in it.
pub fn request(redirect_uris: &[String]) -> Value {
    json!({
        "client_name": "bingo",
        "application_type": "native",
        "redirect_uris": redirect_uris,
        "grant_types": ["authorization_code", "refresh_token"],
        "token_endpoint_auth_method": "none",
    })
}

/// Register at the endpoint the server's metadata named.
pub async fn register(
    http: &reqwest::Client,
    endpoint: &str,
    redirect_uris: &[String],
) -> Result<Registration, AuthError> {
    let reply = crate::exchange::read(
        http.post(endpoint)
            .json(&request(redirect_uris))
            .send()
            .await?,
    )
    .await?;
    read(&reply)
}

fn read(reply: &Value) -> Result<Registration, AuthError> {
    let client_id = reply
        .get("client_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| AuthError::Invalid("the registration reply carries no client_id".into()))?;
    Ok(Registration {
        client_id: client_id.to_string(),
        client_secret: reply
            .get("client_secret")
            .and_then(Value::as_str)
            .filter(|secret| !secret.is_empty())
            .map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn uris() -> Vec<String> {
        vec![
            "http://localhost:1455/auth/callback".to_string(),
            "http://127.0.0.1:1455/auth/callback".to_string(),
        ]
    }

    /// Pinned as a literal: what a server is told about this client is a wire
    /// contract, and a field that quietly stops being sent is a registration
    /// that quietly stops working.
    #[test]
    fn the_request_is_the_native_public_client_rfc_7591_describes() {
        assert_eq!(
            request(&uris()),
            json!({
                "client_name": "bingo",
                "application_type": "native",
                "redirect_uris": [
                    "http://localhost:1455/auth/callback",
                    "http://127.0.0.1:1455/auth/callback",
                ],
                "grant_types": ["authorization_code", "refresh_token"],
                "token_endpoint_auth_method": "none",
            })
        );
    }

    #[test]
    fn a_reply_yields_the_id_and_the_secret_only_when_there_is_one() {
        assert_eq!(
            read(&json!({ "client_id": "cl_1" })).expect("a registration"),
            Registration {
                client_id: "cl_1".into(),
                client_secret: None
            }
        );
        assert_eq!(
            read(&json!({ "client_id": "cl_1", "client_secret": "sh" })).expect("a registration"),
            Registration {
                client_id: "cl_1".into(),
                client_secret: Some("sh".into())
            }
        );
        assert!(read(&json!({ "client_secret": "sh" })).is_err());
        assert!(read(&json!({ "client_id": "" })).is_err());
    }

    #[tokio::test]
    async fn a_server_that_registers_us_answers_with_the_name_it_gave() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/register"))
            .and(body_json(request(&uris())))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(serde_json::json!({ "client_id": "cl_1" })),
            )
            .mount(&server)
            .await;
        assert_eq!(
            register(
                &reqwest::Client::new(),
                &format!("{}/register", server.uri()),
                &uris()
            )
            .await
            .expect("a registration"),
            Registration {
                client_id: "cl_1".into(),
                client_secret: None
            }
        );
    }

    #[tokio::test]
    async fn a_server_that_refuses_says_so_with_its_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/register"))
            .respond_with(
                ResponseTemplate::new(400).set_body_string(r#"{"error":"invalid_redirect_uri"}"#),
            )
            .mount(&server)
            .await;
        let refused = register(
            &reqwest::Client::new(),
            &format!("{}/register", server.uri()),
            &uris(),
        )
        .await
        .expect_err("refused");
        assert!(
            matches!(refused, AuthError::Http { status: 400, .. }),
            "{refused:?}"
        );
    }
}
