//! One experience: the playbook a person or the model wrote down, the
//! outcomes it has been given since, and the id that is its file name.
//!
//! `helpful` and `harmful` are counted from `outcomes` whenever they are
//! wanted and never written down (ADR-0014 §2): a counter beside the history
//! it derives from is a second representation waiting to disagree.

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    #[default]
    Active,
    /// Kept for its history, never recalled. The old design's third state,
    /// `degraded`, had no behaviour of its own.
    Retired,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Active => "active",
            Status::Retired => "retired",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    Helpful,
    Harmful,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Helpful => "helpful",
            Outcome::Harmful => "harmful",
        }
    }
}

/// What happened when the playbook was followed. Evidence is required: an
/// outcome nobody can check is a self-confirmation.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Record {
    pub outcome: Outcome,
    pub at: Timestamp,
    pub evidence: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The minted slug that is also the file's stem; stored nowhere inside it.
    pub id: String,
    pub status: Status,
    /// What brings this playbook to mind.
    pub trigger: Vec<String>,
    pub summary: String,
    pub steps: Vec<String>,
    /// How to tell it worked.
    pub verify: Option<String>,
    pub created: Timestamp,
    pub outcomes: Vec<Record>,
    /// Everything after the frontmatter, trimmed.
    pub notes: String,
}

impl Entry {
    pub fn helpful(&self) -> usize {
        self.count(Outcome::Helpful)
    }

    pub fn harmful(&self) -> usize {
        self.count(Outcome::Harmful)
    }

    fn count(&self, want: Outcome) -> usize {
        self.outcomes
            .iter()
            .filter(|record| record.outcome == want)
            .count()
    }

    /// Whether two entries say the same thing, which is what `ExperienceCommit`
    /// without an id dedups on: the same trigger, summary and steps are the
    /// same playbook, whatever its notes have grown into.
    pub fn same_playbook(&self, other: &Entry) -> bool {
        self.trigger == other.trigger && self.summary == other.summary && self.steps == other.steps
    }

    /// How long this has been on the shelf, as a person reads it.
    pub fn age(&self, now: Timestamp) -> String {
        let hours = now.duration_since(self.created).as_hours().max(0);
        if hours < 1 {
            "just now".into()
        } else if hours < 24 {
            format!("{hours}h")
        } else {
            format!("{}d", hours / 24)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn record(outcome: Outcome) -> Record {
        Record {
            outcome,
            at: Timestamp::UNIX_EPOCH,
            evidence: "the suite went green".into(),
        }
    }

    pub(crate) fn entry() -> Entry {
        Entry {
            id: "abcd1234".into(),
            status: Status::Active,
            trigger: vec!["the build breaks".into()],
            summary: "clear the target directory".into(),
            steps: vec!["cargo clean".into(), "cargo build".into()],
            verify: Some("the build is green".into()),
            created: Timestamp::UNIX_EPOCH,
            outcomes: Vec::new(),
            notes: String::new(),
        }
    }

    #[test]
    fn the_counts_are_read_off_the_history() {
        let mut entry = entry();
        assert_eq!((entry.helpful(), entry.harmful()), (0, 0));
        entry.outcomes = vec![
            record(Outcome::Helpful),
            record(Outcome::Harmful),
            record(Outcome::Helpful),
        ];
        assert_eq!((entry.helpful(), entry.harmful()), (2, 1));
    }

    #[test]
    fn the_same_playbook_is_the_trigger_the_summary_and_the_steps() {
        let a = entry();
        let mut b = entry();
        b.id = "zzzz9999".into();
        b.notes = "and mind the lockfile".into();
        b.status = Status::Retired;
        assert!(a.same_playbook(&b), "notes and status are not the playbook");
        b.steps.push("cargo test".into());
        assert!(!a.same_playbook(&b));
    }

    #[test]
    fn an_age_is_hours_then_days() {
        let entry = entry();
        let created = entry.created;
        assert_eq!(entry.age(created), "just now");
        assert_eq!(
            entry.age(created + jiff::SignedDuration::from_hours(5)),
            "5h"
        );
        assert_eq!(
            entry.age(created + jiff::SignedDuration::from_hours(50)),
            "2d"
        );
    }
}
