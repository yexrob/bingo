//! `/models [refresh]`: which ids each provider offers here, where that list
//! came from and how old it is — and, when a person asks, a fetch now.
//!
//! It reads the one catalogue every surface reads (ADR-0026 §4); the refresh
//! is the same one that runs in the background at start, waited for because
//! somebody asked for it.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use bingo_sdk::*;
use jiff::{SignedDuration, Timestamp};

use crate::host::{Host, Refreshed};

pub(super) struct ModelsCommand {
    pub(super) host: Weak<Host>,
}

#[async_trait]
impl Command for ModelsCommand {
    fn spec(&self) -> CommandSpec {
        super::spec(
            "models",
            "[refresh]",
            ArgSpec::Free {
                hint: "refresh".into(),
            },
            true,
        )
    }

    async fn run(&self, args: &str, _cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let host = super::host(&self.host)?;
        match args.trim() {
            "" => Ok(CommandOutcome::View {
                view: View::Text {
                    text: listing(&host).await?,
                },
            }),
            "refresh" => Ok(CommandOutcome::Applied {
                message: Some(counted(&host.refresh_models().await)),
            }),
            other => Err(KernelError::new(
                ErrorCode::InvalidInput,
                format!("unknown argument `{other}`; usage: /models [refresh]"),
            )),
        }
    }
}

async fn listing(host: &Arc<Host>) -> Result<String, KernelError> {
    let entries = host.catalog(CatalogKind::Models).await?.entries;
    let now = Timestamp::now();
    let asked = |provider: &str| {
        host.served_at(provider)
            .map(|at| ago(now.duration_since(at)))
    };
    Ok(blocks(&entries, &asked))
}

/// A provider per block, its ids under it, and one line saying where the list
/// came from. An empty catalogue is said in words, not as a blank answer.
fn blocks(entries: &[CatalogEntry], asked: &dyn Fn(&str) -> Option<String>) -> String {
    if entries.is_empty() {
        return "No provider offers a model in this build.".to_string();
    }
    let mut lines = Vec::new();
    for (provider, models) in grouped(entries) {
        lines.extend(block(provider, &models, asked(provider)));
    }
    lines.join("\n")
}

/// The entries by the provider that serves them, in the order the catalogue
/// lists them — which is the order the providers were registered in.
fn grouped(entries: &[CatalogEntry]) -> Vec<(&str, Vec<&CatalogEntry>)> {
    let mut groups: Vec<(&str, Vec<&CatalogEntry>)> = Vec::new();
    for entry in entries {
        let provider = meta(entry, "provider");
        match groups.last_mut() {
            Some((last, models)) if *last == provider => models.push(entry),
            _ => groups.push((provider, vec![entry])),
        }
    }
    groups
}

/// One provider's block. The header says where its list came from; a row that
/// came from somewhere else says so for itself.
fn block(provider: &str, models: &[&CatalogEntry], asked: Option<String>) -> Vec<String> {
    let from = match models.iter().any(|m| meta(m, "source") == ENDPOINT) {
        true => ENDPOINT,
        false => CATALOGUE,
    };
    let when = match (from, asked) {
        (ENDPOINT, Some(asked)) => format!(" · asked {asked}"),
        _ => String::new(),
    };
    let mut lines = vec![format!(
        "{provider}  {} models · from the {from}{when}",
        models.len()
    )];
    lines.extend(models.iter().map(|model| match meta(model, "source") {
        source if source == from => format!("  {}", model.label),
        source => format!("  {}  ({source})", model.label),
    }));
    lines
}

const ENDPOINT: &str = "endpoint";
const CATALOGUE: &str = "catalogue";

fn meta<'a>(entry: &'a CatalogEntry, key: &str) -> &'a str {
    entry.meta.get(key).and_then(|v| v.as_str()).unwrap_or("?")
}

/// What a refresh came to, one provider at a time.
fn counted(refreshed: &[Refreshed]) -> String {
    if refreshed.is_empty() {
        return "no provider could be asked: none of them is signed in".to_string();
    }
    let said: Vec<String> = refreshed
        .iter()
        .map(|one| match &one.answer {
            Ok(count) => format!("{} {count} models", one.provider),
            Err(why) => format!("{} could not be asked: {why}", one.provider),
        })
        .collect();
    said.join(" · ")
}

/// How long ago, coarsely: a person asking where a list came from wants this
/// morning told from last week, not a count of seconds.
fn ago(past: SignedDuration) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    match past.as_secs().max(0) {
        ..MINUTE => "just now".to_string(),
        seconds @ MINUTE..HOUR => format!("{}m ago", seconds / MINUTE),
        seconds @ HOUR..DAY => format!("{}h ago", seconds / HOUR),
        seconds => format!("{}d ago", seconds / DAY),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn entry(provider: &str, id: &str, source: &str) -> CatalogEntry {
        CatalogEntry {
            id: format!("{provider}/{id}"),
            label: id.to_string(),
            meta: json!({ "provider": provider, "source": source }),
        }
    }

    fn asked(_provider: &str) -> Option<String> {
        Some("2h ago".to_string())
    }

    #[test]
    fn a_provider_says_how_many_ids_it_offers_and_who_named_them() {
        let entries = vec![
            entry("work", "glm-5", "configured"),
            entry("work", "deepseek-v4-pro", "endpoint"),
            entry("anthropic", "claude-sonnet-4-5", "catalogue"),
        ];
        assert_eq!(
            blocks(&entries, &asked),
            "work  2 models · from the endpoint · asked 2h ago\n\
             \x20 glm-5  (configured)\n\
             \x20 deepseek-v4-pro\n\
             anthropic  1 models · from the catalogue\n\
             \x20 claude-sonnet-4-5"
        );
    }

    #[test]
    fn a_build_with_no_models_says_so() {
        assert!(blocks(&[], &asked).starts_with("No provider"));
    }

    #[test]
    fn a_refresh_answers_with_a_count_a_provider_and_a_failure_by_name() {
        assert_eq!(
            counted(&[
                Refreshed {
                    provider: "work".into(),
                    answer: Ok(12),
                },
                Refreshed {
                    provider: "anthropic".into(),
                    answer: Err("connection refused".into()),
                },
            ]),
            "work 12 models · anthropic could not be asked: connection refused"
        );
        assert!(counted(&[]).starts_with("no provider could be asked"));
    }

    #[test]
    fn an_age_is_coarse_and_never_negative() {
        assert_eq!(ago(SignedDuration::from_secs(-5)), "just now");
        assert_eq!(ago(SignedDuration::from_secs(59)), "just now");
        assert_eq!(ago(SignedDuration::from_mins(90)), "1h ago");
        assert_eq!(ago(SignedDuration::from_hours(50)), "2d ago");
    }
}
