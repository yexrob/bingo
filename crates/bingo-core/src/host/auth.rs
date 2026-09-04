//! Which provider may be called, and what to say when none may.
//!
//! A provider that cannot authenticate is refused before any turn is spent on
//! it, and — the other side of the same fact — is never picked as the default
//! for a person who named none. Registration order decides among the ones
//! that *can* answer, never between one that can and one that cannot.

use std::sync::Arc;

use bingo_sdk::{AuthStatus, ErrorCode, KernelError, Provider};

/// Whether a provider's credentials are in place. `NotApplicable` is a
/// provider that needs none — a local agent, a proxy — and can answer.
pub(super) fn signed_in(provider: &dyn Provider) -> bool {
    matches!(
        provider.auth(),
        AuthStatus::Ready | AuthStatus::NotApplicable
    )
}

/// A provider that cannot authenticate is refused before any turn is spent
/// on it. This is for a provider somebody *named*: the reason is about that
/// one, so it says which and how to fix it.
pub(super) fn check(provider: &dyn Provider) -> Result<(), KernelError> {
    let refuse = |message: String| Err(KernelError::new(ErrorCode::AuthRequired, message));
    match provider.auth() {
        AuthStatus::Ready | AuthStatus::NotApplicable => Ok(()),
        AuthStatus::Missing { hint } => refuse(format!(
            "The {} provider has no credentials. {hint}",
            provider.id()
        )),
        AuthStatus::Expired { hint } => refuse(format!(
            "The {} provider's credentials have expired. {hint}",
            provider.id()
        )),
    }
}

/// Nobody named a provider and none of them can answer. The first registered
/// one is not the subject here — every one of them is — so the refusal lists
/// them all with what each wants, and a person picks.
pub(super) fn nobody_signed_in(providers: &[Arc<dyn Provider>]) -> KernelError {
    let mut lines =
        vec!["No provider is signed in. Sign in to one, or name it in the settings:".to_string()];
    lines.extend(providers.iter().map(|p| format!("  {}", wants(p.as_ref()))));
    KernelError::new(ErrorCode::AuthRequired, lines.join("\n"))
}

/// One provider's line in that list: its id and, when it said one, the hint
/// it gives for getting a credential in place.
fn wants(provider: &dyn Provider) -> String {
    let id = provider.id();
    match provider.auth() {
        AuthStatus::Missing { hint } => format!("{id} — {hint}"),
        AuthStatus::Expired { hint } => format!("{id} — credentials expired. {hint}"),
        AuthStatus::Ready | AuthStatus::NotApplicable => id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ScriptedProvider;

    #[test]
    fn the_refusal_names_every_provider_and_what_it_wants() {
        let a = ScriptedProvider::named("anthropic", vec![]).missing("Run `bingo login anthropic`.")
            as Arc<dyn Provider>;
        let b = ScriptedProvider::named("openai", vec![]).expired("Sign in again.")
            as Arc<dyn Provider>;
        let said = nobody_signed_in(&[a, b]).message;
        assert!(
            said.contains("anthropic — Run `bingo login anthropic`."),
            "{said}"
        );
        assert!(
            said.contains("openai — credentials expired. Sign in again."),
            "{said}"
        );
    }
}
