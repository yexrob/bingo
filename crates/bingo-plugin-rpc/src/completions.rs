//! What `command/complete` has answered so far, for one plugin.
//!
//! `Command::complete` is synchronous and a round trip to another process is
//! not, so the ask happens on its own task and the answer waits here for the
//! next keystroke — the same discipline a contribution source follows: answer
//! from a cache, do the I/O elsewhere (ADR-0009 §1). The cache belongs to the
//! plugin rather than to a command object, because a command object is built
//! afresh on every source read and a cache that died with it would answer
//! nothing, ever.

use std::collections::HashMap;
use std::sync::Mutex;

use bingo_sdk::Completion;

/// Partials remembered at once, across every command of one plugin. A person
/// types their way through a handful; past the cap the map starts again rather
/// than growing for a session.
const CAP: usize = 64;

/// What one command was asked, and what it answered.
type Asked = (String, String);

#[derive(Debug, Default)]
pub struct Completions {
    known: Mutex<HashMap<Asked, Vec<Completion>>>,
}

impl Completions {
    /// What is known for this command and partial. `None` means nobody has
    /// asked yet — and claims the ask, so two keystrokes send one request.
    pub fn claim(&self, command: &str, partial: &str) -> Option<Vec<Completion>> {
        let mut known = self.lock();
        if known.len() >= CAP {
            known.clear();
        }
        let asked = (command.to_string(), partial.to_string());
        match known.get(&asked) {
            Some(found) => Some(found.clone()),
            None => {
                known.insert(asked, Vec::new());
                None
            }
        }
    }

    pub fn fill(&self, command: &str, partial: &str, completions: Vec<Completion>) {
        self.lock()
            .insert((command.to_string(), partial.to_string()), completions);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Asked, Vec<Completion>>> {
        self.known.lock().unwrap_or_else(|held| held.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completion(value: &str) -> Completion {
        Completion {
            value: value.to_string(),
            label: None,
        }
    }

    #[test]
    fn the_first_ask_is_claimed_and_answers_nothing() {
        let completions = Completions::default();
        assert_eq!(completions.claim("wordcount", "no"), None);
        assert_eq!(
            completions.claim("wordcount", "no"),
            Some(Vec::new()),
            "the second caller waits for the first ask rather than sending its own"
        );
    }

    #[test]
    fn a_filled_partial_answers_what_the_plugin_said() {
        let completions = Completions::default();
        completions.claim("wordcount", "no");
        completions.fill("wordcount", "no", vec![completion("notes.txt")]);
        assert_eq!(
            completions.claim("wordcount", "no"),
            Some(vec![completion("notes.txt")])
        );
    }

    #[test]
    fn two_commands_asked_the_same_thing_keep_their_own_answers() {
        let completions = Completions::default();
        completions.fill("count", "no", vec![completion("notes.txt")]);
        completions.fill("clear", "no", vec![completion("nothing")]);
        assert_eq!(
            completions.claim("count", "no"),
            Some(vec![completion("notes.txt")])
        );
    }

    #[test]
    fn a_long_session_of_typing_starts_again_rather_than_growing() {
        let completions = Completions::default();
        for n in 0..CAP {
            completions.claim("count", &format!("partial {n}"));
        }
        assert_eq!(completions.lock().len(), CAP);
        completions.claim("count", "one more");
        assert_eq!(completions.lock().len(), 1);
    }
}
