//! PKCE (RFC 7636) and the `state` nonce.
//!
//! The verifier never leaves the process and the challenge is all the issuer
//! ever sees, so the only thing that matters here is that the bytes come from
//! the system random source — a predictable verifier would let a local
//! attacker who saw the authorize URL redeem the code.

use aws_lc_rs::digest::{SHA256, digest};
use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::error::AuthError;

/// 32 random bytes as base64url: 43 characters, inside RFC 7636's 43..128.
pub fn verifier() -> Result<String, AuthError> {
    random::<32>()
}

/// The S256 transformation the authorize URL carries. Pure: the same verifier
/// always yields the same challenge, which is what the RFC's test vector pins.
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest(&SHA256, verifier.as_bytes()))
}

/// The CSRF nonce a callback is checked against, one per attempt.
pub fn state() -> Result<String, AuthError> {
    random::<16>()
}

/// `Invalid` rather than a weaker fallback: a random source that will not
/// answer is a reason to stop, not a reason to guess.
fn random<const N: usize>() -> Result<String, AuthError> {
    let mut bytes = [0u8; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| AuthError::Invalid("the system random source refused".into()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 appendix B.
    #[test]
    fn the_challenge_matches_the_rfc_test_vector() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_verifier_is_43_url_safe_characters_and_never_repeats() {
        let first = verifier().expect("a verifier");
        let second = verifier().expect("a verifier");
        assert_eq!(first.len(), 43);
        assert_ne!(first, second);
        assert!(
            first
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "{first}"
        );
    }

    #[test]
    fn a_state_nonce_is_22_characters_and_never_repeats() {
        let first = state().expect("a state");
        assert_eq!(first.len(), 22);
        assert_ne!(first, state().expect("a state"));
    }
}
