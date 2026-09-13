//! The redirect the browser sends back, read out of the target it asked for.
//!
//! A parser and nothing else: `bingo_loopback` owns the socket and reads the
//! request line, `redirect` owns the port and the page, and this owns the
//! meaning — so the `state` check has a pure test with no bytes in it.

use crate::error::AuthError;
use crate::percent;

/// The path the authorize redirect is pointed at.
pub const PATH: &str = "/auth/callback";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Callback {
    pub code: String,
    /// The nonce to compare against the one that built the authorize URL; an
    /// absent one is empty, which no generated nonce ever matches.
    pub state: String,
    /// RFC 9207: which authorization server answered. Present only when the
    /// server sends it, and then it must be the one the flow started from.
    pub iss: Option<String>,
}

/// `/auth/callback?code=…&state=…` → the values it carries.
pub fn parse(target: &str) -> Result<Callback, AuthError> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != PATH {
        return Err(AuthError::Invalid(format!(
            "the callback was sent to {path}"
        )));
    }
    query_of(query)
}

/// The same reading, of a query a person pasted rather than a browser sent:
/// the whole redirect URL, its query alone, or the bare code off the page.
pub fn pasted(text: &str) -> Result<Callback, AuthError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(AuthError::Invalid("nothing was pasted".into()));
    }
    match text.split_once('?') {
        Some((_, query)) => query_of(query),
        None if text.contains('=') => query_of(text),
        // A bare code, read off the page by the person who started the flow:
        // there is no `state` to check because there is no redirect to forge.
        None => Ok(Callback {
            code: text.to_string(),
            state: String::new(),
            iss: None,
        }),
    }
}

fn query_of(query: &str) -> Result<Callback, AuthError> {
    let query = query.split('#').next().unwrap_or(query);
    if let Some(error) = field(query, "error") {
        return Err(AuthError::Invalid(format!(
            "the authorization server refused: {error}"
        )));
    }
    let code = field(query, "code")
        .ok_or_else(|| AuthError::Invalid("the callback carries no code".into()))?;
    Ok(Callback {
        code,
        state: field(query, "state").unwrap_or_default(),
        iss: field(query, "iss"),
    })
}

fn field(query: &str, name: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| percent::decode(value))
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_callback_yields_its_code_and_state_percent_decoded() {
        let callback = parse("/auth/callback?code=ac%2F1&state=st-2").expect("a callback");
        assert_eq!(callback.code, "ac/1");
        assert_eq!(callback.state, "st-2");
    }

    #[test]
    fn the_order_of_the_query_does_not_matter_and_extra_fields_are_ignored() {
        let callback = parse("/auth/callback?state=st&extra=x&code=ac").expect("a callback");
        assert_eq!(
            callback,
            Callback {
                code: "ac".into(),
                state: "st".into(),
                iss: None,
            }
        );
    }

    /// RFC 9207: the server may name itself in the redirect, and a flow that
    /// asked one server must not accept a code minted by another.
    #[test]
    fn an_issuer_the_server_named_is_read_back_decoded() {
        let callback = parse("/auth/callback?code=ac&state=st&iss=https%3A%2F%2Fas.example.com")
            .expect("a callback");
        assert_eq!(callback.iss.as_deref(), Some("https://as.example.com"));
    }

    /// `--paste`: a person hands back the whole redirect, its query, or the
    /// code the page showed them.
    #[test]
    fn a_pasted_redirect_reads_the_same_as_one_the_browser_delivered() {
        let whole = pasted("http://localhost:1455/auth/callback?code=ac&state=st").expect("a code");
        assert_eq!(whole.code, "ac");
        assert_eq!(whole.state, "st");
        assert_eq!(pasted("code=ac&state=st").expect("a code").state, "st");
        let bare = pasted("  ac-1  ").expect("a code");
        assert_eq!(bare.code, "ac-1");
        assert_eq!(bare.state, "", "a bare code carries no nonce to check");
        assert!(matches!(pasted("   "), Err(AuthError::Invalid(_))));
    }

    /// A refusal is what the server said, not a missing code: a person who
    /// declined the consent screen should read why.
    #[test]
    fn a_refusal_in_the_query_is_reported_in_the_servers_own_words() {
        let refused = parse("/auth/callback?error=access_denied&state=st").expect_err("a refusal");
        assert!(refused.to_string().contains("access_denied"), "{refused}");
    }

    #[test]
    fn a_callback_without_a_state_is_read_but_matches_no_nonce() {
        let callback = parse("/auth/callback?code=ac").expect("a callback");
        assert_eq!(callback.state, "");
    }

    #[test]
    fn a_missing_code_or_another_path_is_invalid() {
        for target in [
            "/auth/callback",
            "/auth/callback?state=st",
            "/auth/callback?code=",
            "/favicon.ico",
            "/?code=ac",
        ] {
            assert!(
                matches!(parse(target), Err(AuthError::Invalid(_))),
                "{target} is not a usable callback"
            );
        }
        assert!(matches!(parse(""), Err(AuthError::Invalid(_))));
    }
}
