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

/// Redeem an authorization code with its PKCE verifier. `resource` rides
/// along when the issuer mints for one (RFC 8707), because a token endpoint
/// that was asked for an audience on authorize must be asked again here.
pub async fn authorization_code(
    http: &Client,
    issuer: &Issuer,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<Value, AuthError> {
    let mut fields = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", issuer.client_id.as_str()),
        ("code_verifier", verifier),
    ];
    if let Some(resource) = &issuer.resource {
        fields.push(("resource", resource.as_str()));
    }
    let body = form(&fields);
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

/// Renew an access token. RFC 6749 §6 says a form; codex answers JSON there,
/// which is what the old project ran against the live issuer, so the issuer
/// says which it takes ([`Issuer::form_encoded`]).
pub async fn refresh(
    http: &Client,
    issuer: &Issuer,
    refresh_token: &str,
) -> Result<Value, AuthError> {
    let url = issuer.url(&issuer.token_path);
    let request = if issuer.form_encoded {
        let mut fields = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", issuer.client_id.as_str()),
        ];
        if let Some(resource) = &issuer.resource {
            fields.push(("resource", resource.as_str()));
        }
        http.post(url)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form(&fields))
    } else {
        http.post(url).json(&json!({
            "client_id": issuer.client_id,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
        }))
    };
    read(request.send().await?).await
}

/// Tell the issuer to forget the refresh token (RFC 7009). Best effort by
/// contract: a caller signing out locally has already decided, and an issuer
/// that publishes no revocation endpoint is signed out of by forgetting.
pub async fn revoke(http: &Client, issuer: &Issuer, token: &str) -> Result<(), AuthError> {
    let Some(path) = &issuer.revoke_path else {
        return Ok(());
    };
    let url = issuer.url(path);
    let request = if issuer.form_encoded {
        http.post(url)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form(&[
                ("token", token),
                ("token_type_hint", "refresh_token"),
                ("client_id", issuer.client_id.as_str()),
            ]))
    } else {
        http.post(url)
            .json(&json!({ "client_id": issuer.client_id, "token": token }))
    };
    read(request.send().await?).await?;
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
