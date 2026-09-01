//! `/model [<provider>/]<model>`: the next turn runs on it.

use std::sync::Weak;

use async_trait::async_trait;
use bingo_sdk::*;

use crate::host::{Change, Host};

pub(super) struct ModelCommand {
    pub(super) host: Weak<Host>,
}

#[async_trait]
impl Command for ModelCommand {
    fn spec(&self) -> CommandSpec {
        super::spec(
            "model",
            "[<provider>/]<model>",
            ArgSpec::Catalog {
                source: "models".into(),
            },
            true,
        )
    }

    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let host = super::host(&self.host)?;
        if args.trim().is_empty() {
            let current = host.session_summary(&cx.session).await?;
            return Ok(CommandOutcome::View {
                view: View::Text {
                    text: format!(
                        "model: {}\nusage: /model [<provider>/]<model>",
                        named(&current)
                    ),
                },
            });
        }
        let known = host.providers().await;
        let (provider, model) = split(args.trim(), |id| known.iter().any(|p| p.id() == id));
        let summary = host
            .reconfigure(
                &cx.session,
                Change::Model {
                    provider: provider.map(str::to_string),
                    model: model.to_string(),
                },
            )
            .await?;
        Ok(CommandOutcome::Applied {
            message: Some(format!("model: {}", named(&summary))),
        })
    }
}

/// `provider/model` when the first segment names a provider; else the whole
/// text is the model and the provider stays.
fn split(text: &str, is_provider: impl Fn(&str) -> bool) -> (Option<&str>, &str) {
    match text.split_once('/') {
        Some((provider, model)) if is_provider(provider) && !model.is_empty() => {
            (Some(provider), model)
        }
        _ => (None, text),
    }
}

fn named(summary: &SessionSummary) -> String {
    format!(
        "{}/{}",
        summary.provider.as_deref().unwrap_or("?"),
        summary.model.as_deref().unwrap_or("?")
    )
}

#[cfg(test)]
mod tests {
    use super::split;

    #[test]
    fn a_leading_provider_is_taken_only_when_it_is_one() {
        let known = |id: &str| id == "openai";
        assert_eq!(split("openai/gpt-5", known), (Some("openai"), "gpt-5"));
        assert_eq!(
            split("meta/llama-3", known),
            (None, "meta/llama-3"),
            "an unknown first segment is part of the model id"
        );
        assert_eq!(split("gpt-5", known), (None, "gpt-5"));
        assert_eq!(split("openai/", known), (None, "openai/"));
    }
}
