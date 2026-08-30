//! The code-on-another-screen flow (ADR-0012 §4).
//!
//! Not RFC 8628: codex mints the code at one path and grants at another, and
//! the grant carries the PKCE pair the issuer generated for itself — so the
//! exchange that follows is the ordinary one, with a verifier this process
//! never chose.

use serde_json::{Value, json};
use tokio::time::{Duration, Instant, sleep};

use crate::error::AuthError;
use crate::issuer::Issuer;

/// How long a person has to enter the code, unless the issuer says sooner.
pub const MAX_WAIT: Duration = Duration::from_secs(15 * 60);

/// What a person is shown and what the poll is keyed by.
#[derive(Clone, Debug)]
pub struct Started {
    pub device_auth_id: String,
    pub user_code: String,
    pub interval_secs: u64,
    pub verify_url: String,
}

pub async fn start(http: &reqwest::Client, issuer: &Issuer) -> Result<Started, AuthError> {
    let body = json!({ "client_id": issuer.client_id });
    let reply = crate::exchange::read(
        http.post(issuer.url(&issuer.device_code_path))
            .json(&body)
            .send()
            .await?,
    )
    .await?;
    Ok(Started {
        device_auth_id: required(&reply, "device_auth_id")?,
        user_code: required(&reply, "user_code")?,
        // A zero interval would be a busy loop against the issuer.
        interval_secs: reply
            .get("interval")
            .and_then(Value::as_u64)
            .unwrap_or(5)
            .max(1),
        verify_url: issuer.verify_url(),
    })
}

/// Poll until the issuer grants, refuses, or the deadline passes. A 403 or a
/// 404 is the issuer saying "not yet" — it is not an error to report.
pub async fn poll(
    http: &reqwest::Client,
    issuer: &Issuer,
    started: &Started,
    deadline: Instant,
) -> Result<Value, AuthError> {
    let body = json!({
        "device_auth_id": started.device_auth_id,
        "user_code": started.user_code,
    });
    let url = issuer.url(&issuer.device_token_path);
    loop {
        let response = http.post(&url).json(&body).send().await?;
        let status = response.status().as_u16();
        if !matches!(status, 403 | 404) {
            return crate::exchange::read(response).await;
        }
        if Instant::now() >= deadline {
            return Err(AuthError::Timeout);
        }
        sleep(Duration::from_secs(started.interval_secs)).await;
    }
}

/// Redeem the grant. The issuer generated the code and the verifier, and the
/// redirect it names was never called by anything.
pub async fn exchange(
    http: &reqwest::Client,
    issuer: &Issuer,
    grant: &Value,
) -> Result<Value, AuthError> {
    crate::exchange::authorization_code(
        http,
        issuer,
        &required(grant, "authorization_code")?,
        &issuer.device_redirect_uri(),
        &required(grant, "code_verifier")?,
    )
    .await
}

fn required(body: &Value, field: &str) -> Result<String, AuthError> {
    body.get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AuthError::Invalid(format!("the device reply carries no {field}")))
}
