//! What an endpoint says it serves. The embedded snapshot knows what a model
//! can do; only the endpoint knows which ids exist behind *this* base url, and
//! a named instance (ADR-0017) serves whatever its proxy fronts rather than
//! its wire shape's catalogue. So each provider's own answer to
//! `Provider::models()` is kept here, across processes, in the `Learned`
//! manner (`models/learned.rs`): one file under `data_dir`, where missing or
//! unreadable is an empty start and never an error.
//!
//! One file and not one per provider: a provider id is a name out of the
//! settings, and a path built from one is a path a settings file can steer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use bingo_sdk::ModelInfo;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

/// A list older than this is asked for again: an endpoint's menu changes with
/// a release, not with the hour.
pub const STALE_AFTER: SignedDuration = SignedDuration::from_hours(24);

/// One provider's list, as its endpoint last answered it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Served {
    pub fetched: Timestamp,
    pub models: Vec<ModelInfo>,
}

/// Who says a provider offers an id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The endpoint listed it.
    Endpoint,
    /// The embedded snapshot files it under this provider's family.
    Catalogue,
    /// Neither: the settings named it, and it is offered on their word.
    Configured,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Endpoint => "endpoint",
            Source::Catalogue => "catalogue",
            Source::Configured => "configured",
        }
    }
}

/// One id a provider offers, and who says it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub id: String,
    pub source: Source,
}

/// The ids one provider offers: what its endpoint answered if it has ever
/// answered, else what the snapshot files under its family — with the
/// configured model first either way, and only once.
pub fn merge(served: Option<&Served>, catalogued: &[&str], configured: Option<&str>) -> Vec<Offer> {
    let offered: Vec<&str> = match served {
        Some(served) => served.models.iter().map(|m| m.id.as_str()).collect(),
        None => catalogued.to_vec(),
    };
    configured
        .into_iter()
        .chain(offered.into_iter().filter(|id| Some(*id) != configured))
        .map(|id| Offer {
            id: id.to_string(),
            source: source(served, catalogued, id),
        })
        .collect()
}

/// The endpoint outranks the snapshot, and the snapshot outranks the word of
/// a settings file — an id nobody else knows is offered as configured.
fn source(served: Option<&Served>, catalogued: &[&str], id: &str) -> Source {
    if served.is_some_and(|s| s.models.iter().any(|m| m.id == id)) {
        Source::Endpoint
    } else if catalogued.contains(&id) {
        Source::Catalogue
    } else {
        Source::Configured
    }
}

/// Whether a list fetched then should be fetched again now. A stamp that is
/// not within [`STALE_AFTER`] of `now` in *either* direction is stale: a
/// clock that went backwards must not freeze a list for a day.
pub fn stale(fetched: Timestamp, now: Timestamp) -> bool {
    now.duration_since(fetched).abs() >= STALE_AFTER
}

/// Every provider's list, keyed by provider id.
#[derive(Debug, Default)]
pub struct ServedModels {
    lists: Mutex<BTreeMap<String, Served>>,
    /// Where the lists outlive the process; `None` keeps them in memory, as
    /// tests do.
    path: Option<PathBuf>,
}

impl ServedModels {
    /// The lists earlier processes wrote, from `path`.
    pub fn load(path: PathBuf) -> Self {
        let lists = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            lists: Mutex::new(lists),
            path: Some(path),
        }
    }

    pub fn get(&self, provider: &str) -> Option<Served> {
        self.lock().get(provider).cloned()
    }

    /// Whether this provider should be asked: never asked, or asked a day ago.
    pub fn stale(&self, provider: &str, now: Timestamp) -> bool {
        self.get(provider)
            .is_none_or(|served| stale(served.fetched, now))
    }

    /// Keep what an endpoint just answered; returns whether the ids changed,
    /// which is the only part worth telling anybody about. An empty answer is
    /// no answer — an endpoint that lists nothing has told us nothing, and
    /// the snapshot's list stays.
    pub fn record(&self, provider: &str, models: Vec<ModelInfo>, now: Timestamp) -> bool {
        if models.is_empty() {
            return false;
        }
        let mut lists = self.lock();
        let changed = lists.get(provider).is_none_or(|old| old.models != models);
        lists.insert(
            provider.to_string(),
            Served {
                fetched: now,
                models,
            },
        );
        self.save(&lists);
        changed
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Served>> {
        self.lists.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn save(&self, lists: &BTreeMap<String, Served>) {
        let Some(path) = &self.path else { return };
        if let Err(e) = write_atomically(path, lists) {
            tracing::warn!(path = %path.display(), error = %e, "served models not saved");
        }
    }
}

fn write_atomically(path: &Path, lists: &BTreeMap<String, Served>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(lists)?)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str) -> ModelInfo {
        ModelInfo {
            id: id.to_string(),
            display: None,
        }
    }

    fn served(ids: &[&str]) -> Served {
        Served {
            fetched: Timestamp::UNIX_EPOCH,
            models: ids.iter().map(|id| info(id)).collect(),
        }
    }

    fn ids(offers: &[Offer]) -> Vec<(&str, &str)> {
        offers
            .iter()
            .map(|o| (o.id.as_str(), o.source.as_str()))
            .collect()
    }

    /// The record a later process reads: these keys, this shape. A rename here
    /// throws every cached list away, so it is pinned rather than described.
    #[test]
    fn the_file_holds_one_stamped_list_per_provider() {
        let text = r#"{
          "work": {
            "fetched": "2026-09-01T10:00:00Z",
            "models": [{"id": "deepseek-v4-pro"}, {"id": "glm-5", "display": "GLM 5"}]
          }
        }"#;
        let lists: BTreeMap<String, Served> = serde_json::from_str(text).expect("the shape");
        let work = lists.get("work").expect("one provider");
        assert_eq!(work.fetched.to_string(), "2026-09-01T10:00:00Z");
        assert_eq!(
            work.models,
            vec![
                info("deepseek-v4-pro"),
                ModelInfo {
                    id: "glm-5".into(),
                    display: Some("GLM 5".into()),
                },
            ]
        );
        let written = serde_json::to_string(&lists).expect("written");
        assert_eq!(
            serde_json::from_str::<BTreeMap<String, Served>>(&written).expect("read back"),
            lists
        );
    }

    #[test]
    fn a_list_outlives_the_process_through_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("served-models.json");
        let first = ServedModels::load(path.clone());
        assert!(first.record("work", vec![info("a")], Timestamp::UNIX_EPOCH));
        let second = ServedModels::load(path);
        assert_eq!(second.get("work"), Some(served(&["a"])));
        assert_eq!(second.get("other"), None);
        assert_eq!(
            ServedModels::load(dir.path().join("absent.json")).get("work"),
            None
        );
    }

    #[test]
    fn only_a_changed_list_is_worth_announcing_and_an_empty_one_is_no_answer() {
        let lists = ServedModels::default();
        let now = Timestamp::UNIX_EPOCH;
        assert!(lists.record("work", vec![info("a")], now));
        assert!(!lists.record("work", vec![info("a")], now), "the same list");
        assert!(lists.record("work", vec![info("a"), info("b")], now));
        assert!(!lists.record("work", Vec::new(), now), "nothing is not news");
        assert_eq!(
            lists.get("work").map(|s| s.models.len()),
            Some(2),
            "and it did not overwrite what the endpoint had said"
        );
    }

    #[test]
    fn a_list_is_stale_a_day_later_and_a_provider_never_asked_is_stale_too() {
        let fetched = Timestamp::UNIX_EPOCH;
        assert!(!stale(fetched, fetched + SignedDuration::from_hours(23)));
        assert!(stale(fetched, fetched + SignedDuration::from_hours(25)));
        assert!(
            stale(fetched, fetched - SignedDuration::from_hours(25)),
            "a clock that went backwards does not freeze a list"
        );
        let lists = ServedModels::default();
        assert!(lists.stale("work", fetched), "never asked");
        lists.record("work", vec![info("a")], fetched);
        assert!(!lists.stale("work", fetched));
        assert!(lists.stale("work", fetched + SignedDuration::from_hours(25)));
    }

    #[test]
    fn the_endpoint_s_list_replaces_the_snapshot_s() {
        let catalogued = ["gpt-5", "gpt-5-mini"];
        assert_eq!(
            ids(&merge(None, &catalogued, None)),
            [("gpt-5", "catalogue"), ("gpt-5-mini", "catalogue")],
            "with no answer from the endpoint the snapshot stands"
        );
        let served = served(&["deepseek-v4-pro", "gpt-5"]);
        assert_eq!(
            ids(&merge(Some(&served), &catalogued, None)),
            [("deepseek-v4-pro", "endpoint"), ("gpt-5", "endpoint")],
            "an endpoint that has answered says which ids exist here"
        );
    }

    #[test]
    fn the_configured_id_is_first_once_and_named_by_whoever_knows_it() {
        let catalogued = ["gpt-5"];
        let served = served(&["gpt-5", "glm-5"]);
        assert_eq!(
            ids(&merge(Some(&served), &catalogued, Some("glm-5"))),
            [("glm-5", "endpoint"), ("gpt-5", "endpoint")],
            "the configured id leads and is not listed twice"
        );
        assert_eq!(
            ids(&merge(None, &catalogued, Some("gpt-5"))),
            [("gpt-5", "catalogue")]
        );
        assert_eq!(
            ids(&merge(Some(&served), &catalogued, Some("house-private-1"))),
            [
                ("house-private-1", "configured"),
                ("gpt-5", "endpoint"),
                ("glm-5", "endpoint"),
            ],
            "an id neither knows is offered on the settings' word"
        );
    }
}
