//! Disk cache for fetched model lists (D65): `~/.local/share/bingo/models-cache.json`.
//!
//! Only providers that declared nothing reach this file — a declared list is
//! already the answer. The point is the second `/model` of the day: the list an
//! endpoint returns changes on the scale of weeks, so paying a round trip per
//! session buys nothing.
//!
//! Every failure here degrades to "no cache": a missing, unreadable or corrupt
//! file must never keep the menu (or startup) from working. That is why nothing
//! in this module returns an error.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Stable filename inside the bingo data dir.
const CACHE_FILE: &str = "models-cache.json";

/// How long a fetched list is trusted without re-asking.
pub const TTL_SECS: u64 = 24 * 60 * 60;

/// One cached list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedModels {
    pub models: Vec<String>,
    /// Unix seconds at fetch time.
    #[serde(rename = "fetchedAt")]
    pub fetched_at: u64,
}

impl CachedModels {
    /// Within the TTL. A clock that moved backwards reads as stale rather than
    /// eternally fresh.
    pub fn fresh_at(&self, now: u64) -> bool {
        now >= self.fetched_at && now - self.fetched_at < TTL_SECS
    }

    pub fn fresh(&self) -> bool {
        self.fresh_at(now())
    }
}

pub fn cache_path(home: &Path) -> PathBuf {
    crate::storage::data_dir(home).join(CACHE_FILE)
}

/// Read/write access to the cache file.
#[derive(Debug, Clone)]
pub struct ModelCache {
    path: PathBuf,
}

impl ModelCache {
    pub fn new(home: &Path) -> Self {
        Self {
            path: cache_path(home),
        }
    }

    /// The cached list for a provider *at this endpoint*. The key carries the
    /// base URL because repointing a provider at a different endpoint changes
    /// what it offers — the old list would be a confident lie.
    pub fn get(&self, provider: &str, base_url: &str) -> Option<CachedModels> {
        self.load().remove(&key(provider, base_url))
    }

    /// Store a freshly fetched list. Read-modify-write: other providers'
    /// entries survive.
    pub fn put(&self, provider: &str, base_url: &str, models: &[String]) {
        let mut store = self.load();
        store.insert(
            key(provider, base_url),
            CachedModels {
                models: models.to_vec(),
                fetched_at: now(),
            },
        );
        self.save(&store);
    }

    fn load(&self) -> HashMap<String, CachedModels> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    /// tmp + rename (the AuthStore discipline): a kill mid-write must leave the
    /// previous cache intact rather than a truncated file.
    fn save(&self, store: &HashMap<String, CachedModels>) {
        let Some(dir) = self.path.parent() else {
            return;
        };
        let Ok(encoded) = serde_json::to_string(store) else {
            return;
        };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let tmp = self.path.with_extension("json.tmp");
        let written = std::fs::File::create(&tmp).and_then(|mut file| {
            file.write_all(encoded.as_bytes())?;
            file.sync_all()
        });
        if written.is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// Cache key: provider name + endpoint, kept readable so the file can be
/// inspected by hand. `\u{1f}` separates because neither part may contain it.
fn key(provider: &str, base_url: &str) -> String {
    format!("{provider}\u{1f}{base_url}")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_home(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bingo-model-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn round_trips_per_provider_and_endpoint() {
        let home = tmp_home("round-trip");
        let cache = ModelCache::new(&home);
        assert_eq!(cache.get("proxy", "https://a.example"), None, "cold start");

        cache.put("proxy", "https://a.example", &["m1".into(), "m2".into()]);
        cache.put("other", "https://b.example", &["n1".into()]);
        let entry = cache.get("proxy", "https://a.example").unwrap();
        assert_eq!(entry.models, vec!["m1".to_string(), "m2".to_string()]);
        assert!(entry.fresh(), "a just-written entry is fresh");
        assert_eq!(
            cache.get("other", "https://b.example").unwrap().models,
            vec!["n1".to_string()],
            "a second write keeps the first entry"
        );

        // Repointing the provider invalidates: same name, different endpoint.
        assert_eq!(
            cache.get("proxy", "https://moved.example"),
            None,
            "the endpoint is part of the key"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn entries_expire_after_the_ttl() {
        let entry = CachedModels {
            models: vec!["m".into()],
            fetched_at: 1_000_000,
        };
        assert!(entry.fresh_at(1_000_000));
        assert!(entry.fresh_at(1_000_000 + TTL_SECS - 1));
        assert!(!entry.fresh_at(1_000_000 + TTL_SECS));
        assert!(
            !entry.fresh_at(999_999),
            "a backwards clock reads as stale, never as eternally fresh"
        );
    }

    /// Any unusable file degrades to "no cache" — never an error, never a
    /// refusal to start.
    #[test]
    fn a_corrupt_file_degrades_to_no_cache() {
        let home = tmp_home("corrupt");
        let path = cache_path(&home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not json at all").unwrap();
        let cache = ModelCache::new(&home);
        assert_eq!(cache.get("proxy", "https://a.example"), None);
        // And a write repairs it rather than compounding the damage.
        cache.put("proxy", "https://a.example", &["m1".into()]);
        assert_eq!(
            cache.get("proxy", "https://a.example").unwrap().models,
            vec!["m1".to_string()]
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
