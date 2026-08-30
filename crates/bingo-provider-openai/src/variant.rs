//! Which Responses endpoint a provider instance talks to.
//!
//! The wire format is one format; a variant is the handful of places the
//! ChatGPT subscription endpoint departs from the public API. Keeping the
//! departures in one enum is what lets the encoder stay a pure function of
//! `(request, variant)` and the isolation test read as a table.

/// The header the subscription endpoint identifies the client by (old
/// `providers/openai.rs:239-248`).
pub const ORIGINATOR: &str = "bingo";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Variant {
    /// `https://api.openai.com/v1/responses`, an API key.
    #[default]
    Default,
    /// The ChatGPT subscription endpoint, a bearer from an OAuth flow
    /// (ADR-0012 §6).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_variant_names_its_own_path_and_budget_rule() {
        assert_eq!(Variant::Default.path(), "/v1/responses");
        assert_eq!(Variant::Codex.path(), "/codex/responses");
        assert!(Variant::Default.sends_max_output_tokens());
        assert!(!Variant::Codex.sends_max_output_tokens());
        assert_eq!(Variant::Default.provider_id(), "openai");
        assert_eq!(Variant::Codex.provider_id(), "codex");
    }
}
