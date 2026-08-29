//! Which Responses endpoint a provider instance talks to.
//!
//! The wire format is one format; a variant is the handful of places the
//! ChatGPT subscription endpoint departs from the public API. Keeping the
//! departures in one enum is what lets the encoder stay a pure function of
//! `(request, variant)` and the isolation test read as a table.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;

/// The header the subscription endpoint identifies the client by (old
/// `providers/openai.rs:239-248`).
pub const ORIGINATOR: &str = "bingo";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Variant {
    /// `https://api.openai.com/v1/responses`, an API key.
    #[default]
    Default,
    /// The ChatGPT subscription endpoint, a bearer OAuth token. Encoded and
    /// tested here; registered when OAuth lands (M10).
    Codex,
}

impl Variant {
    pub fn path(self) -> &'static str {
        match self {
            Variant::Default => "/v1/responses",
            Variant::Codex => "/codex/responses",
        }
    }

    /// The subscription endpoint rejects `max_output_tokens` with a 400
    /// (`Unsupported parameter`); the budget is the plan's, not the caller's.
    pub fn sends_max_output_tokens(self) -> bool {
        self == Variant::Default
    }

    /// The id configuration refers to. The wire format is one format, so
    /// `provider_metadata` stays keyed by `openai` for both.
    pub fn provider_id(self) -> &'static str {
        match self {
            Variant::Default => "openai",
            Variant::Codex => "codex",
        }
    }
}

/// The ChatGPT account a subscription token belongs to, read from the JWT's
/// claims. Pure over the token text: no signature check, because this is not
/// an authorisation decision — the server re-validates the token it was sent,
/// and a claim we cannot read only means the header is omitted.
pub fn account_id(token: &str) -> Option<String> {
    let claims = claims(token)?;
    let auth = claims.get("https://api.openai.com/auth");
    auth.and_then(|auth| auth.get("chatgpt_account_id"))
        .or_else(|| claims.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// A JWT's payload segment, decoded. `None` for anything that is not one —
/// an API key, an opaque token, a truncated string.
fn claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&decoded).ok()
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
    fn each_variant_names_its_own_path_and_budget_rule() {
        assert_eq!(Variant::Default.path(), "/v1/responses");
        assert_eq!(Variant::Codex.path(), "/codex/responses");
        assert!(Variant::Default.sends_max_output_tokens());
        assert!(!Variant::Codex.sends_max_output_tokens());
        assert_eq!(Variant::Default.provider_id(), "openai");
        assert_eq!(Variant::Codex.provider_id(), "codex");
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
    fn a_token_that_is_not_a_jwt_yields_no_account() {
        assert_eq!(account_id("sk-proj-not-a-jwt"), None);
        assert_eq!(account_id(""), None);
        assert_eq!(account_id("only.two"), None);
        assert_eq!(account_id("a.!!!not-base64!!!.c"), None);
        assert_eq!(account_id(&jwt(&json!({ "sub": "user_1" }))), None);
        assert_eq!(account_id(&jwt(&json!({ "chatgpt_account_id": 7 }))), None);
    }
}
