//! What a token endpoint hands back, and what a stored entry becomes.
//!
//! The expiry is kept as the instant it happens rather than the lifetime the
//! issuer quoted: a lifetime is only true at the moment it is read, and one
//! fact stored twice is one fact too many (`expires_in` never survives here).

use serde_json::Value;

use crate::jwt;
use crate::store::Entry;

/// How long before an expiry a token counts as stale, so a turn that starts
/// now does not 401 halfway through.
const REFRESH_LEAD: i64 = 300;

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Tokens {
    pub access: String,
    pub refresh: Option<String>,
    pub id_token: Option<String>,
    /// Unix seconds. `None` is "the issuer did not say", which is treated as
    /// stale rather than eternal.
    pub expires_at: Option<i64>,
    pub account_id: Option<String>,
}

impl Tokens {
    /// A token endpoint reply. `account_id` is backfilled from the id token
    /// and then the access token, because the subscription endpoint routes on
    /// it and the reply often omits it.
    pub fn from_response(body: &Value, now: i64) -> Self {
        let access = string(body, "access_token").unwrap_or_default();
        let id_token = string(body, "id_token");
        let account_id = string(body, "account_id")
            .or_else(|| id_token.as_deref().and_then(jwt::account_id))
            .or_else(|| jwt::account_id(&access));
        Tokens {
            expires_at: body
                .get("expires_in")
                .and_then(Value::as_i64)
                .map(|e| now + e),
            refresh: string(body, "refresh_token"),
            access,
            id_token,
            account_id,
        }
    }

    pub fn is_fresh(&self, now: i64) -> bool {
        self.expires_at
            .is_some_and(|at| at.saturating_sub(now) > REFRESH_LEAD)
    }

    /// A refresh reply carries the new access token and, often, nothing else:
    /// what it leaves out is still true of the credential it renewed.
    pub fn merged(self, previous: &Tokens) -> Tokens {
        Tokens {
            refresh: self
                .refresh
                .filter(|token| !token.is_empty())
                .or_else(|| previous.refresh.clone()),
            id_token: self.id_token.or_else(|| previous.id_token.clone()),
            account_id: self.account_id.or_else(|| previous.account_id.clone()),
            ..self
        }
    }

    /// The address a receipt greets a person by, when the id token names one.
    pub fn email(&self) -> Option<String> {
        self.id_token.as_deref().and_then(jwt::email)
    }

    pub fn entry(&self) -> Entry {
        Entry::OAuth {
            access: self.access.clone(),
            refresh: self.refresh.clone().unwrap_or_default(),
            expires: self.expires_at.unwrap_or_default(),
            account_id: self.account_id.clone(),
        }
    }

    /// The stored half of a token set: the id token is not persisted, so an
    /// account read back from the file is the one that was written with it.
    pub fn from_entry(entry: &Entry) -> Option<Tokens> {
        match entry {
            Entry::OAuth {
                access,
                refresh,
                expires,
                account_id,
            } => Some(Tokens {
                access: access.clone(),
                refresh: Some(refresh.clone()).filter(|token| !token.is_empty()),
                id_token: None,
                expires_at: (*expires > 0).then_some(*expires),
                account_id: account_id.clone(),
            }),
            Entry::Api { .. } => None,
        }
    }
}

/// Secrets stay out of every line this process writes, a panic message
/// included.
impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("access", &"<redacted>")
            .field("refresh", &self.refresh.as_ref().map(|_| "<redacted>"))
            .field("id_token", &self.id_token.as_ref().map(|_| "<redacted>"))
            .field("expires_at", &self.expires_at)
            .field("account_id", &self.account_id)
            .finish()
    }
}

/// Unix seconds. A clock before the epoch is a clock nothing can be decided
/// from, so it reads as the epoch and every token reads as stale.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default()
}

fn string(body: &Value, key: &str) -> Option<String> {
    body.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde_json::json;

    fn jwt(payload: &Value) -> String {
        format!(
            "{}.{}.signature",
            URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#),
            URL_SAFE_NO_PAD.encode(payload.to_string())
        )
    }

    #[test]
    fn a_lifetime_becomes_an_instant_and_the_reply_fills_every_field() {
        let tokens = Tokens::from_response(
            &json!({
                "access_token": "at",
                "refresh_token": "rt",
                "id_token": "it",
                "expires_in": 3600,
                "account_id": "acc_body",
            }),
            1_000,
        );
        assert_eq!(tokens.access, "at");
        assert_eq!(tokens.refresh.as_deref(), Some("rt"));
        assert_eq!(tokens.id_token.as_deref(), Some("it"));
        assert_eq!(tokens.expires_at, Some(4_600));
        assert_eq!(tokens.account_id.as_deref(), Some("acc_body"));
    }

    #[test]
    fn an_account_absent_from_the_reply_comes_from_the_id_token_then_the_access_token() {
        let from_id = Tokens::from_response(
            &json!({
                "access_token": jwt(&json!({ "chatgpt_account_id": "acc_access" })),
                "id_token": jwt(&json!({ "chatgpt_account_id": "acc_id" })),
            }),
            0,
        );
        assert_eq!(from_id.account_id.as_deref(), Some("acc_id"));

        let from_access = Tokens::from_response(
            &json!({ "access_token": jwt(&json!({ "chatgpt_account_id": "acc_access" })) }),
            0,
        );
        assert_eq!(from_access.account_id.as_deref(), Some("acc_access"));

        let from_neither = Tokens::from_response(&json!({ "access_token": "opaque" }), 0);
        assert_eq!(from_neither.account_id, None);
    }

    #[test]
    fn freshness_leads_the_expiry_by_five_minutes_and_no_expiry_is_never_fresh() {
        let expiring = Tokens {
            expires_at: Some(1_000),
            ..Tokens::default()
        };
        assert!(expiring.is_fresh(699), "301s of life left is fresh");
        assert!(!expiring.is_fresh(700), "300s of life left is not");
        assert!(!expiring.is_fresh(2_000), "past the expiry is not");
        assert!(
            !Tokens {
                expires_at: None,
                ..Tokens::default()
            }
            .is_fresh(0),
            "an unknown expiry is treated as stale"
        );
    }

    #[test]
    fn a_refresh_reply_that_omits_a_field_keeps_the_previous_one() {
        let previous = Tokens {
            access: "old".into(),
            refresh: Some("rt-old".into()),
            id_token: Some("it-old".into()),
            expires_at: Some(1),
            account_id: Some("acc_old".into()),
        };
        let merged =
            Tokens::from_response(&json!({ "access_token": "new", "expires_in": 60 }), 1_000)
                .merged(&previous);
        assert_eq!(merged.access, "new");
        assert_eq!(merged.expires_at, Some(1_060));
        assert_eq!(merged.refresh.as_deref(), Some("rt-old"));
        assert_eq!(merged.id_token.as_deref(), Some("it-old"));
        assert_eq!(merged.account_id.as_deref(), Some("acc_old"));
    }

    #[test]
    fn a_rotated_refresh_token_replaces_the_previous_one() {
        let previous = Tokens {
            refresh: Some("rt-old".into()),
            ..Tokens::default()
        };
        let merged = Tokens::from_response(
            &json!({ "access_token": "a", "refresh_token": "rt-new" }),
            0,
        )
        .merged(&previous);
        assert_eq!(merged.refresh.as_deref(), Some("rt-new"));
    }

    #[test]
    fn an_entry_round_trips_and_an_api_key_is_not_a_token_set() {
        let tokens = Tokens {
            access: "at".into(),
            refresh: Some("rt".into()),
            id_token: Some("it".into()),
            expires_at: Some(1_786_000_000),
            account_id: Some("acc_1".into()),
        };
        let read_back = Tokens::from_entry(&tokens.entry()).expect("an oauth entry");
        assert_eq!(read_back.access, "at");
        assert_eq!(read_back.refresh.as_deref(), Some("rt"));
        assert_eq!(read_back.expires_at, Some(1_786_000_000));
        assert_eq!(read_back.account_id.as_deref(), Some("acc_1"));
        assert_eq!(read_back.id_token, None, "the id token is not persisted");
        assert_eq!(Tokens::from_entry(&Entry::Api { key: "sk".into() }), None);
    }

    #[test]
    fn an_entry_with_no_expiry_or_refresh_token_reads_back_as_unknown() {
        let entry = Entry::OAuth {
            access: "at".into(),
            refresh: String::new(),
            expires: 0,
            account_id: None,
        };
        let tokens = Tokens::from_entry(&entry).expect("an oauth entry");
        assert_eq!(tokens.expires_at, None);
        assert_eq!(tokens.refresh, None);
    }

    #[test]
    fn a_receipt_reads_the_email_out_of_the_id_token() {
        let tokens = Tokens {
            id_token: Some(jwt(&json!({ "email": "me@example.com" }))),
            ..Tokens::default()
        };
        assert_eq!(tokens.email().as_deref(), Some("me@example.com"));
        assert_eq!(Tokens::default().email(), None);
    }

    #[test]
    fn debug_never_prints_a_secret() {
        let tokens = Tokens {
            access: "at-secret".into(),
            refresh: Some("rt-secret".into()),
            id_token: Some("it-secret".into()),
            expires_at: Some(1),
            account_id: Some("acc_1".into()),
        };
        let printed = format!("{tokens:?}");
        assert!(!printed.contains("secret"), "{printed}");
        assert!(printed.contains("acc_1"), "{printed}");
    }
}
