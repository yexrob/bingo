//! What each of an adapter's models last said its window is.
//!
//! An ACP session's context is the agent's, and the only true word on how big
//! it is is the `size` the adapter puts in every `usage_update` (ADR-0055 §4).
//! But `endpoint()` is asked at the start of a session, before any turn has
//! heard one — so what a model said last time is kept here and named then. A
//! model nobody has spoken to yet names nothing, and the catalogue's guess
//! stands until the first reading of the turn corrects it.

use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Debug, Default)]
pub struct Windows(Mutex<BTreeMap<String, u64>>);

impl Windows {
    /// The window this model last named, if it ever has.
    pub fn of(&self, model: &str) -> Option<u64> {
        self.0.lock().ok()?.get(model).copied()
    }

    /// What the adapter just said, for whoever asks next.
    pub fn heard(&self, model: &str, window: u64) {
        if let Ok(mut known) = self.0.lock() {
            known.insert(model.to_string(), window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_names_the_window_it_last_reported_and_nothing_before_that() {
        let windows = Windows::default();
        assert_eq!(windows.of("agent"), None);
        windows.heard("agent", 200_000);
        windows.heard("other", 400_000);
        assert_eq!(windows.of("agent"), Some(200_000));
        windows.heard("agent", 1_000_000);
        assert_eq!(windows.of("agent"), Some(1_000_000), "the last word wins");
        assert_eq!(windows.of("other"), Some(400_000), "and is its own model's");
    }
}
