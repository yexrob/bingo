//! Pages already fetched, keyed by canonical URL. A model that reads a page,
//! acts, and reads it again in the same minute should cost the network once.
//!
//! It lives in the tool, not in a static: two hosts in one process are two
//! caches, and a test starts from an empty one.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How long a fetched page stays fresh.
const TTL: Duration = Duration::from_secs(15 * 60);

/// What every entry together may cost.
const MAX_BYTES: usize = 50 * 1024 * 1024;

#[derive(Debug)]
struct Entry {
    text: String,
    stored: Instant,
}

#[derive(Debug, Default)]
pub(crate) struct Cache {
    entries: Mutex<HashMap<String, Entry>>,
}

impl Cache {
    pub(crate) fn get(&self, key: &str) -> Option<String> {
        self.get_at(key, Instant::now())
    }

    pub(crate) fn put(&self, key: &str, text: &str) {
        self.put_at(key, text, Instant::now());
    }

    fn get_at(&self, key: &str, now: Instant) -> Option<String> {
        self.entries()
            .get(key)
            .filter(|entry| fresh(entry, now))
            .map(|entry| entry.text.clone())
    }

    fn put_at(&self, key: &str, text: &str, now: Instant) {
        let mut entries = self.entries();
        entries.insert(
            key.to_string(),
            Entry {
                text: text.to_string(),
                stored: now,
            },
        );
        entries.retain(|_, entry| fresh(entry, now));
        evict_oldest_until_within_budget(&mut entries);
    }

    /// A poisoned lock means a panic while a page was being stored. The cache
    /// holds nothing another caller needs to be protected from.
    fn entries(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn fresh(entry: &Entry, now: Instant) -> bool {
    now.duration_since(entry.stored) < TTL
}

fn evict_oldest_until_within_budget(entries: &mut HashMap<String, Entry>) {
    while total_bytes(entries) > MAX_BYTES {
        let Some(oldest) = oldest_key(entries) else {
            break;
        };
        entries.remove(&oldest);
    }
}

fn total_bytes(entries: &HashMap<String, Entry>) -> usize {
    entries.values().map(|entry| entry.text.len()).sum()
}

fn oldest_key(entries: &HashMap<String, Entry>) -> Option<String> {
    entries
        .iter()
        .min_by_key(|(_, entry)| entry.stored)
        .map(|(key, _)| key.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_that_was_never_stored_is_a_miss() {
        assert_eq!(Cache::default().get("https://example.com/"), None);
    }

    #[test]
    fn a_page_stored_within_the_ttl_comes_back() {
        let cache = Cache::default();
        cache.put("https://example.com/", "# Title");
        assert_eq!(
            cache.get("https://example.com/"),
            Some("# Title".to_string())
        );
    }

    #[test]
    fn a_page_older_than_the_ttl_is_a_miss() {
        let cache = Cache::default();
        let stored = Instant::now();
        cache.put_at("https://example.com/", "# Title", stored);
        assert!(cache.get_at("https://example.com/", stored + TTL).is_none());
        assert!(
            cache
                .get_at(
                    "https://example.com/",
                    stored + TTL - Duration::from_secs(1)
                )
                .is_some()
        );
    }

    #[test]
    fn a_second_store_replaces_the_first() {
        let cache = Cache::default();
        cache.put("https://example.com/", "old");
        cache.put("https://example.com/", "new");
        assert_eq!(cache.get("https://example.com/"), Some("new".to_string()));
    }

    #[test]
    fn the_oldest_page_goes_when_the_total_would_pass_the_budget() {
        let cache = Cache::default();
        let big = "a".repeat(30 * 1024 * 1024);
        let first = Instant::now();
        cache.put_at("https://example.com/one", &big, first);
        cache.put_at(
            "https://example.com/two",
            &big,
            first + Duration::from_secs(1),
        );
        assert_eq!(cache.get("https://example.com/one"), None);
        assert!(cache.get("https://example.com/two").is_some());
    }
}
