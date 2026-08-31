//! What an entry looks like to the ranker. The weights are the whole of the
//! judgement (ADR-0014 §5): a trigger word is what the entry is *for*, the
//! summary names the pattern, steps and notes only fill in vocabulary.

use crate::bm25::{Bm25, Document};
use crate::entry::Entry;

const TRIGGER: f64 = 3.0;
const SUMMARY: f64 = 2.0;
const STEPS: f64 = 1.0;
const NOTES: f64 = 1.0;

fn document(entry: &Entry) -> Document {
    Document::default()
        .field(&entry.trigger.join(" "), TRIGGER)
        .field(&entry.summary, SUMMARY)
        .field(&entry.steps.join("\n"), STEPS)
        .field(&entry.notes, NOTES)
}

/// The entries `query` matches, best first. `floor` is on for recall, which
/// nobody asked for, and off for a search someone did.
pub fn best<'a>(entries: &[&'a Entry], query: &str, floor: bool) -> Vec<&'a Entry> {
    let corpus = Bm25::new(entries.iter().map(|entry| document(entry)).collect());
    corpus
        .rank(query, floor)
        .into_iter()
        .filter_map(|(index, _)| entries.get(index).copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::tests::entry;

    fn about(id: &str, trigger: &str, summary: &str) -> Entry {
        Entry {
            id: id.into(),
            trigger: vec![trigger.into()],
            summary: summary.into(),
            steps: vec!["do the thing".into()],
            ..entry()
        }
    }

    #[test]
    fn the_trigger_outweighs_the_notes() {
        let triggered = about("aaaa1111", "the cache is stale", "clear it");
        let mentioned = Entry {
            notes: "the cache is stale sometimes, but that is not what this is for".into(),
            ..about("bbbb2222", "the disk is full", "clear the target dir")
        };
        let entries = [&triggered, &mentioned];
        let best = best(&entries, "the cache is stale", false);
        assert_eq!(
            best.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["aaaa1111", "bbbb2222"]
        );
    }

    #[test]
    fn nothing_matches_nothing() {
        let entry = about("aaaa1111", "the cache is stale", "clear it");
        assert!(best(&[&entry], "an unrelated question", false).is_empty());
        assert!(best(&[], "anything", false).is_empty());
    }
}
