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
}

/// `/auth/callback?code=…&state=…` → the two values it carries.
pub fn parse(target: &str) -> Result<Callback, AuthError> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != PATH {
        return Err(AuthError::Invalid(format!(
            "the callback was sent to {path}"
        )));
    }
    let code = field(query, "code")
        .ok_or_else(|| AuthError::Invalid("the callback carries no code".into()))?;
    Ok(Callback {
        code,
        state: field(query, "state").unwrap_or_default(),
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
                state: "st".into()
            }
        );
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
