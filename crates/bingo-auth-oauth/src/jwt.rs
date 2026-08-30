//! The one JWT reader (ADR-0012 consequences).
//!
//! Pure over the token text: no signature check, because nothing here is an
//! authorisation decision. The server re-validates the token it was sent; a
//! claim we cannot read only means a header is omitted or a receipt names an
//! account id instead of an address.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;

/// A JWT's payload segment, decoded. `None` for anything that is not one —
/// an API key, an opaque token, a truncated string.
pub fn claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&decoded).ok()
}

/// The ChatGPT account a subscription token belongs to. The namespaced claim
/// is authoritative; a top-level one and the first organization are what
/// older tokens carry instead.
pub fn account_id(token: &str) -> Option<String> {
    let claims = claims(token)?;
    let namespaced = claims
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"));
    namespaced
        .or_else(|| claims.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| organization(&claims))
}

/// The address a receipt greets a person by.
pub fn email(token: &str) -> Option<String> {
    claims(token)?
        .get("email")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn organization(claims: &Value) -> Option<String> {
    claims
        .get("organizations")?
        .as_array()?
        .first()?
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A signature-less JWT: the decoder never checks one.
    fn jwt(payload: &Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let body = URL_SAFE_NO_PAD.encode(payload.to_string());
        format!("{header}.{body}.signature")
    }

    #[test]
    fn the_account_id_comes_from_the_namespaced_claim() {
        let token = jwt(&json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": "acc_namespaced" }
        }));
        assert_eq!(account_id(&token).as_deref(), Some("acc_namespaced"));
    }

    #[test]
    fn the_namespaced_claim_wins_over_a_top_level_one() {
        let token = jwt(&json!({
            "chatgpt_account_id": "acc_top",
            "https://api.openai.com/auth": { "chatgpt_account_id": "acc_namespaced" },
        }));
        assert_eq!(account_id(&token).as_deref(), Some("acc_namespaced"));
    }

    #[test]
    fn a_top_level_claim_is_still_read() {
        let token = jwt(&json!({ "chatgpt_account_id": "acc_top" }));
        assert_eq!(account_id(&token).as_deref(), Some("acc_top"));
    }

    #[test]
    fn the_first_organization_is_the_last_resort() {
        let token = jwt(&json!({ "organizations": [{ "id": "org_1" }, { "id": "org_2" }] }));
        assert_eq!(account_id(&token).as_deref(), Some("org_1"));
    }

    #[test]
    fn a_token_that_is_not_a_jwt_yields_no_account() {
        assert_eq!(account_id("sk-proj-not-a-jwt"), None);
        assert_eq!(account_id(""), None);
        assert_eq!(account_id("only.two"), None);
        assert_eq!(account_id("a.!!!not-base64!!!.c"), None);
        assert_eq!(account_id(&jwt(&json!({ "sub": "user_1" }))), None);
        assert_eq!(account_id(&jwt(&json!({ "chatgpt_account_id": 7 }))), None);
    }

    #[test]
    fn an_email_claim_is_read_and_a_missing_one_is_none() {
        assert_eq!(
            email(&jwt(&json!({ "email": "me@example.com" }))).as_deref(),
            Some("me@example.com")
        );
        assert_eq!(email(&jwt(&json!({ "sub": "user_1" }))), None);
        assert_eq!(email("not-a-jwt"), None);
    }
}
