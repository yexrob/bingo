//! The token: one secret per bridged conversation.
//!
//! ADR-0036 §3: the token is the address and the session is the authority. It
//! rides the server row's `env` rather than its argv, and nothing but the
//! bridge that minted it ever sees it — which is why [`Token`] prints as a
//! blind and never as itself.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::error::AcpError;

/// 32 random bytes as base64url: 43 characters, urlsafe, no padding — the
/// same shape the OAuth verifier is minted in (`bingo-auth-oauth::pkce`).
///
/// The source is `getrandom` rather than that module's `aws-lc-rs`: this
/// crate spells a named pipe, and its Windows cross-check has to be runnable
/// without a Windows C toolchain (AGENTS.md, "Every platform the release
/// ships"). Random bytes need no C.
const BYTES: usize = 32;

/// One conversation's secret.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

/// A token in a log is a token in someone else's hands.
impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(…)")
    }
}

impl Token {
    /// A fresh secret from the system random source. A source that will not
    /// answer is a reason to stop, not a reason to guess.
    pub fn mint() -> Result<Self, AcpError> {
        let mut bytes = [0u8; BYTES];
        getrandom::fill(&mut bytes)
            .map_err(|_| AcpError::Spawn("the system random source refused".into()))?;
        Ok(Self(URL_SAFE_NO_PAD.encode(bytes)))
    }

    /// A token that already exists: the one the far side offered, or a test's.
    pub fn from_raw(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// What goes in the server row's environment.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether what was offered is this token.
    ///
    /// Constant in the length of the token: a comparison that stopped at the
    /// first differing byte would tell an offerer how much of its guess was
    /// right, one byte at a time. The length itself is public — every token
    /// this bridge mints is the same 43 characters — so only the bytes are
    /// hidden.
    pub fn matches(&self, offered: &str) -> bool {
        let ours = self.0.as_bytes();
        let theirs = offered.as_bytes();
        if ours.len() != theirs.len() {
            return false;
        }
        let mut differing = 0u8;
        for (ours, theirs) in ours.iter().zip(theirs) {
            differing |= ours ^ theirs;
        }
        std::hint::black_box(differing) == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minted() -> Token {
        Token::mint().expect("the system random source answers")
    }

    #[test]
    fn a_token_is_43_url_safe_characters_and_never_repeats() {
        let first = minted();
        assert_eq!(first.as_str().len(), 43);
        assert_ne!(first.as_str(), minted().as_str());
        assert!(
            first
                .as_str()
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "{}",
            first.as_str()
        );
    }

    #[test]
    fn a_token_matches_itself_and_nothing_else() {
        let token = minted();
        assert!(token.matches(token.as_str()));
        assert!(!token.matches(minted().as_str()));
        assert!(!token.matches(""));
        // A prefix is not a token: a comparison that stopped at the shorter
        // of the two would admit every truncation.
        assert!(!token.matches(&token.as_str()[..42]));
        assert!(!token.matches(&format!("{}x", token.as_str())));
    }

    /// The one thing a token must never do is turn up in a log line.
    #[test]
    fn a_token_never_prints_itself() {
        let token = minted();
        let printed = format!("{token:?}");
        assert_eq!(printed, "Token(…)");
        assert!(!printed.contains(token.as_str()));
    }
}
