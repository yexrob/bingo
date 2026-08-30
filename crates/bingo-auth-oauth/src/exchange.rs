//! The issuer's token and revoke endpoints, in one place.
//!
//! Both flows redeem a code the same way, so the form is written once; the
//! refresh next to it is the same endpoint with a different grant, and
//! keeping them together is what makes the encoding difference visible —
//! codex takes the code exchange as a form and the refresh as JSON.

use reqwest::Client;
use reqwest::header::CONTENT_TYPE;
use serde_json::{Value, json};

use crate::error::AuthError;
use crate::issuer::Issuer;
use crate::percent;

/// Redeem an authorization code with its PKCE verifier.
pub async fn authorization_code(
    http: &Client,
    issuer: &Issuer,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<Value, AuthError> {
    let body = form(&[
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", issuer.client_id.as_str()),
        ("code_verifier", verifier),
    ]);
    read(
        http.post(issuer.url(&issuer.token_path))
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await?,
    )
    .await
}

/// Written out rather than reached for through reqwest's `form` feature,
/// which would pull `serde_urlencoded` into every crate in the workspace for
/// five pairs that are already percent-encoded here.
fn form(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(name, value)| format!("{}={}", percent::encode(name), percent::encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Renew an access token. The body is JSON, which is what the old project ran
/// against the live issuer; opencode sends a form here, and the endpoint is
/// the same one either way.
pub async fn refresh(
    http: &Client,
    issuer: &Issuer,
    refresh_token: &str,
) -> Result<Value, AuthError> {
    let body = json!({
        "client_id": issuer.client_id,
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
    });
    read(
        http.post(issuer.url(&issuer.token_path))
            .json(&body)
            .send()
            .await?,
    )
    .await
}

/// Tell the issuer to forget the refresh token. Best effort by contract: a
/// caller signing out locally has already decided.
pub async fn revoke(http: &Client, issuer: &Issuer, token: &str) -> Result<(), AuthError> {
    let body = json!({ "client_id": issuer.client_id, "token": token });
    read(
        http.post(issuer.url(&issuer.revoke_path))
            .json(&body)
            .send()
            .await?,
    )
    .await?;
    Ok(())
}

/// A non-success status never leaves this module as a body: it leaves as a
/// classified error, so a retired refresh token is named once and everywhere.
pub(crate) async fn read(response: reqwest::Response) -> Result<Value, AuthError> {
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(AuthError::http(status.as_u16(), body));
    }
    if body.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&body).map_err(|e| AuthError::Invalid(format!("unreadable reply: {e}")))
}
